//! Thumbnails and hover-scrub sprites, made with Media Foundation (decode) and WIC (JPEG).
//!
//! Everything here runs on the library's one low-priority worker thread.

use std::path::Path;

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{GENERIC_WRITE, HGLOBAL, RPC_E_CHANGED_MODE};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatJpeg, GUID_WICPixelFormat24bppBGR, IWICBitmapFrameEncode,
    IWICImagingFactory, WICBitmapEncoderNoCache,
};
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFSample, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE,
    MF_PD_DURATION, MF_SOURCE_READER_ALL_STREAMS, MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING,
    MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_SOURCE_READER_MEDIASOURCE, MF_SOURCE_READERF_ENDOFSTREAM,
    MF_VERSION, MFCreateAttributes, MFCreateMediaType, MFCreateSourceReaderFromURL, MFMediaType_Video,
    MFSTARTUP_FULL, MFShutdown, MFStartup, MFVideoFormat_RGB32,
};
use windows::Win32::System::Com::StructuredStorage::{
    CreateStreamOnHGlobal, IPropertyBag2, PROPVARIANT, PropVariantToUInt64,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, IStream,
    STREAM_SEEK_END, STREAM_SEEK_SET,
};
use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL};

use windows::core::{GUID, HSTRING, PCWSTR};

/// Width of the gallery thumbnail.
const THUMB_WIDTH: u32 = 320;
/// Width of one sprite frame.
const SPRITE_WIDTH: u32 = 160;
/// How many frames the reader may decode while seeking to the exact position.
const MAX_READS: usize = 600;

/// File name (in the thumbs folder) of a clip's thumbnail or sprite: a stable hash of the clip id.
pub fn thumb_file_name(id: &str, sprite: bool) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in id.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}{}.jpg", if sprite { ".sprite" } else { "" })
}

/// Lets the worker thread yield to the game that is probably running.
pub(crate) fn lower_priority() {
    // SAFETY: changes the priority of the calling thread only.
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    }
}

/// COM and Media Foundation, started for the lifetime of this value on the current thread.
pub(crate) struct MfSession {
    com: bool,
}

impl MfSession {
    pub fn start() -> Result<Self> {
        // SAFETY: plain initialisation; `Drop` undoes it on the same thread.
        unsafe {
            let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
            if hr.is_err() && hr != RPC_E_CHANGED_MODE {
                bail!("can't start COM: {hr:?}");
            }
            let com = hr.is_ok();
            if let Err(e) = MFStartup(MF_VERSION, MFSTARTUP_FULL) {
                if com {
                    CoUninitialize();
                }
                return Err(e).context("can't start Media Foundation");
            }
            Ok(Self { com })
        }
    }
}

impl Drop for MfSession {
    fn drop(&mut self) {
        // SAFETY: pairs with the calls in `start`.
        unsafe {
            let _ = MFShutdown();
            if self.com {
                CoUninitialize();
            }
        }
    }
}

/// A decoded picture: tightly packed, top-down BGRA.
struct Frame {
    w: u32,
    h: u32,
    bgra: Vec<u8>,
}

/// What [`generate`] produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Made {
    /// Thumbnail and sprite.
    Both,
    /// The duration is unknown, so only the thumbnail.
    ThumbOnly,
}

