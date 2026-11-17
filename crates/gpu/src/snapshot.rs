//! Small RGB snapshots for the live preview, made from the NV12 frames the encoder gets (so the preview shows
//! exactly what is recorded, and there is always a frame, even on a screen that doesn't change). The GPU
//! scales the frame down and converts it to RGB; the CPU reads the result back a tick later so the video
//! thread never waits on the GPU.

use anyhow::{Context, Result, ensure};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_CPU_ACCESS_READ, D3D11_MAP_FLAG_DO_NOT_WAIT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
    ID3D11DeviceContext, ID3D11Texture2D, ID3D11VideoProcessorInputView, ID3D11VideoProcessorOutputView,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709, DXGI_FORMAT,
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::DXGI_ERROR_WAS_STILL_DRAWING;

use crate::Gpu;
use crate::convert::Processor;

/// One snapshot: `width`×`height` pixels, 4 bytes each (B, G, R, A), rows packed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

/// Scales NV12 frames of one size (the encoder's) to at most `max`, keeping the shape, and reads them back.
///
/// Call [`start`](Self::start) with a frame, then [`take`](Self::take) on later ticks until it returns the
/// snapshot. Recreate it when the input size changes.
pub struct Snapshotter {
    context: ID3D11DeviceContext,
    processor: Processor,
    input_size: (u32, u32),
    size: (u32, u32),
    target: ID3D11Texture2D,
    view: ID3D11VideoProcessorOutputView,
    staging: ID3D11Texture2D,
    pending: bool,
    input_views: Vec<(usize, ID3D11VideoProcessorInputView)>,
}

// SAFETY: all members are D3D11 interfaces on a multithread-protected device.
unsafe impl Send for Snapshotter {}

impl Snapshotter {
    pub fn new(gpu: &Gpu, input_size: (u32, u32), max: (u32, u32)) -> Result<Self> {
        ensure!(
            input_size.0 > 0 && input_size.1 > 0,
            "empty input size {input_size:?}"
        );
        let size = fit_size(input_size, max);
        let target_rect = RECT {
            left: 0,
            top: 0,
            right: size.0 as i32,
            bottom: size.1 as i32,
        };
        let processor = Processor::new(
            gpu,
            input_size,
            size,
            30,
            target_rect,
            DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
            DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
        )?;
        let target = texture(gpu, size, DXGI_FORMAT_B8G8R8A8_UNORM, false)?;
        let view = processor.output_view(&target)?;
        let staging = texture(gpu, size, DXGI_FORMAT_B8G8R8A8_UNORM, true)?;
        Ok(Self {
            context: gpu.context.clone(),
            processor,
            input_size,
            size,
            target,
            view,
            staging,
            pending: false,
            input_views: Vec::new(),
        })
    }

    pub fn input_size(&self) -> (u32, u32) {
        self.input_size
    }

    /// Whether a snapshot is on its way (started, not taken yet).
    pub fn pending(&self) -> bool {
        self.pending
    }

    /// Starts a snapshot of `source` (an NV12 texture of [`input_size`](Self::input_size)).
    pub fn start(&mut self, source: &ID3D11Texture2D) -> Result<()> {
        let input = self.processor.input_view(&mut self.input_views, source)?;
        self.processor.blt(input, &self.view)?;
        // SAFETY: same-sized textures of one format on one device.
        unsafe {
            self.context.CopyResource(&self.staging, &self.target);
            self.context.Flush();
        }
        self.pending = true;
        Ok(())
    }

    /// The started snapshot, once the GPU has finished it (`None` until then, or when none was started).
    pub fn take(&mut self) -> Result<Option<Snapshot>> {
        if !self.pending {
            return Ok(None);
        }
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: a CPU-readable staging texture; unmapped below before anything else touches it.
        let map = unsafe {
            self.context.Map(
                &self.staging,
                0,
                D3D11_MAP_READ,
                D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32,
                Some(&mut mapped),
            )
        };
        match map {
            Err(e) if e.code() == DXGI_ERROR_WAS_STILL_DRAWING => return Ok(None),
            Err(e) => {
                self.pending = false;
                return Err(e).context("can't read the preview back");
            }
            Ok(()) => {}
        }
        let (w, h) = self.size;
        let row = w as usize * 4;
        let pitch = mapped.RowPitch as usize;
        let mut bgra = Vec::with_capacity(row * h as usize);
        // SAFETY: the mapping holds `h` rows of `pitch` bytes, each starting with `row` pixel bytes.
        unsafe {
            let base = mapped.pData as *const u8;
            for y in 0..h as usize {
                bgra.extend_from_slice(std::slice::from_raw_parts(base.add(y * pitch), row));
            }
            self.context.Unmap(&self.staging, 0);
        }
        self.pending = false;
        Ok(Some(Snapshot {
            width: w,
            height: h,
            bgra,
        }))
    }
}

/// `input` scaled down to fit in `max`, keeping its shape (never scaled up), both sides even and at least 2.
pub fn fit_size((iw, ih): (u32, u32), (mw, mh): (u32, u32)) -> (u32, u32) {
    let scale = (f64::from(mw) / f64::from(iw))
        .min(f64::from(mh) / f64::from(ih))
        .min(1.0);
    let even = |v: f64| ((v.round() as u32) & !1).max(2);
    (even(f64::from(iw) * scale), even(f64::from(ih) * scale))
}

fn texture(
    gpu: &Gpu,
    (width, height): (u32, u32),
    format: DXGI_FORMAT,
    staging: bool,
) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: if staging {
            D3D11_USAGE_STAGING
        } else {
            D3D11_USAGE_DEFAULT
        },
        BindFlags: if staging {
            0
        } else {
            D3D11_BIND_RENDER_TARGET.0 as u32
        },
        CPUAccessFlags: if staging {
            D3D11_CPU_ACCESS_READ.0 as u32
        } else {
            0
        },
        MiscFlags: 0,
    };
    let mut texture = None;
    // SAFETY: valid descriptor and out-pointer.
    unsafe { gpu.device.CreateTexture2D(&desc, None, Some(&mut texture)) }
        .context("can't create the preview texture")?;
    texture.context("no preview texture")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_inside_and_keeps_the_shape() {
        assert_eq!(fit_size((1920, 1080), (640, 360)), (640, 360));
        assert_eq!(fit_size((2560, 1440), (640, 360)), (640, 360));
        // Ultrawide: width-limited.
        assert_eq!(fit_size((3440, 1440), (640, 360)), (640, 268));
        // 4:3: height-limited.
        assert_eq!(fit_size((1440, 1080), (640, 360)), (480, 360));
        // Never scaled up; odd sizes become even.
        assert_eq!(fit_size((301, 201), (640, 360)), (300, 200));
    }
}
