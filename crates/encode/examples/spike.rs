//! Phase 1 spike: capture a monitor, convert to NV12 on the GPU, hardware-encode to an MP4, and measure.
//!
//! ```text
//! cargo run --release -p clipr-encode --example spike -- [--seconds 10] [--codec h264|hevc|av1]
//!     [--fps 60] [--bitrate 20000] [--size 1920x1080] [--monitor 0] [--out spike.mp4]
//! ```
//!
//! Run it while a game is on screen to see the cost in a real scene. The frame loop is paced at a constant
//! rate: WGC only delivers frames when something changes, so idle ticks repeat the last frame.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clipr_capture::{CaptureOptions, CaptureSession, CapturedFrame, Target};
use clipr_encode::mf::MfFileEncoder;
use clipr_encode::{Codec, EncodeSettings, VideoEncoder};
use clipr_gpu::{Gpu, Nv12Converter, ScaleMode};
use windows::Win32::Foundation::FILETIME;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

struct Args {
    seconds: u32,
    codec: Codec,
    fps: u32,
    bitrate_kbps: u32,
    size: (u32, u32),
    monitor: Option<usize>,
    out: PathBuf,
}

fn parse_args() -> Result<Args> {
    let mut args = Args {
        seconds: 10,
        codec: Codec::H264,
        fps: 60,
        bitrate_kbps: 20_000,
        size: (1920, 1080),
        monitor: None,
        out: PathBuf::new(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().with_context(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--seconds" => args.seconds = value()?.parse()?,
            "--fps" => args.fps = value()?.parse()?,
            "--bitrate" => args.bitrate_kbps = value()?.parse()?,
            "--monitor" => args.monitor = Some(value()?.parse()?),
            "--out" => args.out = value()?.into(),
            "--codec" => {
                args.codec = match value()?.to_lowercase().as_str() {
                    "h264" => Codec::H264,
                    "hevc" | "h265" => Codec::Hevc,
                    "av1" => Codec::Av1,
                    other => bail!("unknown codec {other}"),
                }
            }
            "--size" => {
                let v = value()?;
                let (w, h) = v.split_once('x').context("--size is WIDTHxHEIGHT")?;
                args.size = (w.parse()?, h.parse()?);
            }
            other => bail!("unknown flag {other}"),
        }
    }
    if args.out.as_os_str().is_empty() {
        args.out = format!("clipr-spike-{:?}.mp4", args.codec).to_lowercase().into();
    }
    Ok(args)
}

/// Process CPU time (user + kernel) so far.
fn cpu_time() -> Duration {
    let (mut created, mut exited, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: valid out-pointers; the pseudo-handle of the current process needs no closing.
    let _ = unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    };
    let ticks = |t: FILETIME| ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64;
    Duration::from_nanos((ticks(kernel) + ticks(user)) * 100)
}

#[derive(Default)]
struct Timing {
    total: Duration,
    max: Duration,
    count: u32,
}

impl Timing {
    fn add(&mut self, d: Duration) {
        self.total += d;
        self.max = self.max.max(d);
        self.count += 1;
    }
    fn report(&self) -> String {
        let avg = self.total.checked_div(self.count.max(1)).unwrap_or_default();
        format!(
            "avg {:.2} ms, max {:.2} ms",
            avg.as_secs_f64() * 1e3,
            self.max.as_secs_f64() * 1e3
        )
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    let args = parse_args()?;

    let monitors = clipr_capture::list_monitors()?;
    let monitor = match args.monitor {
        Some(i) => monitors.get(i).with_context(|| format!("no monitor {i}"))?.handle,
        None => clipr_capture::primary_monitor(),
    };
    let gpu = Gpu::for_monitor(monitor)?;

    let latest: Arc<Mutex<Option<CapturedFrame>>> = Arc::default();
    let captured = Arc::new(AtomicU64::new(0));
    let session = {
        let (latest, captured) = (latest.clone(), captured.clone());
        CaptureSession::start(
            &gpu,
            Target::Monitor(monitor),
            CaptureOptions {
                buffers: 3,
                ..Default::default()
            },
            move |frame| {
                captured.fetch_add(1, Ordering::Relaxed);
                // Keep only the newest; dropping the older one hands its surface back to WGC.
                *latest.lock().unwrap() = Some(frame);
            },
        )?
    };

    let settings = EncodeSettings {
        codec: args.codec,
        width: args.size.0,
        height: args.size.1,
        fps: args.fps,
        bitrate_kbps: args.bitrate_kbps,
        keyframe_interval_secs: 1.0,
    };
    let mut encoder: Box<dyn VideoEncoder> = Box::new(MfFileEncoder::create(&gpu, &args.out, &settings)?);

    let period = Duration::from_secs_f64(1.0 / args.fps as f64);
    let period_100ns = 10_000_000 / args.fps as i64;
    let ticks = args.seconds as u64 * args.fps as u64;
    let mut converter: Option<Nv12Converter> = None;
    let mut last: Option<ID3D11Texture2D> = None;
    let (mut fresh, mut repeated, mut waiting, mut late) = (0u64, 0u64, 0u64, 0u64);
    let (mut convert_t, mut push_t) = (Timing::default(), Timing::default());
    let mut first_tick = None;

    println!(
        "Recording {} s of {:?} at {}x{} {} fps, {} kbps on {} → {}",
        args.seconds,
        args.codec,
        args.size.0,
        args.size.1,
        args.fps,
        args.bitrate_kbps,
        gpu.adapter_name,
        args.out.display()
    );
    let start = Instant::now();
    let cpu_start = cpu_time();

    for tick in 0..ticks {
        let deadline = start + period * tick as u32;
        let now = Instant::now();
        if now < deadline {
            std::thread::sleep(deadline - now);
        } else if now - deadline > period {
            late += 1;
        }
        let frame = latest.lock().unwrap().take();
        if let Some(frame) = frame {
            let size = (frame.width, frame.height);
            if converter.as_ref().map(Nv12Converter::input_size) != Some(size) {
                converter = Some(Nv12Converter::new(
                    &gpu,
                    size,
                    args.size,
                    args.fps,
                    ScaleMode::Fit,
                    8,
                )?);
            }
            let t = Instant::now();
            let nv12 = converter.as_mut().expect("just set").convert(&frame.texture)?;
            convert_t.add(t.elapsed());
            last = Some(nv12);
            fresh += 1;
        } else if last.is_some() {
            repeated += 1;
        } else {
            waiting += 1;
            continue;
        }

        let first = *first_tick.get_or_insert(tick);
        let t = Instant::now();
        encoder.push(
            last.as_ref().expect("checked above"),
            (tick - first) as i64 * period_100ns,
        )?;
        push_t.add(t.elapsed());
    }

    let wall = start.elapsed();
    let cpu = cpu_time() - cpu_start;
    session.stop();
    let t = Instant::now();
    encoder.finish()?;
    let finish = t.elapsed();

    let bytes = std::fs::metadata(&args.out).map(|m| m.len()).unwrap_or(0);
    let encoded_secs = (fresh + repeated) as f64 / args.fps as f64;
    println!(
        "Done in {:.2} s (finalising took {:.0} ms)",
        wall.as_secs_f64(),
        finish.as_secs_f64() * 1e3
    );
    println!(
        "CPU: {:.1}% of one core ({:.2} s CPU over {:.2} s)",
        cpu.as_secs_f64() / wall.as_secs_f64() * 100.0,
        cpu.as_secs_f64(),
        wall.as_secs_f64()
    );
    println!(
        "Frames: {} captured by WGC, {} encoded ({} new, {} repeated), {} ticks before the first frame, {} late ticks",
        captured.load(Ordering::Relaxed),
        fresh + repeated,
        fresh,
        repeated,
        waiting,
        late
    );
    println!("GPU convert: {}", convert_t.report());
    println!("Encoder push: {}", push_t.report());
    println!(
        "File: {} ({:.1} MB, {:.1} Mbps actual)",
        args.out.display(),
        bytes as f64 / 1e6,
        bytes as f64 * 8.0 / encoded_secs.max(0.001) / 1e6
    );
    Ok(())
}
