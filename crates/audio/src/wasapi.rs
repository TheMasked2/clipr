//! WASAPI capture: the default render device (loopback), the default capture device (microphone), or one
//! app's audio (process loopback).
//!
//! A [`Source`] opens its device lazily, follows the default device (or the app's process) when it
//! changes, retries after failures, runs each source's filter chain, and pushes everything it reads onto the
//! [`Timeline`] with the packet's QPC time.

use std::mem::ManuallyDrop;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use clipr_core::clock::now_100ns;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_E_DEVICE_INVALIDATED,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, AUDIOCLIENT_ACTIVATION_PARAMS,
    AUDIOCLIENT_ACTIVATION_PARAMS_0, AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
    AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS, ActivateAudioInterfaceAsync, EDataFlow,
    IActivateAudioInterfaceAsyncOperation, IActivateAudioInterfaceCompletionHandler,
    IActivateAudioInterfaceCompletionHandler_Impl, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator,
    MMDeviceEnumerator, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
    VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, WAVE_FORMAT_PCM, WAVEFORMATEX, WAVEFORMATEXTENSIBLE, eCapture,
    eConsole, eRender,
};
use windows::Win32::Media::KernelStreaming::{KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE};
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
};
use windows::Win32::System::Com::{
    BLOB, CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, IAgileObject, IAgileObject_Impl,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Variant::VT_BLOB;
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::core::{IUnknown, Interface, implement};

use crate::chain::{Ptt, SourceChain};
use crate::levels::{Levels, Meter};
use crate::mix::RESYNC_FRAMES;
use crate::mix::{RATE, Resampler, SampleKind, Timeline, to_stereo};
use crate::plan::SourceKind;

/// How often a source checks that its device is still the default one, or that its app is still the
/// same process.
const DEFAULT_CHECK: Duration = Duration::from_secs(2);
const APP_CHECK: Duration = Duration::from_secs(3);

/// How long a source that failed to open waits before trying again.
const RETRY: Duration = Duration::from_secs(3);
const APP_RETRY: Duration = Duration::from_secs(30);

/// WASAPI buffer length (100 ns units): 150 ms.
const BUFFER_100NS: i64 = 1_500_000;

fn label(kind: &SourceKind) -> String {
    match kind {
        SourceKind::System => "system audio".into(),
        SourceKind::Mic => "microphone".into(),
        SourceKind::Game => "game audio".into(),
        SourceKind::App(exe) => format!("app {exe}"),
    }
}

// Declared by hand: the windows crate only has it behind the Win32_Security feature, which nothing else
// here needs.
unsafe extern "system" {
    fn CreateEventW(
        attributes: *const std::ffi::c_void,
        manual_reset: i32,
        initial_state: i32,
        name: *const u16,
    ) -> HANDLE;
}

/// The enumerator every source asks for devices.
pub fn enumerator() -> Result<IMMDeviceEnumerator> {
    // SAFETY: plain COM creation; COM is initialised on this thread.
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.context("can't list audio devices")
}

/// A capture session that is currently open.
struct Open {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    /// Device id, or the process id for an app.
    id: String,
    kind: SampleKind,
    channels: usize,
    resampler: Option<Resampler>,
    event: Option<HANDLE>,
    /// Scratch buffers, kept to avoid allocating on every packet.
    stereo: Vec<f32>,
    resampled: Vec<f32>,
    processed: Vec<f32>,
}

impl Open {
    fn new(
        client: IAudioClient,
        capture: IAudioCaptureClient,
        id: String,
        kind: SampleKind,
        channels: usize,
        rate: u32,
        event: Option<HANDLE>,
    ) -> Self {
        Self {
            client,
            capture,
            id,
            kind,
            channels,
            resampler: (rate != RATE).then(|| Resampler::new(rate, RATE)),
            event,
            stereo: Vec::new(),
            resampled: Vec::new(),
            processed: Vec::new(),
        }
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        // SAFETY: valid client (stopping a stopped client is harmless); the event handle is ours.
        unsafe {
            let _ = self.client.Stop();
            if let Some(event) = self.event {
                let _ = CloseHandle(event);
            }
        }
    }
}

/// The push-to-talk key and its state.
pub struct PttKey {
    pub vk: i32,
    pub state: Ptt,
}

/// What a [`Source`] does to its audio beyond capturing it.
pub struct Processing {
    /// Shift on the timeline (positive = later), already including the chain's delay compensation.
    pub shift_100ns: i64,
    pub chain: Option<SourceChain>,
    pub ptt: Option<PttKey>,
    /// QPC time just after the last packet, to notice gaps (the chain's delayed tail must be flushed).
    pub last_end: Option<i64>,
}

pub struct Source {
    kind: SourceKind,
    lane: usize,
    processing: Processing,
    levels: Arc<Levels>,
    level_index: usize,
    meter: Meter,
    /// The same after the filter chain and push-to-talk.
    meter_after: Meter,
    /// This source's key, for the mute list.
    key: String,
    /// Whether this source is muted in clips, as of mute list change `muted_gen`.
    muted: bool,
    muted_gen: u64,
    epoch: Instant,
    open: Option<Open>,
    /// The process an app source is (or should be) capturing.
    pid: Option<u32>,
    retry_at: Instant,
    next_check: Instant,
    /// Whether the last open attempt failed, so a persistent failure logs once.
    failing: bool,
}

impl Source {
    pub fn new(
        kind: SourceKind,
        lane: usize,
        processing: Processing,
        levels: Arc<Levels>,
        level_index: usize,
    ) -> Self {
        let now = Instant::now();
        let key = kind.key();
        Self {
            kind,
            lane,
            processing,
            levels,
            level_index,
            meter: Meter::new(),
            meter_after: Meter::new(),
            key,
            muted: false,
            // Not a real generation, so the first poll reads the list.
            muted_gen: u64::MAX,
            epoch: now,
            open: None,
            pid: None,
            retry_at: now,
            next_check: now,
            failing: false,
        }
    }

    /// Reads everything the device has and puts it on the timeline. Never fails: problems close the
    /// device and are retried later, and the timeline carries on with silence.
    pub fn poll(&mut self, devices: &IMMDeviceEnumerator, timeline: &mut Timeline) {
        let now = Instant::now();
        self.check_target(devices, now);
        self.ensure_open(devices, now);

        if let Some(open) = &mut self.open {
            let open_ptt = match &mut self.processing.ptt {
                Some(p) => {
                    // SAFETY: plain API call; the high bit is set while the key is down.
                    let held = unsafe { GetAsyncKeyState(p.vk) } < 0;
                    p.state
                        .update(held, now.duration_since(self.epoch).as_millis() as u64)
                }
                None => true,
            };
            // Muting (the mute hotkey, the mixer) closes a source the same way push-to-talk does.
            let generation = crate::muted_gen();
            if generation != self.muted_gen {
                self.muted_gen = generation;
                self.muted = crate::is_muted(&self.key);
            }
            let open_ptt = open_ptt && !self.muted;
            let result = read_all(
                open,
                self.lane,
                timeline,
                &mut self.processing,
                open_ptt,
                [&mut self.meter, &mut self.meter_after],
            );
            if let Err(e) = result {
                if e.downcast_ref::<windows::core::Error>()
                    .is_some_and(|e| e.code() == AUDCLNT_E_DEVICE_INVALIDATED)
                {
                    tracing::info!("{}: the device went away, reopening", label(&self.kind));
                    self.retry_at = now;
                } else {
                    // Back off, so a device that keeps failing can't spin this thread or flood the log.
                    if !self.failing {
                        tracing::warn!("{}: lost the device: {e:#}", label(&self.kind));
                    }
                    self.failing = true;
                    self.retry_at = now + Duration::from_secs(1);
                }
                self.open = None;
            }
        }
        if let Some(db) = self.meter.take(now) {
            self.levels.set(self.level_index, db);
        }
        if let Some(db) = self.meter_after.take(now) {
            self.levels.set_after(self.level_index, db);
        }
    }

    /// Notices a changed default device or app process and drops the open session so it is reopened.
    fn check_target(&mut self, devices: &IMMDeviceEnumerator, now: Instant) {
        if now < self.next_check {
            return;
        }
        match &self.kind {
            kind @ (SourceKind::App(_) | SourceKind::Game) => {
                self.next_check = now + APP_CHECK;
                let pid = match kind {
                    SourceKind::App(exe) => find_process(exe),
                    _ => crate::game_pid(),
                };
                if pid != self.pid {
                    if let Some(pid) = pid {
                        tracing::info!("{}: found it (process {pid})", label(&self.kind));
                    } else if self.pid.is_some() {
                        tracing::info!("{}: not running any more", label(&self.kind));
                    }
                    self.pid = pid;
                    self.open = None;
                    self.failing = false;
                    self.retry_at = now;
                }
            }
            kind => {
                self.next_check = now + DEFAULT_CHECK;
                let flow = if *kind == SourceKind::Mic {
                    eCapture
                } else {
                    eRender
                };
                let current = self.open.as_ref().map(|o| o.id.as_str());
                if let Ok(id) = default_id(devices, flow)
                    && current.is_some_and(|c| c != id)
                {
                    tracing::info!("{}: the default device changed, switching", label(kind));
                    self.open = None;
                    self.retry_at = now;
                }
            }
        }
    }

    fn ensure_open(&mut self, devices: &IMMDeviceEnumerator, now: Instant) {
        if self.open.is_some() || now < self.retry_at {
            return;
        }
        let result = match &self.kind {
            SourceKind::System => open_endpoint(devices, eRender, true),
            SourceKind::Mic => open_endpoint(devices, eCapture, false),
            // Not running: silence, no error.
            SourceKind::Game | SourceKind::App(_) => match self.pid {
                Some(pid) => open_process(pid),
                None => return,
            },
        };
        match result {
            Ok(open) => {
                tracing::info!(
                    "{}: capturing {} ({:?}, {} ch)",
                    label(&self.kind),
                    open.id,
                    open.kind,
                    open.channels
                );
                self.failing = false;
                self.open = Some(open);
            }
            Err(e) => {
                if !self.failing {
                    tracing::warn!("{} unavailable, will keep trying: {e:#}", label(&self.kind));
                }
                self.failing = true;
                // Activating an app's loopback can block this thread for a while: try it less often.
                let wait = if self.kind.is_process() { APP_RETRY } else { RETRY };
                self.retry_at = now + wait;
            }
        }
    }
}

/// The id of the current default device for `flow`.
fn default_id(devices: &IMMDeviceEnumerator, flow: EDataFlow) -> Result<String> {
    // SAFETY: valid enumerator; the id string is CoTaskMem-allocated and freed below.
    unsafe {
        let device = devices.GetDefaultAudioEndpoint(flow, eConsole)?;
        let id = device.GetId()?;
        let text = id.to_string();
        CoTaskMemFree(Some(id.0.cast()));
        text.map_err(|e| anyhow!("bad device id: {e}"))
    }
}

/// Opens the default device for `flow` in shared mode (loopback for render) and starts it.
fn open_endpoint(devices: &IMMDeviceEnumerator, flow: EDataFlow, loopback: bool) -> Result<Open> {
    let what = if loopback { "output" } else { "input" };
    // SAFETY: valid enumerator and COM objects throughout; the mix format is CoTaskMem-allocated, read
    // before it's freed, and the client copies it in Initialize.
    unsafe {
        let device = devices
            .GetDefaultAudioEndpoint(flow, eConsole)
            .with_context(|| format!("no default {what} device"))?;
        let id = device.GetId()?;
        let name = id.to_string().unwrap_or_default();
        CoTaskMemFree(Some(id.0.cast()));
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;

        let format = client.GetMixFormat()?;
        let parsed = parse_format(format);
        let flags = if loopback { AUDCLNT_STREAMFLAGS_LOOPBACK } else { 0 };
        let init = client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, BUFFER_100NS, 0, format, None);
        CoTaskMemFree(Some(format.cast()));
        let (kind, channels, rate) = parsed?;
        init.with_context(|| format!("can't open the {what} device"))?;

        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        Ok(Open::new(client, capture, name, kind, channels, rate, None))
    }
}

