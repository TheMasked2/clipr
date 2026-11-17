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
use clipr_encode::EncodeSettings;
use clipr_encode::mf::HwEncoder;
use clipr_gpu::{Gpu, Nv12Converter, ScaleMode, Snapshotter};
use clipr_mux::MediaFormat;
use windows::Win32::Graphics::Direct3D11::{D3D11_TEXTURE2D_DESC, ID3D11Texture2D};
use windows::Win32::Graphics::Gdi::HMONITOR;

use super::Shared;
use crate::preview;

#[derive(Clone, Debug, PartialEq)]
pub struct VideoSettings {
    /// The monitor to capture (an `HMONITOR` value).
    pub monitor: isize,
    pub encode: EncodeSettings,
    /// Height asked for in the settings (0 = native); the width follows the monitor's shape.
    pub height: u32,
    pub high_priority: bool,
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
    if settings.high_priority {
        clipr_gpu::raise_thread_priority();
    }
    let monitor = HMONITOR(settings.monitor as *mut _);
    let gpu = Gpu::for_monitor(monitor)?;
    if settings.high_priority {
        clipr_gpu::raise_gpu_priority(&gpu);
    }
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
    let size = output_size(monitor_size, settings.height);
    let encode = EncodeSettings {
        width: size.0,
        height: size.1,
        ..settings.encode.clone()
    };

    let latest: Arc<Mutex<Option<CapturedFrame>>> = Arc::default();
    let session = {
        let latest = latest.clone();
        CaptureSession::start(
            &gpu,
            Target::Monitor(monitor),
            CaptureOptions {
                buffers: 3,
                ..Default::default()
            },
            move |frame| {
                // Keep only the newest; dropping the older one hands its surface back to WGC.
                *latest.lock().unwrap() = Some(frame);
            },
        )?
    };
    let mut encoder = HwEncoder::new(&gpu, &encode)?;
    tracing::info!(
        "capturing {}x{} → {}x{} {:?} {} fps {} kbps with {}",
        monitor_size.0,
        monitor_size.1,
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

        let frame = latest.lock().unwrap().take();
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
            shared.lock().unwrap().stats.fresh += 1;
        } else if last.is_some() {
            shared.lock().unwrap().stats.repeated += 1;
        } else {
            // Nothing captured yet (WGC sends the first frame when the screen next changes).
            tick += 1;
            continue;
        }
        let frame = last.as_ref().expect("checked above");
        live.tick(&gpu, frame);
        encoder.encode(frame, deadline, &mut on_packet)?;
        tick += 1;
    }

    session.stop();
    encoder.drain(&mut on_packet)?;
    Ok(())
}

/// The live preview's snapshots (see [`crate::preview`]): one every [`preview::INTERVAL`] while a page
/// watches, made from the frame the encoder gets and read back a tick later. If the GPU can't make them, the
/// page is told, capture goes on, and the preview tries again after [`RETRY`].
#[derive(Default)]
struct Preview {
    snapshotter: Option<Snapshotter>,
    next: Option<Instant>,
}

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
        if self.snapshotter.as_ref().map(Snapshotter::input_size) != Some(size) {
            self.snapshotter = Some(Snapshotter::new(gpu, size, preview::MAX_SIZE)?);
        }
        self.snapshotter.as_mut().expect("just set").start(frame)?;
        self.next = Some(now + preview::INTERVAL);
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
