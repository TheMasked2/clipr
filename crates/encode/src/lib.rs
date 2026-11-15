//! Video encoding behind the [`VideoEncoder`] trait.
//!
//! - [`mf`]: Media Foundation hardware encoders (AMD AMF, NVIDIA NVENC, Intel Quick Sync) fed with D3D11
//!   NV12 textures, so frames never leave the GPU.
//! - [`probe`]: lists GPUs and hardware encoders, for the auto preset and for diagnostics.
//!
//! If Media Foundation turns out unworkable for one vendor, that vendor's SDK goes behind the same trait.

#![cfg(windows)]

pub mod mf;
pub mod probe;

use anyhow::Result;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Codec {
    H264,
    Hevc,
    Av1,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EncodeSettings {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    /// Seconds between keyframes. Short (1–2 s) so the replay buffer can cut clips almost anywhere.
    pub keyframe_interval_secs: f32,
}

impl Default for EncodeSettings {
    fn default() -> Self {
        Self {
            codec: Codec::H264,
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_kbps: 20_000,
            keyframe_interval_secs: 1.0,
        }
    }
}

pub trait VideoEncoder {
    /// Encodes one NV12 texture of the configured size. `time_100ns` is the presentation time in 100 ns
    /// units, starting at 0 and increasing.
    fn push(&mut self, nv12: &ID3D11Texture2D, time_100ns: i64) -> Result<()>;

    /// Flushes everything and closes the output.
    fn finish(self: Box<Self>) -> Result<()>;
}