/// Writes the thumbnail to `thumb_out` and the sprite (10 frames side by side) to `sprite_out`.
pub(crate) fn generate(
    video: &Path,
    thumb_out: &Path,
    sprite_out: &Path,
    duration_hint: Option<f64>,
    sprite_frames: u32,
) -> Result<Made> {
    // SAFETY: Media Foundation is started on this thread by the caller (`MfSession`).
    let (thumb, sprite) = unsafe { decode(video, duration_hint, sprite_frames)? };
    let thumb = scale(&thumb, THUMB_WIDTH);
    write_jpeg(thumb_out, thumb.w, thumb.h, &to_bgr(&thumb.bgra))?;
    if sprite.is_empty() {
        return Ok(Made::ThumbOnly);
    }
    let frames: Vec<Frame> = sprite.iter().map(|f| scale(f, SPRITE_WIDTH)).collect();
    let (fw, fh) = (frames[0].w, frames[0].h);
    let total_w = fw * frames.len() as u32;
    let mut canvas = vec![0u8; (total_w * fh * 4) as usize];
    for (i, f) in frames.iter().enumerate() {
        for y in 0..fh.min(f.h) as usize {
            let w = fw.min(f.w) as usize * 4;
            let src = y * f.w as usize * 4;
            let dst = (y * total_w as usize + i * fw as usize) * 4;
            canvas[dst..dst + w].copy_from_slice(&f.bgra[src..src + w]);
        }
    }
    write_jpeg(sprite_out, total_w, fh, &to_bgr(&canvas))?;
    Ok(Made::Both)
}

/// Decodes the frame near 1 s and `sprite_frames` evenly spaced ones.
unsafe fn decode(
    video: &Path,
    duration_hint: Option<f64>,
    sprite_frames: u32,
) -> Result<(Frame, Vec<Frame>)> {
    const VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
    // SAFETY: COM calls on interfaces created right here; buffers are locked only while read.
    unsafe {
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 1)?;
        let attrs = attrs.context("no attributes")?;
        attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;
        let url = HSTRING::from(video.to_string_lossy().as_ref());
        let reader = MFCreateSourceReaderFromURL(PCWSTR(url.as_ptr()), &attrs)
            .context("Media Foundation can't open the video")?;
        reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
        reader.SetStreamSelection(VIDEO, true)?;
        let wanted = MFCreateMediaType()?;
        wanted.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        wanted.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
        reader.SetCurrentMediaType(VIDEO, None, &wanted)?;
        let current = reader.GetCurrentMediaType(VIDEO)?;
        let size = current.GetUINT64(&MF_MT_FRAME_SIZE)?;
        let (w, h) = ((size >> 32) as u32, size as u32);
        if w == 0 || h == 0 {
            bail!("the video has no size");
        }
        let stride = current
            .GetUINT32(&MF_MT_DEFAULT_STRIDE)
            .map_or(w as i32 * 4, |s| s as i32);

        let duration = duration_hint.filter(|d| *d > 0.0).or_else(|| {
            let pv = reader
                .GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
                .ok()?;
            let hns = PropVariantToUInt64(&pv).ok()?;
            Some(hns as f64 / 1e7)
        });

        let grab = |secs: f64| -> Result<Frame> {
            let target = (secs.max(0.0) * 1e7) as i64;
            let pos = PROPVARIANT::from(target);
            reader.SetCurrentPosition(&GUID::zeroed(), &pos)?;
            let mut last: Option<IMFSample> = None;
            for _ in 0..MAX_READS {
                let mut flags = 0u32;
                let mut ts = 0i64;
                let mut sample: Option<IMFSample> = None;
                reader.ReadSample(
                    VIDEO,
                    0,
                    None,
                    Some(&mut flags as *mut u32),
                    Some(&mut ts as *mut i64),
                    Some(&mut sample as *mut Option<IMFSample>),
                )?;
                if let Some(s) = sample {
                    let reached = ts + 5_000_000 / 60 >= target;
                    last = Some(s);
                    if reached {
                        break;
                    }
                }
                if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                    break;
                }
            }
            let sample = last.context("no frame at that position")?;
            read_frame(&sample, w, h, stride)
        };

        let thumb_at = match duration {
            Some(d) if d < 2.0 => d / 2.0,
            _ => 1.0,
        };
        let thumb = grab(thumb_at)?;
        let mut sprite = Vec::new();
        if let Some(d) = duration {
            for i in 0..sprite_frames {
                let t = d * (f64::from(i) + 0.5) / f64::from(sprite_frames);
                match grab(t) {
                    Ok(f) => sprite.push(f),
                    Err(e) => match sprite.last() {
                        // Keep the strip complete by repeating the previous picture.
                        Some(prev) => sprite.push(Frame {
                            w: prev.w,
                            h: prev.h,
                            bgra: prev.bgra.clone(),
                        }),
                        None => return Err(e),
                    },
                }
            }
        }
        Ok((thumb, sprite))
    }
}

