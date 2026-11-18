//! The video pipeline thread: capture a monitor, convert to NV12, hardware-encode, and push the packets
//! into the replay buffer.
//!
//! The loop runs at a fixed frame rate. WGC only delivers a frame when the screen changes, so a tick
//! without a new frame encodes the previous one again; the clip's timing stays even no matter how
//! irregularly the game presents. If the loop falls far behind (the PC is overloaded), it skips ahead
//! instead of drifting out of sync with the audio.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clipr_capture::{CaptureOptions, CaptureSession, CapturedFrame, Target};
use clipr_core::clock::now_100ns;
use clipr_encode::mf::HwEncoder;
use clipr_encode::{Codec, EncodeSettings};
use clipr_gpu::{Gpu, Nv12Converter, ScaleMode, Snapshotter};
use clipr_mux::MediaFormat;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D11::{D3D11_TEXTURE2D_DESC, ID3D11Texture2D};
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::Media::MediaFoundation::{IMFMediaType, MF_MT_MPEG_SEQUENCE_HEADER};

use super::{Shared, StreamOut};
use crate::preview;

#[derive(Clone, Debug, PartialEq)]
pub struct VideoSettings {
    /// The monitor to capture (an `HMONITOR` value). With `window` set, the monitor that window is on.
    pub monitor: isize,
    /// The game window to capture (an `HWND` value) instead of the whole monitor.
    pub window: Option<isize>,
    pub encode: EncodeSettings,
    /// Height asked for in the settings (0 = native); the width follows the monitor's shape.
    pub height: u32,
    pub gpu_priority: bool,
}