/// Completion handler for `ActivateAudioInterfaceAsync`: reports on a channel. It's free-threaded, as
/// the API requires.
#[implement(IActivateAudioInterfaceCompletionHandler, IAgileObject)]
struct ActivationDone(mpsc::Sender<()>);

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivationDone_Impl {
    fn ActivateCompleted(
        &self,
        _operation: windows::core::Ref<'_, IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        let _ = self.0.send(());
        Ok(())
    }
}

impl IAgileObject_Impl for ActivationDone_Impl {}

/// Captures the audio of process `pid` and its children (needs Windows 10 2004 or newer).
fn open_process(pid: u32) -> Result<Open> {
    let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };
    // Never dropped: PROPVARIANT's Drop would CoTaskMemFree the blob, which lives on our stack.
    let variant = ManuallyDrop::new(PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_BLOB,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 {
                    blob: BLOB {
                        cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                        pBlobData: (&mut params as *mut AUDIOCLIENT_ACTIVATION_PARAMS).cast(),
                    },
                },
            }),
        },
    });
    let (tx, rx) = mpsc::channel();
    let handler: IActivateAudioInterfaceCompletionHandler = ActivationDone(tx).into();
    // SAFETY: `variant` and `params` outlive the call (the blob is only read during it); the handler is
    // agile; the format is a valid WAVEFORMATEX; the event handle is closed by `Open` on drop.
    unsafe {
        let operation = ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&*variant),
            &handler,
        )
        .context("per-app audio needs Windows 10 version 2004 or newer")?;
        rx.recv_timeout(Duration::from_secs(2))
            .map_err(|_| anyhow!("Windows didn't answer the per-app audio request"))?;
        let mut result = windows::core::HRESULT(0);
        let mut activated: Option<IUnknown> = None;
        operation.GetActivateResult(&mut result, &mut activated)?;
        result
            .ok()
            .context("can't capture this app's audio (is it still running?)")?;
        let client: IAudioClient = activated.context("no audio client")?.cast()?;

        // The virtual device has no mix format: ask for 32-bit float stereo at 48 kHz.
        let format = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_IEEE_FLOAT as u16,
            nChannels: 2,
            nSamplesPerSec: RATE,
            nAvgBytesPerSec: RATE * 8,
            nBlockAlign: 8,
            wBitsPerSample: 32,
            cbSize: 0,
        };
        client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK
                    | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                    | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                    | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                BUFFER_100NS,
                0,
                &format,
                None,
            )
            .context("can't open the app's audio")?;
        // Event-driven streams must have an event; we still poll, so it's never waited on.
        let event = CreateEventW(std::ptr::null(), 0, 0, std::ptr::null());
        if event.is_invalid() {
            return Err(windows::core::Error::from_win32().into());
        }
        if let Err(e) = client.SetEventHandle(event) {
            let _ = CloseHandle(event);
            return Err(e.into());
        }
        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        Ok(Open::new(
            client,
            capture,
            format!("process {pid}"),
            SampleKind::F32,
            2,
            RATE,
            Some(event),
        ))
    }
}

