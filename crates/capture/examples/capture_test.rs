//! Captures a monitor for 5 seconds and prints frame statistics.
//! Usage: capture_test [monitor index]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use clipr_capture::{CaptureOptions, CaptureSession, Target, list_monitors};
use clipr_gpu::Gpu;

fn main() -> Result<()> {
    let monitors = list_monitors()?;
    for (i, m) in monitors.iter().enumerate() {
        println!(
            "monitor {i}: {} {}x{}{}",
            m.name,
            m.rect.right - m.rect.left,
            m.rect.bottom - m.rect.top,
            if m.primary { " (primary)" } else { "" }
        );
    }
    let monitor = match std::env::args().nth(1) {
        Some(arg) => {
            let i: usize = arg.parse().context("monitor index must be a number")?;
            monitors.get(i).context("no such monitor")?.handle
        }
        None => clipr_capture::primary_monitor(),
    };

    let gpu = Gpu::for_monitor(monitor)?;
    println!("GPU: {}", gpu.adapter_name);

    let times = Arc::new(Mutex::new(Vec::<i64>::new()));
    let sizes = Arc::new(Mutex::new((0u32, 0u32)));
    let session = {
        let times = times.clone();
        let sizes = sizes.clone();
        CaptureSession::start(
            &gpu,
            Target::Monitor(monitor),
            CaptureOptions::default(),
            move |frame| {
                *sizes.lock().unwrap() = (frame.width, frame.height);
                times.lock().unwrap().push(frame.time_100ns);
            },
        )?
    };

    std::thread::sleep(Duration::from_secs(5));
    let (w, h) = session.content_size();
    session.stop();

    let times = times.lock().unwrap();
    let count = times.len();
    println!("frames: {count} in 5 s = {:.1} fps", count as f64 / 5.0);
    println!("content size: {w}x{h} (last frame {:?})", *sizes.lock().unwrap());
    if count > 1 {
        let ms: Vec<f64> = times
            .windows(2)
            .map(|p| (p[1] - p[0]) as f64 / 10_000.0)
            .collect();
        let min = ms.iter().copied().fold(f64::MAX, f64::min);
        let max = ms.iter().copied().fold(0.0, f64::max);
        let avg = ms.iter().sum::<f64>() / ms.len() as f64;
        println!("interval ms: min {min:.2} avg {avg:.2} max {max:.2}");
    } else {
        println!("not enough frames for interval statistics (static desktop?)");
    }
    Ok(())
}