/// Starts the pipeline thread. It runs until `stop` is set or something fails.
pub fn spawn(
    settings: VideoSettings,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<Result<()>> {
    std::thread::Builder::new()
        .name("clipr-video".into())
        .spawn(move || {
            let _clear = preview::ClearOnDrop;
            run(settings, &shared, &stop)
        })
        .expect("can't start a thread")
}

/// The output size for a monitor of `size`: `height` tall (never taller than the monitor), as wide as the
/// monitor's shape needs, both even (encoders need even sizes).
pub fn output_size((w, h): (u32, u32), height: u32) -> (u32, u32) {
    let even = |v: u32| (v & !1).max(2);
    if height == 0 || height >= h {
        return (even(w), even(h));
    }
    let width = (u64::from(w) * u64::from(height) / u64::from(h.max(1))) as u32;
    (even(width), even(height))
}

fn run(settings: VideoSettings, shared: &Mutex<Shared>, stop: &AtomicBool) -> Result<()> {
    // Only clipr's own frame thread: keeps capture smooth when a game loads every core, at no cost to it.
    clipr_gpu::raise_thread_priority();
    let monitor = HMONITOR(settings.monitor as *mut _);
    let gpu = Gpu::for_monitor(monitor)?;
    clipr_gpu::set_gpu_priority(settings.gpu_priority);
    let monitor_size = clipr_capture::list_monitors()?
        .into_iter()
        .find(|m| m.handle == monitor)
        .map(|m| {
            (
                (m.rect.right - m.rect.left) as u32,
                (m.rect.bottom - m.rect.top) as u32,
            )
        })
        .context("the monitor to capture is gone")?;
    let latest: Arc<Mutex<Option<CapturedFrame>>> = Arc::default();
    let start = |target: Target| {
        let latest = latest.clone();
        CaptureSession::start(
            &gpu,
            target,
            CaptureOptions {
                buffers: 3,
                ..Default::default()
            },
            move |frame| {
                // Keep only the newest; dropping the older one hands its surface back to WGC.
                *latest.lock().unwrap() = Some(frame);
            },
        )
    };
    // A game window that Windows won't capture (some protect themselves) falls back to its monitor.
    let (mut session, source_size) = match settings.window {
        Some(hwnd) => match start(Target::Window(HWND(hwnd as *mut _))) {
            Ok(session) => {
                let size = session.content_size();
                let size = if size.0 >= 2 && size.1 >= 2 {
                    size
                } else {
                    monitor_size
                };
                (session, size)
            }
            Err(e) => {
                tracing::warn!("can't capture the game window, capturing its monitor instead: {e:#}");
                (start(Target::Monitor(monitor))?, monitor_size)
            }
        },
        None => (start(Target::Monitor(monitor))?, monitor_size),
    };
    // The output keeps this size; a window resized later is scaled to fit it.
    let size = output_size(source_size, settings.height);
    let encode = EncodeSettings {
        width: size.0,
        height: size.1,
        ..settings.encode.clone()
    };
    let mut encoder = HwEncoder::new(&gpu, &encode)?;
    tracing::info!(
        "capturing the {} {}x{} → {}x{} {:?} {} fps {} kbps with {}",
        if settings.window.is_some() {
            "game window"
        } else {
            "monitor"
        },
        source_size.0,
        source_size.1,
        size.0,
        size.1,
        encode.codec,
        encode.fps,
        encode.bitrate_kbps,
        encoder.name()
    );
    shared.lock().unwrap().encoder = Some(encoder.name().to_string());

    let fps = encode.fps.max(1) as i64;
    let period = 10_000_000 / fps;
    let mut converter: Option<Nv12Converter> = None;
    let mut last: Option<ID3D11Texture2D> = None;
    let mut live = Preview::default();
    let mut tick: i64 = 0;
    let t0 = now_100ns();
    // A window that sends no picture at all (some exclusive-fullscreen games) falls back to the monitor.
    let mut window_deadline = settings.window.map(|_| Instant::now() + WINDOW_FIRST_FRAME);
    let mut stream: Option<StreamEncoder> = None;
    // The stream whose encoder couldn't start: not tried again every frame.
    let mut stream_failed: Option<clipr_stream::Sink> = None;
    let mut on_packet = |encoder: &HwEncoder, sample: clipr_encode::mf::Packet| -> Result<()> {
        let packet = clipr_mux::packet_from_sample(&sample)?;
        let mut s = shared.lock().unwrap();
        if s.video_format.is_none() {
            s.video_format = Some(MediaFormat::from_type(&encoder.output_type()?)?);
        }
        s.push_video(packet);
        Ok(())
    };

    while !stop.load(Ordering::Relaxed) {
        let deadline = t0 + tick * period;
        let now = now_100ns();
        if now < deadline {
            std::thread::sleep(Duration::from_nanos(((deadline - now) * 100) as u64));
        } else if now - deadline > 5 * period {
            // Far behind: skip ahead rather than drift away from real time.
            let skip = (now - deadline) / period;
            tick += skip;
            shared.lock().unwrap().stats.dropped += skip as u64;
            continue;
        } else if now - deadline > period {
            shared.lock().unwrap().stats.late += 1;
        }

        // Going live or ending a stream only adds or drops the stream's own encoder: capture goes on.
        let wanted = shared.lock().unwrap().stream.clone();
        match &wanted {
            None => {
                stream = None;
                stream_failed = None;
            }
            Some(w)
                if stream.as_ref().is_none_or(|s| !s.serves(w))
                    && stream_failed.as_ref().is_none_or(|f| !f.same(&w.sink)) =>
            {
                stream = match StreamEncoder::new(&gpu, w, size, encode.fps) {
                    Ok(s) => Some(s),
                    Err(e) => {
                        stream_failed = Some(w.sink.clone());
                        shared.lock().unwrap().stream_error =
                            Some(format!("can't start an encoder for the stream: {e:#}"));
                        None
                    }
                };
            }
            Some(_) => {}
        }

        let frame = latest.lock().unwrap().take();
        let work_start = now_100ns();
        if let Some(frame) = frame {
            let input = (frame.width, frame.height);
            // A new size (resolution change, window resize) needs a new converter; the output size stays.
            if converter.as_ref().map(Nv12Converter::input_size) != Some(input) {
                converter = Some(Nv12Converter::new(
                    &gpu,
                    input,
                    size,
                    encode.fps,
                    ScaleMode::Fit,
                    8,
                )?);
            }
            last = Some(converter.as_mut().expect("just set").convert(&frame.texture)?);
            if let Some(s) = stream.as_mut()
                && let Err(e) = s.take_frame(&gpu, &frame.texture, input)
            {
                stream = stream_lost(shared, stream.take(), &mut stream_failed, e);
            }
            shared.lock().unwrap().stats.fresh += 1;
        } else if last.is_some() {
            shared.lock().unwrap().stats.repeated += 1;
        } else {
            // Nothing captured yet (WGC sends the first frame when the screen next changes).
            if window_deadline.is_some_and(|d| Instant::now() >= d) {
                tracing::warn!("the game window sent no picture; capturing its monitor instead");
                window_deadline = None;
                session.stop();
                session = start(Target::Monitor(monitor))?;
            }
            tick += 1;
            continue;
        }
        let frame = last.as_ref().expect("checked above");
        live.tick(&gpu, frame);
        encoder.encode(frame, deadline, &mut on_packet)?;
        if let Some(s) = stream.as_mut()
            && tick % s.stride == 0
            && let Err(e) = s.encode(frame, deadline)
        {
            stream = stream_lost(shared, stream.take(), &mut stream_failed, e);
        }
        shared.lock().unwrap().stats.work_100ns += (now_100ns() - work_start).max(0) as u64;
        tick += 1;
    }

    session.stop();
    encoder.drain(&mut on_packet)?;
    Ok(())
}

/// A stream encoder that failed: tell the supervisor (which ends the stream) and don't try it again.
fn stream_lost(
    shared: &Mutex<Shared>,
    lost: Option<StreamEncoder>,
    failed: &mut Option<clipr_stream::Sink>,
    e: anyhow::Error,
) -> Option<StreamEncoder> {
    tracing::warn!("the stream's encoder failed: {e:#}");
    if let Some(s) = lost {
        *failed = Some(s.sink);
    }
    shared.lock().unwrap().stream_error = Some(format!("the stream's encoder failed: {e:#}"));
    None
}

/// The live stream's own H.264 encoder, at the stream's size, frame rate and bitrate (clips keep theirs). It
/// takes every `stride`-th frame of capture.
struct StreamEncoder {
    sink: clipr_stream::Sink,
    /// The height, fps and bitrate it was made for.
    made_for: (u32, u32, u32),
    encoder: HwEncoder,
    /// The stream is smaller than the clips: it scales the captured picture itself. Otherwise the clips' NV12
    /// frame is encoded as it is.
    scaling: bool,
    /// Made with the first frame, for its input size.
    converter: Option<(Nv12Converter, (u32, u32))>,
    size: (u32, u32),
    /// The newest picture at the stream's size (with a converter).
    last: Option<ID3D11Texture2D>,
    stride: i64,
    sent_config: bool,
}

impl StreamEncoder {
    fn new(gpu: &Gpu, out: &StreamOut, capture: (u32, u32), capture_fps: u32) -> Result<Self> {
        let size = output_size(capture, out.height);
        let stride = capture_fps.div_ceil(out.fps.max(1)).max(1);
        let fps = (capture_fps / stride).max(1);
        let encoder = HwEncoder::new(
            gpu,
            &EncodeSettings {
                codec: Codec::H264,
                width: size.0,
                height: size.1,
                fps,
                bitrate_kbps: out.bitrate_kbps,
                // What Twitch and YouTube ask for.
                keyframe_interval_secs: 2.0,
            },
        )?;
        tracing::info!(
            "stream encoder: {}x{} {} fps {} kbps H.264 with {}",
            size.0,
            size.1,
            fps,
            out.bitrate_kbps,
            encoder.name()
        );
        out.sink.set_size(size.0, size.1);
        Ok(Self {
            sink: out.sink.clone(),
            made_for: (out.height, out.fps, out.bitrate_kbps),
            encoder,
            scaling: size != capture,
            converter: None,
            size,
            last: None,
            stride: i64::from(stride),
            sent_config: false,
        })
    }

    fn serves(&self, out: &StreamOut) -> bool {
        self.sink.same(&out.sink) && self.made_for == (out.height, out.fps, out.bitrate_kbps)
    }

    /// A fresh captured frame: scale it to the stream's size, when that differs from the clips'.
    fn take_frame(&mut self, gpu: &Gpu, texture: &ID3D11Texture2D, input: (u32, u32)) -> Result<()> {
        if !self.scaling {
            return Ok(());
        }
        if self.converter.as_ref().map(|(_, i)| *i) != Some(input) {
            let c = Nv12Converter::new(gpu, input, self.size, 60, ScaleMode::Fit, 8)?;
            self.converter = Some((c, input));
        }
        let (c, _) = self.converter.as_mut().expect("just set");
        self.last = Some(c.convert(texture)?);
        Ok(())
    }

    fn encode(&mut self, clips_frame: &ID3D11Texture2D, time: i64) -> Result<()> {
        let frame = if self.scaling {
            match &self.last {
                Some(f) => f.clone(),
                None => return Ok(()),
            }
        } else {
            clips_frame.clone()
        };
        let sink = &self.sink;
        let sent_config = &mut self.sent_config;
        self.encoder.encode(&frame, time, &mut |encoder, sample| {
            let packet = clipr_mux::packet_from_sample(&sample)?;
            if !*sent_config {
                *sent_config = true;
                if let Some(header) = encoder.output_type().ok().as_ref().and_then(sequence_header) {
                    sink.video_config(&header);
                }
            }
            sink.video(packet.pts, packet.keyframe, packet.data);
            Ok(())
        })
    }
}

/// The H.264 parameter sets an encoder keeps in its output type, if it does.
fn sequence_header(t: &IMFMediaType) -> Option<Vec<u8>> {
    // SAFETY: a valid media type; the buffer has the size it reported.
    unsafe {
        let n = t.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER).ok()?;
        let mut v = vec![0u8; n as usize];
        t.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut v, None).ok()?;
        Some(v)
    }
}

