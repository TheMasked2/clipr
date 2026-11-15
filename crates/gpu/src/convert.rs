//! BGRA → NV12 on the GPU with the D3D11 video processor (fixed-function hardware on every modern GPU),
//! including scaling and BT.709 limited-range conversion, which is what H.264/HEVC/AV1 encoders expect.

use std::mem::ManuallyDrop;

use anyhow::{Context, Result, ensure};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_VIDEO_ENCODER, D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_VIDEO_COLOR, D3D11_VIDEO_COLOR_0,
    D3D11_VIDEO_COLOR_RGBA, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, D3D11_VIDEO_PROCESSOR_CONTENT_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0,
    D3D11_VIDEO_PROCESSOR_STREAM, D3D11_VIDEO_USAGE_OPTIMAL_SPEED, D3D11_VPIV_DIMENSION_TEXTURE2D,
    D3D11_VPOV_DIMENSION_TEXTURE2D, ID3D11DeviceContext, ID3D11Texture2D, ID3D11VideoContext1,
    ID3D11VideoDevice, ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorInputView,
    ID3D11VideoProcessorOutputView,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709, DXGI_FORMAT_NV12,
    DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};
use windows_core::Interface;

use crate::Gpu;

/// How a source that doesn't match the output aspect ratio is fitted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScaleMode {
    /// Keep the aspect ratio, black bars where needed.
    #[default]
    Fit,
    /// Fill the frame, stretching (stretched 4:3 players want this).
    Stretch,
}

/// Converts frames of one input size into a small ring of NV12 textures.
///
/// The encoder may still read a texture after `convert` returns, so outputs are recycled only after
/// `pool` further conversions. Recreate the converter when the input size changes.
pub struct Nv12Converter {
    context: ID3D11DeviceContext,
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext1,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    input_size: (u32, u32),
    output_size: (u32, u32),
    outputs: Vec<(ID3D11Texture2D, ID3D11VideoProcessorOutputView)>,
    next: usize,
    /// Input views for the capture pool's textures, which are reused frame after frame.
    input_views: Vec<(usize, ID3D11VideoProcessorInputView)>,
}

// SAFETY: all members are D3D11 video interfaces on a multithread-protected device.
unsafe impl Send for Nv12Converter {}

impl Nv12Converter {
    pub fn new(
        gpu: &Gpu,
        input_size: (u32, u32),
        output_size: (u32, u32),
        fps: u32,
        mode: ScaleMode,
        pool: usize,
    ) -> Result<Self> {
        ensure!(
            input_size.0 > 0 && input_size.1 > 0,
            "empty input size {input_size:?}"
        );
        ensure!(
            output_size.0 % 2 == 0 && output_size.1 % 2 == 0,
            "NV12 needs an even output size, got {output_size:?}"
        );
        let video_device: ID3D11VideoDevice = gpu.device.cast().context("GPU has no video device")?;
        let video_context: ID3D11VideoContext1 = gpu
            .context
            .cast()
            .context("GPU has no video context (needs D3D 11.1)")?;

        let rate = DXGI_RATIONAL {
            Numerator: fps.max(1),
            Denominator: 1,
        };
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: input_size.0,
            InputHeight: input_size.1,
            OutputFrameRate: rate,
            OutputWidth: output_size.0,
            OutputHeight: output_size.1,
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
        };
        // SAFETY: valid descriptor.
        let enumerator = unsafe { video_device.CreateVideoProcessorEnumerator(&content) }
            .context("GPU can't convert video at this size")?;
        // SAFETY: valid enumerator; rate conversion index 0 always exists.
        let processor = unsafe { video_device.CreateVideoProcessor(&enumerator, 0) }
            .context("can't create the video processor")?;