/// The process id of the lowest-numbered running process named `exe` that isn't a child of another one
/// with the same name (its children are captured along with it), if any is running.
fn find_process(exe: &str) -> Option<u32> {
    let wanted = exe.to_ascii_lowercase();
    let wanted_exe = format!("{wanted}.exe");
    let mut found: Vec<(u32, u32)> = Vec::new();
    // SAFETY: plain snapshot calls; the entry's size is set before use and the handle is closed.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
        while more {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(260);
            let name = String::from_utf16_lossy(&entry.szExeFile[..len]).to_ascii_lowercase();
            if name == wanted || name == wanted_exe {
                found.push((entry.th32ProcessID, entry.th32ParentProcessID));
            }
            more = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
    }
    pick_root(&found)
}

/// The lowest pid among `(pid, parent)` entries whose parent isn't also in the list.
fn pick_root(found: &[(u32, u32)]) -> Option<u32> {
    found
        .iter()
        .filter(|(_, parent)| !found.iter().any(|(pid, _)| pid == parent))
        .map(|(pid, _)| *pid)
        .min()
}

/// Sample layout, channel count and rate of a device mix format.
///
/// # Safety
/// `format` must point to a valid WAVEFORMATEX (a WAVEFORMATEXTENSIBLE when its tag says so).
unsafe fn parse_format(format: *const WAVEFORMATEX) -> Result<(SampleKind, usize, u32)> {
    // SAFETY: per the contract; the structs are packed, so fields are read unaligned.
    unsafe {
        let tag = u32::from(std::ptr::addr_of!((*format).wFormatTag).read_unaligned());
        let channels = std::ptr::addr_of!((*format).nChannels).read_unaligned() as usize;
        let rate = std::ptr::addr_of!((*format).nSamplesPerSec).read_unaligned();
        let bits = std::ptr::addr_of!((*format).wBitsPerSample).read_unaligned();
        let float = if tag == WAVE_FORMAT_EXTENSIBLE {
            let sub = std::ptr::addr_of!((*format.cast::<WAVEFORMATEXTENSIBLE>()).SubFormat).read_unaligned();
            if sub == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {
                true
            } else if sub == KSDATAFORMAT_SUBTYPE_PCM {
                false
            } else {
                bail!("unsupported audio format {sub:?}");
            }
        } else if tag == WAVE_FORMAT_IEEE_FLOAT {
            true
        } else if tag == WAVE_FORMAT_PCM {
            false
        } else {
            bail!("unsupported audio format tag {tag}");
        };
        let kind = match (float, bits) {
            (true, 32) => SampleKind::F32,
            (false, 16) => SampleKind::I16,
            _ => bail!("unsupported sample size ({bits} bits, float: {float})"),
        };
        if channels == 0 || rate == 0 {
            bail!("the device reports no channels or no sample rate");
        }
        Ok((kind, channels, rate))
    }
}