/// The live preview's snapshots (see [`crate::preview`]): at [`preview::interval`] while a page
/// watches, made from the frame the encoder gets and read back a tick later. If the GPU can't make them, the
/// page is told, capture goes on, and the preview tries again after [`RETRY`].
#[derive(Default)]
struct Preview {
    snapshotter: Option<Snapshotter>,
    /// The picture size the snapshotter makes: a page that wants a bigger (or smaller) one gets a new
    /// snapshotter, but only when the picture really changes (asking 1920 of a 1280-wide capture doesn't).
    out: (u32, u32),
    next: Option<Instant>,
}

/// How long a game window may take to send its first picture before capture falls back to the monitor.
const WINDOW_FIRST_FRAME: Duration = Duration::from_secs(3);

/// How long the preview waits after a GPU failure before it tries again.
const RETRY: Duration = Duration::from_secs(10);

impl Preview {
    fn tick(&mut self, gpu: &Gpu, frame: &ID3D11Texture2D) {
        if let Err(e) = self.try_tick(gpu, frame) {
            let e = format!("{e:#}");
            tracing::warn!(
                "the live preview failed (trying again in {} s): {e}",
                RETRY.as_secs()
            );
            preview::fail(e);
            self.snapshotter = None;
            self.next = Some(Instant::now() + RETRY);
        }
    }

