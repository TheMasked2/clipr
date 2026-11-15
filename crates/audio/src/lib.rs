#![cfg(windows)]
//! Audio capture: the default output device (WASAPI loopback), the default microphone and individual apps
//! (process loopback), mixed into a stereo 48 kHz "Mix" track and optionally one track per source, each
//! encoded to AAC with the Media Foundation encoder.
//!
//! One thread per [`AudioCapture`] polls the devices every ~10 ms. Everything lands on a continuous
//! timeline that starts at the capture start and is stamped with `clipr_core::clock::now_100ns()`'s clock
//! (the same one video frames use): sources place their samples by the QPC time WASAPI reports, silence
//! fills gaps (loopback delivers nothing while nothing plays), and mixed audio is encoded up to ~30 ms
//! before now, so a clip saved at any time has audio up to near its end.
//!
//! The microphone goes through a chain (noise suppression, gate, compressor, push-to-talk, volume), see
//! [`chain`]. Devices that fail to open or disappear are retried, and the default device is followed when
//! it changes; the timeline carries on with silence meanwhile.

mod aac;
pub mod chain;
mod levels;
pub mod mix;
pub mod plan;
mod wasapi;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clipr_core::clock::now_100ns;
use clipr_core::config::AudioConfig;
use windows::Win32::Media::MediaFoundation::{
    IMFMediaType, MF_VERSION, MFSTARTUP_FULL, MFShutdown, MFStartup,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

use crate::aac::AacEncoder;
use crate::chain::{ChainSettings, MicChain, Ptt, ptt_vk};
use crate::levels::Levels;
use crate::mix::{HNS_PER_SEC, RATE, Timeline, time_at};
use crate::plan::{Plan, SourceKind};
use crate::wasapi::{Processing, PttKey, Source};

/// How far behind "now" mixed audio is cut, so slow sources have time to deliver.
const LATENCY_100NS: i64 = 30 * HNS_PER_SEC / 1000;

/// How often the devices are polled.
const POLL: Duration = Duration::from_millis(10);

/// One raw AAC frame of one track.
#[derive(Clone, Debug)]
pub struct AudioPacket {
    /// Index into [`AudioCapture::tracks`].
    pub track: usize,
    /// Presentation time on `clipr_core::clock::now_100ns()`'s clock.
    pub pts_100ns: i64,
    pub duration_100ns: i64,
    /// One AAC frame, without an ADTS header.
    pub data: Vec<u8>,
}

/// A running capture thread.
pub struct AudioCapture {
    thread: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    tracks: Vec<String>,
    /// Each track's encoder output type, serialised (`IMFMediaType` itself isn't `Send`).
    media_types: Vec<Vec<u8>>,
    levels: Arc<Levels>,
}

impl AudioCapture {
    /// Starts capture and encoding on its own thread. Errors if no source is enabled or the encoder
    /// can't start. `on_packet` is called on the capture thread.
    pub fn start(config: &AudioConfig, on_packet: impl FnMut(AudioPacket) + Send + 'static) -> Result<Self> {
        let plan = plan::plan(config.system, config.mic, &config.apps, config.separate_tracks);
        if plan.sources.is_empty() {
            bail!("no audio source is enabled");
        }
        for warning in &plan.warnings {
            tracing::warn!("{warning}");
        }
        let tracks: Vec<String> = plan.tracks.iter().map(|t| t.name.clone()).collect();
        let levels = Arc::new(Levels::new(plan.sources.iter().map(SourceKind::label).collect()));

        let config = config.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = {
            let (stop, levels) = (stop.clone(), levels.clone());
            std::thread::Builder::new()
                .name("clipr-audio".into())
                .spawn(move || run(&config, plan, &stop, levels, ready_tx, on_packet))
                .context("can't start the audio thread")?
        };
        let media_types = match ready_rx.recv() {
            Ok(Ok(blobs)) => blobs,
            Ok(Err(e)) => {
                let _ = thread.join();
                return Err(e);
            }
            Err(_) => {
                let _ = thread.join();
                bail!("the audio thread stopped unexpectedly");
            }
        };
        Ok(Self {
            thread: Some(thread),
            stop,
            tracks,
            media_types,
            levels,
        })
    }

    /// The track names; the index is [`AudioPacket::track`]. Track 0 is always "Mix".
    pub fn tracks(&self) -> Vec<String> {
        self.tracks.clone()
    }

    /// A track's AAC output media type (with `MF_MT_USER_DATA`), for an MP4 Sink Writer passthrough
    /// stream. Media Foundation must be started in this process.
    pub fn media_type(&self, track: usize) -> Result<IMFMediaType> {
        let blob = self
            .media_types
            .get(track)
            .with_context(|| format!("there is no audio track {track}"))?;
        aac::type_from_blob(blob)
    }

    /// Each source's peak level over the last ~100 ms in dBFS (-100 = silence), measured on the device's
    /// own signal before the microphone chain. Cheap enough to call ten times a second.
    pub fn levels(&self) -> Vec<(String, f32)> {
        self.levels.snapshot()
    }

    /// Stops the thread and waits for it (also on drop). The encoders are flushed first, so the last
    /// packets still reach `on_packet`.
    pub fn stop(self) {}
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("the audio thread panicked");
        }
    }
}

/// Balances `CoInitializeEx`.
struct ComRuntime;

impl ComRuntime {
    fn start() -> Result<Self> {
        // SAFETY: plain API call; balanced by CoUninitialize in Drop.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .context("can't start COM")?;
        Ok(Self)
    }
}

impl Drop for ComRuntime {
    fn drop(&mut self) {
        // SAFETY: balances the CoInitializeEx in `start`.
        unsafe { CoUninitialize() };
    }
}

/// Balances `MFStartup`.
struct MfRuntime;

