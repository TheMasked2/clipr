//! The auto preset: what this PC can record well, found by a short encoder benchmark.
//!
//! It times the hardware encoder on blank frames at the monitor's resolution. If that's too slow for 60 fps
//! with plenty of headroom, it tries smaller sizes. The bitrate leans high (clips are short, quality
//! matters more than size), scaled by pixel rate.

use std::time::Instant;

use anyhow::Result;
use clipr_encode::mf::HwEncoder;
use clipr_encode::{Codec, EncodeSettings};
use clipr_gpu::{Gpu, nv12_texture};

/// The result, for the settings page and the first-run wizard.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Preset {
    pub gpu: String,
    pub encoder: String,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    /// Encoder time per frame at the chosen size, in ms.
    pub frame_ms: f64,
}

/// Runs the benchmark (about a second). Call it off the UI thread.
pub fn detect() -> Result<Preset> {
    // Media Foundation's encoder activation needs COM on this thread.
    // SAFETY: plain call; an already-initialised apartment is fine.
    let _ = unsafe {
        windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED)
    };
    let monitor = clipr_capture::primary_monitor();
    let gpu = Gpu::for_monitor(monitor)?;
    let native = clipr_capture::list_monitors()?
        .into_iter()
        .find(|m| m.handle == monitor)
        .map(|m| {
            (
                (m.rect.right - m.rect.left) as u32,
                (m.rect.bottom - m.rect.top) as u32,
            )
        })
        .unwrap_or((1920, 1080));
    // Native up to 1440p, then smaller until the encoder has room to spare.
    let mut heights: Vec<u32> = [native.1.min(1440), 1080, 720]
        .into_iter()
        .filter(|&h| h <= native.1)
        .collect();
    heights.dedup();
    let mut last = None;
    for height in heights {
        let size = super::video::output_size(native, height);
        let (ms, name) = time_encoder(&gpu, size)?;
        tracing::info!("preset benchmark: {}x{} {ms:.2} ms per frame", size.0, size.1);
        let preset = Preset {
            gpu: gpu.adapter_name.clone(),
            encoder: name,
            height: if size.1 == native.1 { 0 } else { size.1 },
            fps: 60,
            bitrate_kbps: bitrate_for(size, 60),
            frame_ms: ms,
        };
        // This is one frame's round trip, not throughput (the encoder pipelines frames). 60 fps leaves
        // 16.7 ms a frame; demand half of that so a busy GPU still keeps up.
        if ms < 8.0 {
            return Ok(preset);
        }
        last = Some(preset);
    }
    let mut preset = last.ok_or_else(|| anyhow::anyhow!("no size to test"))?;
    preset.fps = if preset.frame_ms < 14.0 { 60 } else { 30 };
    preset.bitrate_kbps = bitrate_for(super::video::output_size(native, preset.height), preset.fps);
    Ok(preset)
}

/// About 0.16 bits per pixel per frame for H.264: 1080p60 ≈ 20 Mbit/s, 1440p60 ≈ 35, 720p60 ≈ 9.
fn bitrate_for((w, h): (u32, u32), fps: u32) -> u32 {
    let bps = f64::from(w) * f64::from(h) * f64::from(fps) * 0.16;
    ((bps / 1000.0 / 1000.0).round() as u32 * 1000).clamp(6_000, 60_000)
}

fn time_encoder(gpu: &Gpu, size: (u32, u32)) -> Result<(f64, String)> {
    let settings = EncodeSettings {
        codec: Codec::H264,
        width: size.0,
        height: size.1,
        fps: 60,
        bitrate_kbps: bitrate_for(size, 60),
        keyframe_interval_secs: 1.0,
    };
    let texture = nv12_texture(gpu, size)?;
    let mut encoder = HwEncoder::new(gpu, &settings)?;
    let mut sink = |_: &HwEncoder, _| Ok(());
    // Warm up, then time.
    for i in 0..10 {
        encoder.encode(&texture, i * 166_667, &mut sink)?;
    }
    let frames = 60;
    let start = Instant::now();
    for i in 10..10 + frames {
        encoder.encode(&texture, i * 166_667, &mut sink)?;
    }
    encoder.drain(&mut sink)?;
    let ms = start.elapsed().as_secs_f64() * 1000.0 / frames as f64;
    Ok((ms, encoder.name().to_string()))
}

#[cfg(test)]
mod tests {
    use super::bitrate_for;

    #[test]
    fn bitrate_scales_with_pixels() {
        assert_eq!(bitrate_for((1920, 1080), 60), 20_000);
        assert_eq!(bitrate_for((2560, 1440), 60), 35_000);
        assert_eq!(bitrate_for((1280, 720), 30), 6_000);
    }
}