unsafe fn read_frame(sample: &IMFSample, w: u32, h: u32, stride: i32) -> Result<Frame> {
    // SAFETY: the buffer pointer is valid between `Lock` and `Unlock`, for `len` bytes.
    unsafe {
        let buffer = sample.ConvertToContiguousBuffer()?;
        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut len = 0u32;
        buffer.Lock(&mut ptr, None, Some(&mut len as *mut u32))?;
        let data = std::slice::from_raw_parts(ptr, len as usize);
        let abs = stride.unsigned_abs() as usize;
        let row_bytes = w as usize * 4;
        let result = if abs < row_bytes || data.len() < abs * (h as usize - 1) + row_bytes {
            Err(anyhow::anyhow!("the decoded frame is smaller than expected"))
        } else {
            let mut bgra = vec![0u8; row_bytes * h as usize];
            for y in 0..h as usize {
                // A negative stride means the rows are stored bottom-up.
                let src_row = if stride >= 0 { y } else { h as usize - 1 - y };
                let src = &data[src_row * abs..src_row * abs + row_bytes];
                bgra[y * row_bytes..(y + 1) * row_bytes].copy_from_slice(src);
            }
            Ok(Frame { w, h, bgra })
        };
        let _ = buffer.Unlock();
        result
    }
}

/// Box-filter downscale to `width` (never upscales).
fn scale(src: &Frame, width: u32) -> Frame {
    if src.w <= width {
        return Frame {
            w: src.w,
            h: src.h,
            bgra: src.bgra.clone(),
        };
    }
    let dw = width as usize;
    let dh =
        ((u64::from(src.h) * u64::from(width) + u64::from(src.w) / 2) / u64::from(src.w)).max(1) as usize;
    let (sw, sh) = (src.w as usize, src.h as usize);
    let mut out = vec![0u8; dw * dh * 4];
    for y in 0..dh {
        let y0 = y * sh / dh;
        let y1 = ((y + 1) * sh / dh).max(y0 + 1).min(sh);
        for x in 0..dw {
            let x0 = x * sw / dw;
            let x1 = ((x + 1) * sw / dw).max(x0 + 1).min(sw);
            let mut acc = [0u32; 3];
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let p = (sy * sw + sx) * 4;
                    acc[0] += u32::from(src.bgra[p]);
                    acc[1] += u32::from(src.bgra[p + 1]);
                    acc[2] += u32::from(src.bgra[p + 2]);
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            let o = (y * dw + x) * 4;
            out[o] = (acc[0] / n) as u8;
            out[o + 1] = (acc[1] / n) as u8;
            out[o + 2] = (acc[2] / n) as u8;
            out[o + 3] = 255;
        }
    }
    Frame {
        w: dw as u32,
        h: dh as u32,
        bgra: out,
    }
}

fn to_bgr(bgra: &[u8]) -> Vec<u8> {
    bgra.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect()
}

fn write_jpeg(out: &Path, w: u32, h: u32, bgr: &[u8]) -> Result<()> {
    let mut tmp = out.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    // SAFETY: COM calls on interfaces created here; `bgr` outlives the `WritePixels` call.
    let written = unsafe {
        (|| -> Result<()> {
            let factory: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let stream = factory.CreateStream()?;
            stream.InitializeFromFilename(&HSTRING::from(tmp.to_string_lossy().as_ref()), GENERIC_WRITE.0)?;
            encode_jpeg(&factory, &stream, w, h, bgr)
        })()
    };
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).context("can't write the JPEG");
    }
    std::fs::rename(&tmp, out).with_context(|| format!("can't write {}", out.display()))
}