        let (iw, ih) = (input_size.0 as i32, input_size.1 as i32);
        let source = RECT {
            left: 0,
            top: 0,
            right: iw,
            bottom: ih,
        };
        let target = match mode {
            ScaleMode::Stretch => RECT {
                left: 0,
                top: 0,
                right: output_size.0 as i32,
                bottom: output_size.1 as i32,
            },
            ScaleMode::Fit => fit_rect(input_size, output_size),
        };
        let black = D3D11_VIDEO_COLOR {
            Anonymous: D3D11_VIDEO_COLOR_0 {
                RGBA: D3D11_VIDEO_COLOR_RGBA {
                    R: 0.0,
                    G: 0.0,
                    B: 0.0,
                    A: 1.0,
                },
            },
        };
        // SAFETY: plain state setters on a valid processor; rect/colour pointers outlive the calls.
        unsafe {
            video_context.VideoProcessorSetStreamFrameFormat(
                &processor,
                0,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            );
            video_context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            video_context.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&source));
            video_context.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&target));
            video_context.VideoProcessorSetOutputTargetRect(&processor, false, None);
            video_context.VideoProcessorSetOutputBackgroundColor(&processor, false, &black);
            // Desktop/game frames are full-range sRGB-ish BT.709; encoders want limited-range BT.709 YUV.
            video_context.VideoProcessorSetStreamColorSpace1(
                &processor,
                0,
                DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
            );
            video_context
                .VideoProcessorSetOutputColorSpace1(&processor, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709);
        }

        let mut outputs = Vec::with_capacity(pool.max(2));
        for _ in 0..pool.max(2) {
            let texture = nv12_texture(gpu, output_size)?;
            let view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
                },
            };
            let mut view = None;
            // SAFETY: valid texture/enumerator/descriptor and out-pointer.
            unsafe {
                video_device.CreateVideoProcessorOutputView(
                    &texture,
                    &enumerator,
                    &view_desc,
                    Some(&mut view),
                )
            }
            .context("can't create the NV12 output view")?;
            outputs.push((texture, view.context("no output view")?));
        }

        Ok(Self {
            context: gpu.context.clone(),
            video_device,
            video_context,
            enumerator,
            processor,
            input_size,
            output_size,
            outputs,
            next: 0,
            input_views: Vec::new(),
        })
    }

    pub fn input_size(&self) -> (u32, u32) {
        self.input_size
    }

    pub fn output_size(&self) -> (u32, u32) {
        self.output_size
    }

    /// Converts one BGRA texture of [`input_size`](Self::input_size). Returns the NV12 texture holding the
    /// result; it stays valid until the ring wraps around.
    pub fn convert(&mut self, source: &ID3D11Texture2D) -> Result<ID3D11Texture2D> {
        let input = self.input_view(source)?;
        let (texture, output) = &self.outputs[self.next];
        self.next = (self.next + 1) % self.outputs.len();

        let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            pInputSurface: ManuallyDrop::new(Some(input)),
            ..Default::default()
        };
        // SAFETY: valid processor and views; `stream` lives across the call.
        let result = unsafe {
            self.video_context
                .VideoProcessorBlt(&self.processor, output, 0, std::slice::from_ref(&stream))
        };
        // SAFETY: we put an owned reference in the ManuallyDrop above and release it exactly once.
        unsafe { ManuallyDrop::drop(&mut stream.pInputSurface) };
        result.context("GPU colour conversion failed")?;
        // The encoder reads the texture on its own GPU engine. Without a flush the conversion can sit in
        // the immediate context's queue indefinitely, and a hardware encoder then waits on it forever.
        // SAFETY: valid context.
        unsafe { self.context.Flush() };
        Ok(texture.clone())
    }

    fn input_view(&mut self, source: &ID3D11Texture2D) -> Result<ID3D11VideoProcessorInputView> {
        let key = source.as_raw() as usize;
        if let Some((_, view)) = self.input_views.iter().find(|(k, _)| *k == key) {
            return Ok(view.clone());
        }
        let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV {
                    MipSlice: 0,
                    ArraySlice: 0,
                },
            },
        };
        let mut view = None;
        // SAFETY: valid source/enumerator/descriptor and out-pointer.
        unsafe {
            self.video_device
                .CreateVideoProcessorInputView(source, &self.enumerator, &desc, Some(&mut view))
        }
        .context("can't read the captured frame on the GPU")?;
        let view = view.context("no input view")?;
        // A capture pool has a handful of textures; if we somehow see many, start over.
        if self.input_views.len() >= 16 {
            self.input_views.clear();
        }
        self.input_views.push((key, view.clone()));
        Ok(view)
    }
}

/// A blank NV12 texture usable as encoder input.
pub fn nv12_texture(gpu: &Gpu, (width, height): (u32, u32)) -> Result<ID3D11Texture2D> {
    let mut desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        // VIDEO_ENCODER lets the encoder read it without an internal copy; not every driver allows the combo.
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_VIDEO_ENCODER.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut texture = None;
    // SAFETY: valid descriptor and out-pointer.
    if unsafe { gpu.device.CreateTexture2D(&desc, None, Some(&mut texture)) }.is_err() {
        desc.BindFlags = D3D11_BIND_RENDER_TARGET.0 as u32;
        // SAFETY: as above.
        unsafe { gpu.device.CreateTexture2D(&desc, None, Some(&mut texture)) }
            .context("can't create an NV12 texture")?;
    }
    texture.context("no NV12 texture")
}

/// The largest even-sized rect with the input's aspect ratio, centred in the output.
fn fit_rect((iw, ih): (u32, u32), (ow, oh): (u32, u32)) -> RECT {
    let scale = (ow as f64 / iw as f64).min(oh as f64 / ih as f64);
    let w = ((iw as f64 * scale).round() as u32).min(ow) & !1;
    let h = ((ih as f64 * scale).round() as u32).min(oh) & !1;
    let x = ((ow - w) / 2) & !1;
    let y = ((oh - h) / 2) & !1;
    RECT {
        left: x as i32,
        top: y as i32,
        right: (x + w) as i32,
        bottom: (y + h) as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(r: RECT) -> (i32, i32, i32, i32) {
        (r.left, r.top, r.right, r.bottom)
    }

    #[test]
    fn fit_keeps_aspect_and_centres() {
        // Same aspect: fills.
        assert_eq!(rect(fit_rect((2560, 1440), (1920, 1080))), (0, 0, 1920, 1080));
        // 4:3 into 16:9: pillarbox.
        assert_eq!(rect(fit_rect((1440, 1080), (1920, 1080))), (240, 0, 1680, 1080));
        // Ultrawide into 16:9: letterbox, even edges.
        let (l, t, r, b) = rect(fit_rect((3440, 1440), (1920, 1080)));
        assert_eq!((l, r), (0, 1920));
        assert!(t > 0 && t % 2 == 0 && (b - t) % 2 == 0);
    }
}