    fn try_tick(&mut self, gpu: &Gpu, frame: &ID3D11Texture2D) -> Result<()> {
        if let Some(s) = self.snapshotter.as_mut()
            && let Some(snapshot) = s.take()?
        {
            preview::publish(snapshot);
        }
        let now = Instant::now();
        if self.next.is_some_and(|t| now < t)
            || self.snapshotter.as_ref().is_some_and(Snapshotter::pending)
            || !preview::wanted()
        {
            return Ok(());
        }
        let size = texture_size(frame);
        let max = preview::max_size();
        let out = clipr_gpu::fit_size(size, max);
        if self.snapshotter.as_ref().map(Snapshotter::input_size) != Some(size) || self.out != out {
            self.snapshotter = Some(Snapshotter::new(gpu, size, max)?);
            self.out = out;
        }
        self.snapshotter.as_mut().expect("just set").start(frame)?;
        self.next = Some(now + preview::interval());
        Ok(())
    }
}

fn texture_size(texture: &ID3D11Texture2D) -> (u32, u32) {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: fills a plain descriptor from a valid texture.
    unsafe { texture.GetDesc(&mut desc) };
    (desc.Width, desc.Height)
}

#[cfg(test)]
mod tests {
    use super::output_size;

    #[test]
    fn output_keeps_the_monitor_shape() {
        assert_eq!(output_size((2560, 1440), 1080), (1920, 1080));
        assert_eq!(output_size((3440, 1440), 1080), (2580, 1080));
        assert_eq!(output_size((1920, 1080), 0), (1920, 1080));
        assert_eq!(output_size((1920, 1080), 1440), (1920, 1080));
        assert_eq!(output_size((1366, 768), 720), (1280, 720));
    }
}