/// Encodes `bgr` (24-bit, `w`×`h`) as a JPEG into `stream`.
///
/// # Safety
/// COM must be running on this thread.
unsafe fn encode_jpeg(
    factory: &IWICImagingFactory,
    stream: &IStream,
    w: u32,
    h: u32,
    bgr: &[u8],
) -> Result<()> {
    // SAFETY: COM calls on interfaces created here; `bgr` outlives the `WritePixels` call.
    unsafe {
        let encoder = factory.CreateEncoder(&GUID_ContainerFormatJpeg, std::ptr::null())?;
        encoder.Initialize(stream, WICBitmapEncoderNoCache)?;
        let mut frame: Option<IWICBitmapFrameEncode> = None;
        let mut bag: Option<IPropertyBag2> = None;
        encoder.CreateNewFrame(&mut frame, &mut bag)?;
        let frame = frame.context("no JPEG frame")?;
        frame.Initialize(bag.as_ref())?;
        frame.SetSize(w, h)?;
        let mut format = GUID_WICPixelFormat24bppBGR;
        frame.SetPixelFormat(&mut format)?;
        frame.WritePixels(h, w * 3, bgr)?;
        frame.Commit()?;
        encoder.Commit()?;
    }
    Ok(())
}

/// `bgra` (`w`×`h`, 4 bytes a pixel, rows packed) as a JPEG file in memory. Starts COM on this thread when
/// it isn't running yet, so any thread can call it.
pub fn jpeg(w: u32, h: u32, bgra: &[u8]) -> Result<Vec<u8>> {
    anyhow::ensure!(
        w > 0 && h > 0 && bgra.len() == w as usize * h as usize * 4,
        "a {w}x{h} image needs {} bytes, got {}",
        w as usize * h as usize * 4,
        bgra.len()
    );
    let bgr = to_bgr(bgra);
    // SAFETY: plain initialisation, undone below on the same thread when it started COM.
    let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if hr.is_err() && hr != RPC_E_CHANGED_MODE {
        bail!("can't start COM: {hr:?}");
    }
    // SAFETY: COM runs on this thread; the stream grows its own memory, and `out` has room for each read.
    let written = unsafe {
        (|| -> Result<Vec<u8>> {
            let factory: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let stream = CreateStreamOnHGlobal(HGLOBAL::default(), true)?;
            encode_jpeg(&factory, &stream, w, h, &bgr)?;
            let mut len = 0u64;
            stream.Seek(0, STREAM_SEEK_END, Some(&mut len))?;
            stream.Seek(0, STREAM_SEEK_SET, None)?;
            let mut out = vec![0u8; usize::try_from(len)?];
            let mut read = 0u32;
            stream
                .Read(out.as_mut_ptr().cast(), u32::try_from(len)?, Some(&mut read))
                .ok()?;
            out.truncate(read as usize);
            Ok(out)
        })()
    };
    if hr.is_ok() {
        // SAFETY: balances the successful CoInitializeEx above.
        unsafe { CoUninitialize() };
    }
    written.context("can't make the JPEG")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jpeg_in_memory() {
        let (w, h) = (64, 36);
        let bgra: Vec<u8> = (0..w * h).flat_map(|i| [(i % 256) as u8, 80, 200, 255]).collect();
        let j = jpeg(w, h, &bgra).unwrap();
        assert_eq!(&j[..2], [0xFF, 0xD8]);
        assert_eq!(&j[j.len() - 2..], [0xFF, 0xD9]);
        assert!(jpeg(w, h, &bgra[4..]).is_err());
    }

    #[test]
    fn names_are_stable_and_distinct() {
        let a = thumb_file_name("Game/a.mp4", false);
        assert_eq!(a, thumb_file_name("Game/a.mp4", false));
        assert_ne!(a, thumb_file_name("Game/b.mp4", false));
        assert!(thumb_file_name("x", true).ends_with(".sprite.jpg"));
    }

    #[test]
    fn scale_keeps_aspect() {
        let f = Frame {
            w: 640,
            h: 360,
            bgra: vec![200; 640 * 360 * 4],
        };
        let s = scale(&f, 160);
        assert_eq!((s.w, s.h), (160, 90));
        assert!(s.bgra.iter().step_by(4).all(|&v| v == 200));
    }
}