impl MfRuntime {
    fn start() -> Result<Self> {
        // SAFETY: plain API call; balanced by MFShutdown in Drop.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.context("can't start Media Foundation")?;
        Ok(Self)
    }
}

impl Drop for MfRuntime {
    fn drop(&mut self) {
        // SAFETY: balances the MFStartup in `start`.
        let _ = unsafe { MFShutdown() };
    }
}

fn gain(percent: u32) -> f32 {
    percent.min(200) as f32 / 100.0
}

/// Builds each source's capture and processing from the settings.
fn build_sources(config: &AudioConfig, plan: &Plan, levels: &Arc<Levels>) -> (Vec<Source>, Vec<f32>, i64) {
    // Offsets beyond ±5 s are typos; clamping them keeps the mixer from padding minutes of silence.
    let ms = |ms: i32| i64::from(ms.clamp(-5000, 5000)) * HNS_PER_SEC / 1000;
    let mut sources = Vec::new();
    let mut gains = Vec::new();
    let mut earliest_shift = 0i64;
    for (lane, kind) in plan.sources.iter().enumerate() {
        let mut processing = Processing {
            shift_100ns: 0,
            chain: None,
            ptt: None,
        };
        let mut lane_gain = 1.0;
        match kind {
            SourceKind::System => {
                processing.shift_100ns = ms(config.system_offset_ms);
                lane_gain = gain(config.system_volume);
            }
            SourceKind::Mic => {
                let chain = MicChain::new(&ChainSettings {
                    suppress: config.noise_suppression,
                    gate_db: config.noise_gate.then_some(config.gate_threshold_db as f32),
                    compressor: config.compressor,
                    volume: gain(config.mic_volume),
                });
                // The chain's output lags its input; place it that much earlier to line up again.
                let delay = chain.delay_frames() as i64 * HNS_PER_SEC / RATE as i64;
                processing.shift_100ns = ms(config.mic_offset_ms) - delay;
                processing.chain = Some(chain);
                processing.ptt = match ptt_vk(&config.push_to_talk) {
                    Ok(vk) => vk.map(|vk| PttKey {
                        vk,
                        state: Ptt::new(config.ptt_release_ms),
                    }),
                    Err(e) => {
                        tracing::warn!("ignoring the push-to-talk key: {e:#}");
                        None
                    }
                };
            }
            SourceKind::App(_) => {}
        }
        earliest_shift = earliest_shift.min(processing.shift_100ns);
        gains.push(lane_gain);
        sources.push(Source::new(kind.clone(), lane, processing, levels.clone(), lane));
    }
    (sources, gains, -earliest_shift)
}

/// The capture thread: reports each track's encoder media type (or why it can't start) on `ready`, then
/// loops until `stop`.
fn run(
    config: &AudioConfig,
    plan: Plan,
    stop: &AtomicBool,
    levels: Arc<Levels>,
    ready: mpsc::Sender<Result<Vec<Vec<u8>>>>,
    mut on_packet: impl FnMut(AudioPacket),
) {
    // The timeline starts now, before the (slow) encoder and device setup: sources place data by QPC
    // time, so the setup time simply becomes leading silence.
    let start = now_100ns();
    // Declared first so they are released last.
    let _com;
    let _mf;
    let setup = (|| -> Result<_> {
        let com = ComRuntime::start()?;
        let mf = MfRuntime::start()?;
        let mut encoders = Vec::new();
        let mut blobs = Vec::new();
        for track in 0..plan.tracks.len() {
            let encoder = AacEncoder::new(track, config.bitrate_kbps, start)?;
            blobs.push(encoder.media_type_blob()?);
            encoders.push(encoder);
        }
        let devices = wasapi::enumerator()?;
        Ok((com, mf, encoders, blobs, devices))
    })();
    let (mut encoders, devices) = match setup {
        Ok((com, mf, encoders, blobs, devices)) => {
            _com = com;
            _mf = mf;
            let _ = ready.send(Ok(blobs));
            (encoders, devices)
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };

    let (mut sources, gains, extra_latency) = build_sources(config, &plan, &levels);
    let latency = LATENCY_100NS + extra_latency;
    let mut timeline = Timeline::new(
        start,
        &gains,
        plan.tracks.iter().map(|t| t.sources.clone()).collect(),
    );
    let mut pcm: Vec<Vec<i16>> = vec![Vec::new(); plan.tracks.len()];

    let mut encode = |timeline: &mut Timeline, until: i64, encoders: &mut [AacEncoder]| {
        pcm.iter_mut().for_each(Vec::clear);
        let from = time_at(start, timeline.emitted_frames());
        if timeline.pop_until(until, &mut pcm) == 0 {
            return;
        }
        for (encoder, pcm) in encoders.iter_mut().zip(&pcm) {
            if let Err(e) = encoder.encode(pcm, from, &mut on_packet) {
                tracing::error!("audio encoding failed: {e:#}");
            }
        }
    };

    while !stop.load(Ordering::Acquire) {
        for source in &mut sources {
            source.poll(&devices, &mut timeline);
        }
        encode(&mut timeline, now_100ns() - latency, &mut encoders);
        std::thread::sleep(POLL);
    }

    // Final read, everything up to now, and flush the encoders.
    for source in &mut sources {
        source.poll(&devices, &mut timeline);
    }
    encode(&mut timeline, now_100ns(), &mut encoders);
    for encoder in &mut encoders {
        if let Err(e) = encoder.drain(&mut on_packet) {
            tracing::error!("flushing an audio encoder failed: {e:#}");
        }
    }
}

// `AudioCapture` must stay `Send` (the engine moves it between threads).
const _: fn() = || {
    fn send<T: Send>() {}
    send::<AudioCapture>();
};
