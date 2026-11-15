//! Encodes blank NV12 frames with no capture running, to test the hardware encoder on its own.
//! `cargo run -p clipr-encode --example encoder_only -- [h264|hevc|av1] [frames] [WxH] [fps]`

use anyhow::Result;
use clipr_encode::mf::HwEncoder;
use clipr_encode::{Codec, EncodeSettings};
use clipr_gpu::{Gpu, nv12_texture};

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("debug").init();
    let mut args = std::env::args().skip(1);
    let codec = match args.next().as_deref() {
        Some("hevc") => Codec::Hevc,
        Some("av1") => Codec::Av1,
        _ => Codec::H264,
    };
    let frames: i64 = args.next().and_then(|n| n.parse().ok()).unwrap_or(120);
    let gpu = Gpu::best()?;
    let (width, height) = match args.next().as_deref() {
        Some(v) => {
            let (w, h) = v.split_once('x').unwrap();
            (w.parse()?, h.parse()?)
        }
        None => (1920, 1080),
    };
    let fps: u32 = args.next().and_then(|n| n.parse().ok()).unwrap_or(60);
    let settings = EncodeSettings {
        codec,
        width,
        height,
        fps,
        ..Default::default()
    };
    let texture = nv12_texture(&gpu, (settings.width, settings.height))?;
    let mut encoder = HwEncoder::new(&gpu, &settings)?;
    let (mut packets, mut bytes) = (0u32, 0u64);
    let mut count = |_: &HwEncoder, p: clipr_encode::mf::Packet| -> Result<()> {
        packets += 1;
        // SAFETY: valid sample.
        bytes += unsafe { p.GetTotalLength() }? as u64;
        Ok(())
    };
    for i in 0..frames {
        encoder.encode(&texture, i * 10_000_000 / fps as i64, &mut count)?;
    }
    encoder.drain(&mut count)?;
    println!("{} frames in, {packets} packets out, {bytes} bytes", frames);
    Ok(())
}