/// Moves every waiting packet onto the timeline.
fn read_all(
    open: &mut Open,
    lane: usize,
    timeline: &mut Timeline,
    processing: &mut Processing,
    ptt_open: bool,
    [meter, meter_after]: [&mut Meter; 2],
) -> Result<()> {
    loop {
        // SAFETY: valid capture client.
        let waiting = unsafe { open.capture.GetNextPacketSize() }?;
        if waiting == 0 {
            return Ok(());
        }
        let (mut data, mut frames, mut flags, mut qpc) = (std::ptr::null_mut(), 0u32, 0u32, 0u64);
        // SAFETY: valid out-pointers; the buffer stays valid until ReleaseBuffer below.
        unsafe {
            open.capture
                .GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc))?;
        }
        open.stereo.clear();
        if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
            open.stereo.resize(frames as usize * 2, 0.0);
        } else {
            let len = frames as usize * open.channels * open.kind.width();
            // SAFETY: WASAPI guarantees `frames` frames of the stream format at `data` until ReleaseBuffer.
            let bytes = unsafe { std::slice::from_raw_parts(data, len) };
            to_stereo(bytes, open.kind, open.channels, &mut open.stereo);
        }
        // SAFETY: releases exactly the frames got above.
        unsafe { open.capture.ReleaseBuffer(frames) }?;

        // Some virtual devices don't report a capture time: assume the packet just ended.
        let qpc = if qpc == 0 {
            now_100ns() - frames as i64 * 10_000_000 / RATE as i64
        } else {
            qpc as i64
        };
        let discontinuity = flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0;
        let samples = match &mut open.resampler {
            Some(r) => {
                if discontinuity {
                    r.reset(); // don't blend across the gap
                }
                open.resampled.clear();
                r.process(&open.stereo, &mut open.resampled);
                &open.resampled
            }
            None => &open.stereo,
        };
        meter.add(samples);
        let duration = (samples.len() / 2) as i64 * 10_000_000 / RATE as i64;
        let gap = |end: i64| discontinuity || (qpc - end).abs() > RESYNC_FRAMES * 10_000_000 / RATE as i64;
        if let (Some(chain), Some(end)) = (&mut processing.chain, processing.last_end)
            && chain.delay_frames() > 0
            && gap(end)
        {
            // The chain still holds the end of the previous sound. Put it where it belongs (right after
            // that sound), then start fresh, so it isn't played at the new packet's time.
            open.processed.clear();
            chain.flush(ptt_open, &mut open.processed);
            meter_after.add(&open.processed);
            timeline.push(lane, end + processing.shift_100ns, &open.processed, false);
        }
        processing.last_end = Some(qpc + duration);
        let samples = match &mut processing.chain {
            Some(chain) => {
                open.processed.clear();
                chain.process(samples, ptt_open, &mut open.processed);
                &open.processed
            }
            None => samples,
        };
        meter_after.add(samples);
        timeline.push(lane, qpc + processing.shift_100ns, samples, discontinuity);
    }
}

#[cfg(test)]
mod tests {
    use super::pick_root;

    #[test]
    fn picks_the_lowest_root_process() {
        // 10 is a child of 4; 20 is a separate instance whose parent isn't the same app.
        assert_eq!(pick_root(&[(10, 4), (4, 1), (20, 2)]), Some(4));
        assert_eq!(pick_root(&[(30, 4), (20, 2)]), Some(20));
        assert_eq!(pick_root(&[]), None);
    }
}
