//! Writing clips and recordings to disk without re-encoding, plus lossless trim.
//!
//! - [`write_clip`]: a [`Cut`] from the replay buffer → a regular MP4.
//! - [`LiveWriter`]: a full recording, written as fragmented MP4 while it runs, so a crash keeps
//!   everything up to the last fragment.
//! - [`trim`]: copies a time range of an MP4 into a new one, starting at the keyframe before the start.
//! - [`shrink`]: the one re-encode: a smaller copy that fits an upload limit (Discord).
//!
//! The first three use Media Foundation's Sink Writer in passthrough mode (the input type equals the output type,
//! so no encoder is inserted). [`MediaFormat`] carries a stream's media type between threads.

#![cfg(windows)]

mod regular;

pub use regular::{is_fragmented, make_regular};

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clipr_buffer::{Cut, Packet};
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFMediaType, IMFSample, IMFSinkWriter, IMFSourceReader, MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
    MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
    MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS,
    MF_SINK_WRITER_DISABLE_THROTTLING, MF_SOURCE_READER_ALL_STREAMS, MF_SOURCE_READER_ANY_STREAM,
    MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, MF_SOURCE_READERF_ENDOFSTREAM,
    MF_TRANSCODE_CONTAINERTYPE, MF_VERSION, MFCreateAttributes, MFCreateMediaType, MFCreateMemoryBuffer,
    MFCreateSample, MFCreateSinkWriterFromURL, MFCreateSourceReaderFromURL, MFGetAttributesAsBlob,
    MFGetAttributesAsBlobSize, MFInitAttributesFromBlob, MFMediaType_Audio, MFMediaType_Video,
    MFSTARTUP_FULL, MFSampleExtension_CleanPoint, MFShutdown, MFStartup, MFTranscodeContainerType_FMPEG4,
    MFTranscodeContainerType_MPEG4, MFVideoFormat_H264, MFVideoFormat_NV12, MFVideoInterlace_Progressive,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::core::{GUID, HSTRING};

/// A stream's media type as bytes, so it can cross threads and outlive the encoder that made it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaFormat(Vec<u8>);

impl MediaFormat {
    pub fn from_type(media_type: &IMFMediaType) -> Result<Self> {
        // SAFETY: valid attribute store; the buffer has the size MF asked for.
        unsafe {
            let size = MFGetAttributesAsBlobSize(media_type)?;
            let mut blob = vec![0u8; size as usize];
            MFGetAttributesAsBlob(media_type, &mut blob)?;
            Ok(Self(blob))
        }
    }

    pub fn to_type(&self) -> Result<IMFMediaType> {
        // SAFETY: a fresh media type initialised from a blob MF wrote.
        unsafe {
            let t = MFCreateMediaType()?;
            MFInitAttributesFromBlob(&t, &self.0)?;
            Ok(t)
        }
    }
}

/// Keeps COM and Media Foundation started for the current thread while it lives.
struct Mf {
    /// Whether our `CoInitializeEx` counted (S_OK or S_FALSE) and needs a `CoUninitialize`.
    com: bool,
}

impl Mf {
    fn start() -> Result<Self> {
        // SAFETY: plain calls; an already-initialised apartment returns S_FALSE or RPC_E_CHANGED_MODE,
        // both fine for us (MF objects are free-threaded).
        let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
        // SAFETY: as above.
        if let Err(e) = unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) } {
            if com {
                // SAFETY: balances the successful CoInitializeEx above.
                unsafe { CoUninitialize() };
            }
            return Err(anyhow::Error::from(e).context("can't start Media Foundation"));
        }
        Ok(Self { com })
    }
}

impl Drop for Mf {
    fn drop(&mut self) {
        // SAFETY: balances MFStartup, then the CoInitializeEx that counted, so a thread that runs many trims
        // or posts doesn't pile up COM references.
        unsafe {
            let _ = MFShutdown();
            if self.com {
                CoUninitialize();
            }
        }
    }
}

/// Copies an encoder's output sample into a [`Packet`] for the replay buffer.
pub fn packet_from_sample(sample: &IMFSample) -> Result<Packet> {
    // SAFETY: valid sample; the locked buffer is read within the lock.
    unsafe {
        let pts = sample.GetSampleTime().unwrap_or(0);
        let duration = sample.GetSampleDuration().unwrap_or(0);
        let keyframe = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0;
        let buffer = sample.ConvertToContiguousBuffer()?;
        let mut ptr = std::ptr::null_mut();
        let mut len = 0u32;
        buffer.Lock(&mut ptr, None, Some(&mut len))?;
        let data: std::sync::Arc<[u8]> = std::slice::from_raw_parts(ptr, len as usize).into();
        buffer.Unlock()?;
        Ok(Packet {
            pts,
            duration,
            keyframe,
            data,
        })
    }
}

fn sample_from_packet(packet: &Packet, offset: i64) -> Result<IMFSample> {
    // SAFETY: fresh MF objects; the copy stays inside the locked buffer's length.
    unsafe {
        let len = packet.data.len() as u32;
        let buffer = MFCreateMemoryBuffer(len.max(1))?;
        let mut ptr = std::ptr::null_mut();
        buffer.Lock(&mut ptr, None, None)?;
        std::ptr::copy_nonoverlapping(packet.data.as_ptr(), ptr, packet.data.len());
        buffer.Unlock()?;
        buffer.SetCurrentLength(len)?;
        let sample = MFCreateSample()?;
        sample.AddBuffer(&buffer)?;
        sample.SetSampleTime(packet.pts - offset)?;
        sample.SetSampleDuration(packet.duration)?;
        if packet.keyframe {
            sample.SetUINT32(&MFSampleExtension_CleanPoint, 1)?;
        }
        Ok(sample)
    }
}

/// A Sink Writer with passthrough streams. Writes go to a `.part` file that becomes the real one on finish.
struct Writer {
    writer: IMFSinkWriter,
    part: PathBuf,
    path: PathBuf,
    streams: Vec<u32>,
}

impl Writer {
    fn create(path: &Path, container: GUID, formats: &[&MediaFormat]) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("can't create {}", dir.display()))?;
        }
        let mut part = path.as_os_str().to_owned();
        part.push(".part");
        let part = PathBuf::from(part);
        // SAFETY: valid attributes and types; same type in and out makes a passthrough stream.
        unsafe {
            let mut attrs: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attrs, 3)?;
            let attrs = attrs.context("no attributes")?;
            attrs.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &container)?;
            attrs.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;
            attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 0)?;
            let writer = MFCreateSinkWriterFromURL(&HSTRING::from(part.as_os_str()), None, &attrs)
                .with_context(|| format!("can't create {}", path.display()))?;
            let mut streams = Vec::new();
            for format in formats {
                let t = format.to_type()?;
                let stream = writer
                    .AddStream(&t)
                    .context("the MP4 writer doesn't accept this format")?;
                writer.SetInputMediaType(stream, &t, None)?;
                streams.push(stream);
            }
            writer
                .BeginWriting()
                .context("can't start writing the video file")?;
            Ok(Self {
                writer,
                part,
                path: path.to_owned(),
                streams,
            })
        }
    }

    fn write(&self, stream: usize, sample: &IMFSample) -> Result<()> {
        // SAFETY: valid writer, stream index and sample.
        unsafe { self.writer.WriteSample(self.streams[stream], sample) }
            .context("can't write to the video file")
    }

    fn finish(self) -> Result<PathBuf> {
        // SAFETY: valid writer; writes the index and closes the file.
        unsafe { self.writer.Finalize() }.context("can't finish the video file")?;
        drop(self.writer);
        std::fs::rename(&self.part, &self.path)
            .with_context(|| format!("can't move the finished file to {}", self.path.display()))?;
        Ok(self.path)
    }
}

/// Writes `cut` to an MP4 at `path`, with time 0 at the first video packet.
pub fn write_clip(path: &Path, video: &MediaFormat, audio: &[MediaFormat], cut: &Cut) -> Result<()> {
    let Some(start) = cut.start() else {
        bail!("the replay buffer is empty");
    };
    let _mf = Mf::start()?;
    let formats: Vec<&MediaFormat> = std::iter::once(video).chain(audio).collect();
    let writer = Writer::create(path, MFTranscodeContainerType_MPEG4, &formats)?;
    // Stream 0 is video, stream n + 1 is audio track n. Interleave by time so the file plays smoothly.
    let mut streams: Vec<&[Packet]> = vec![&cut.video];
    for track in 0..audio.len() {
        streams.push(cut.audio.get(track).map_or(&[][..], Vec::as_slice));
    }
    let mut next = vec![0usize; streams.len()];
    loop {
        let pick = (0..streams.len())
            .filter(|&s| next[s] < streams[s].len())
            .min_by_key(|&s| (streams[s][next[s]].pts, s));
        let Some(s) = pick else { break };
        let p = &streams[s][next[s]];
        next[s] += 1;
        if p.pts < start {
            continue; // audio from before the first picture
        }
        writer.write(s, &sample_from_packet(p, start)?)?;
    }
    writer.finish()?;
    Ok(())
}

/// A full recording, written as fragmented MP4 while it runs.
pub struct LiveWriter {
    writer: Writer,
    audio_tracks: usize,
    start: Option<i64>,
    _mf: Mf,
}

// SAFETY: the Sink Writer is free-threaded; the writer is only used by one thread at a time (&mut self).
unsafe impl Send for LiveWriter {}

impl LiveWriter {
    pub fn create(path: &Path, video: &MediaFormat, audio: &[MediaFormat]) -> Result<Self> {
        let mf = Mf::start()?;
        let formats: Vec<&MediaFormat> = std::iter::once(video).chain(audio).collect();
        Ok(Self {
            writer: Writer::create(path, MFTranscodeContainerType_FMPEG4, &formats)?,
            audio_tracks: audio.len(),
            start: None,
            _mf: mf,
        })
    }

    /// The first video packet written must be a keyframe; earlier ones are skipped.
    pub fn write_video(&mut self, packet: &Packet) -> Result<()> {
        if self.start.is_none() {
            if !packet.keyframe {
                return Ok(());
            }
            self.start = Some(packet.pts);
        }
        let start = self.start.expect("set above");
        self.writer.write(0, &sample_from_packet(packet, start)?)
    }

    pub fn write_audio(&mut self, track: usize, packet: &Packet) -> Result<()> {
        match self.start {
            Some(start) if track < self.audio_tracks && packet.pts >= start => {
                self.writer.write(track + 1, &sample_from_packet(packet, start)?)
            }
            _ => Ok(()),
        }
    }

    /// Seconds written so far (video).
    pub fn started(&self) -> bool {
        self.start.is_some()
    }

    pub fn finish(self) -> Result<PathBuf> {
        if self.start.is_none() {
            bail!("the recording has no video");
        }
        self.writer.finish()
    }
}

/// Copies `start_secs..end_secs` of the MP4 `src` into `dst` without re-encoding. The copy starts at the
/// keyframe at or before `start_secs`. `dst` may equal `src` (the original is replaced at the end).
pub fn trim(src: &Path, dst: &Path, start_secs: f64, end_secs: f64) -> Result<()> {
    if end_secs <= start_secs {
        bail!("the end must be after the start");
    }
    let _mf = Mf::start()?;
    // SAFETY: plain MF calls on objects we create; PROPVARIANT is built and dropped by windows-rs.
    unsafe {
        let reader: IMFSourceReader = MFCreateSourceReaderFromURL(&HSTRING::from(src.as_os_str()), None)
            .with_context(|| format!("can't open {}", src.display()))?;
        reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
        // Stream order in the new file: the first video stream, then the first audio stream.
        let mut picks: Vec<(u32, MediaFormat, bool)> = Vec::new();
        let mut have_video = false;
        let mut index = 0u32;
        while let Ok(t) = reader.GetNativeMediaType(index, 0) {
            let major = t.GetGUID(&MF_MT_MAJOR_TYPE)?;
            let video = major == MFMediaType_Video;
            let audio = major == MFMediaType_Audio;
            // Every audio track (clips can carry up to 6), but only the first video stream.
            if (video && !have_video) || audio {
                reader.SetStreamSelection(index, true)?;
                reader.SetCurrentMediaType(index, None, &t)?;
                picks.push((index, MediaFormat::from_type(&t)?, video));
                have_video |= video;
            }
            index += 1;
        }
        picks.sort_by_key(|p| !p.2);
        if !picks.first().is_some_and(|p| p.2) {
            bail!("{} has no video", src.display());
        }
        let start = (start_secs * 1e7) as i64;
        let end = (end_secs * 1e7) as i64;
        // A seek lands video on the keyframe at or before `start`, but audio exactly on `start`. Find that
        // keyframe, then seek to it, so every stream starts at the same moment.
        reader.SetCurrentPosition(&GUID::zeroed(), &PROPVARIANT::from(start))?;
        let offset = first_sample_time(&reader, picks[0].0)?.unwrap_or(start);
        reader.SetCurrentPosition(&GUID::zeroed(), &PROPVARIANT::from(offset))?;

        let formats: Vec<&MediaFormat> = picks.iter().map(|p| &p.1).collect();
        let writer = Writer::create(dst, MFTranscodeContainerType_MPEG4, &formats)?;
        let mut done = vec![false; picks.len()];
        while done.iter().any(|d| !d) {
            let (mut actual, mut flags, mut time) = (0u32, 0u32, 0i64);
            let mut sample = None;
            reader.ReadSample(
                MF_SOURCE_READER_ANY_STREAM.0 as u32,
                0,
                Some(&mut actual),
                Some(&mut flags),
                Some(&mut time),
                Some(&mut sample),
            )?;
            let Some(slot) = picks.iter().position(|p| p.0 == actual) else {
                continue;
            };
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                done[slot] = true;
                continue;
            }
            let Some(sample) = sample else { continue };
            if time >= end {
                done[slot] = true;
                continue;
            }
            if time < offset {
                continue;
            }
            // A fresh sample with just time, duration and data: the reader's samples carry seek attributes
            // (a discontinuity) that make the MP4 sink stretch the audio track's duration.
            let mut packet = packet_from_sample(&sample)?;
            packet.pts = time;
            writer.write(slot, &sample_from_packet(&packet, offset)?)?;
        }
        drop(reader);
        writer.finish()?;
    }
    Ok(())
}

/// The time of the next sample of `stream`, or `None` at its end.
///
/// # Safety
/// `reader` must be a valid source reader with `stream` selected.
unsafe fn first_sample_time(reader: &IMFSourceReader, stream: u32) -> Result<Option<i64>> {
    loop {
        let (mut flags, mut time) = (0u32, 0i64);
        let mut sample = None;
        // SAFETY: valid reader and out-pointers (see the function's contract).
        unsafe {
            reader.ReadSample(
                stream,
                0,
                None,
                Some(&mut flags),
                Some(&mut time),
                Some(&mut sample),
            )?
        };
        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
            return Ok(None);
        }
        if sample.is_some() {
            return Ok(Some(time));
        }
    }
}

/// Whether a media type is video (for callers that hold several).
pub fn is_video(format: &MediaFormat) -> bool {
    format
        .to_type()
        // SAFETY: valid media type.
        .and_then(|t| unsafe { Ok(t.GetGUID(&MF_MT_MAJOR_TYPE)?) })
        .is_ok_and(|g| g == MFMediaType_Video)
}

/// The plan for a copy that fits under a size limit: the video bitrate left after the audio, and the
/// height to scale to (lower bitrates look better at a lower resolution).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShrinkPlan {
    pub video_bps: u32,
    pub height: u32,
}

/// Lowest video bitrate worth sending; below it the clip is too long for the limit.
const MIN_VIDEO_BPS: f64 = 200_000.0;

/// Works out the bitrate and height for a `duration_secs` clip to fit in `max_bytes`, keeping 6 % for the
/// container and the encoder's overshoot. `None` when even the lowest quality wouldn't fit.
pub fn shrink_plan(
    max_bytes: u64,
    duration_secs: f64,
    audio_bps: u32,
    source_height: u32,
) -> Option<ShrinkPlan> {
    if duration_secs <= 0.0 {
        return None;
    }
    let budget_bps = (max_bytes as f64 * 8.0 * 0.94) / duration_secs;
    let video_bps = budget_bps - f64::from(audio_bps);
    if video_bps < MIN_VIDEO_BPS {
        return None;
    }
    let video_bps = video_bps.min(20_000_000.0) as u32;
    let wanted = match video_bps {
        0..=899_999 => 480,
        900_000..=2_499_999 => 720,
        _ => 1080,
    };
    Some(ShrinkPlan {
        video_bps,
        height: wanted.min(source_height.max(2)),
    })
}

/// Re-encodes the MP4 `src` into `dst` so it fits in `max_bytes`: H.264 at the bitrate [`shrink_plan`]
/// picks, scaled down to its height, with only the first audio track (the full mix) copied as it is.
///
/// Uses the software encoder that comes with Windows: a 30 s 1080p60 clip takes about 2 s, and it can't
/// stall the way a hardware encoder without a D3D device does.
pub fn shrink(src: &Path, dst: &Path, max_bytes: u64, duration_secs: f64) -> Result<()> {
    let _mf = Mf::start()?;
    let result = shrink_once(src, dst, max_bytes, duration_secs);
    if result.is_err() {
        let _ = std::fs::remove_file(dst);
    }
    result
}

/// MF packs sizes and ratios as (high << 32) | low.
fn pack(high: u32, low: u32) -> u64 {
    ((high as u64) << 32) | low as u64
}

fn unpack(v: u64) -> (u32, u32) {
    ((v >> 32) as u32, v as u32)
}

fn shrink_once(src: &Path, dst: &Path, max_bytes: u64, duration_secs: f64) -> Result<()> {
    // SAFETY: plain MF calls on objects created here; out-pointers are valid locals.
    unsafe {
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 2)?;
        let attrs = attrs.context("no attributes")?;
        // Decoding, colour conversion and scaling happen inside the reader.
        attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)?;
        let reader: IMFSourceReader = MFCreateSourceReaderFromURL(&HSTRING::from(src.as_os_str()), &attrs)
            .with_context(|| format!("can't open {}", src.display()))?;
        reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
        let video_index = stream_index(&reader, true)?.context("the clip has no video")?;
        let audio_index = stream_index(&reader, false)?;
        reader.SetStreamSelection(video_index, true)?;
        let native = reader.GetNativeMediaType(video_index, 0)?;
        let (src_w, src_h) = unpack(native.GetUINT64(&MF_MT_FRAME_SIZE)?);
        let (fps_n, fps_d) = unpack(native.GetUINT64(&MF_MT_FRAME_RATE).unwrap_or(pack(60, 1)));
        let audio = match audio_index {
            Some(i) => Some(reader.GetNativeMediaType(i, 0)?),
            None => None,
        };
        let audio_bps = audio
            .as_ref()
            .and_then(|t| t.GetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND).ok())
            .map_or(0, |b| b * 8);
        let plan = shrink_plan(max_bytes, duration_secs, audio_bps, src_h)
            .context("the clip is too long to fit the upload limit; trim it first")?;
        // Keep the shape; encoders want even sizes.
        let height = plan.height & !1;
        let width = ((u64::from(src_w) * u64::from(height) / u64::from(src_h.max(1))) as u32) & !1;

        let raw = MFCreateMediaType()?;
        raw.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        raw.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        raw.SetUINT64(&MF_MT_FRAME_SIZE, pack(width, height))?;
        raw.SetUINT64(&MF_MT_FRAME_RATE, pack(fps_n, fps_d.max(1)))?;
        raw.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        raw.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        reader
            .SetCurrentMediaType(video_index, None, &raw)
            .context("can't decode the clip's video")?;
        if let (Some(i), Some(a)) = (audio_index, &audio) {
            reader.SetStreamSelection(i, true)?;
            reader.SetCurrentMediaType(i, None, a)?;
        }

        let out = MFCreateMediaType()?;
        out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
        out.SetUINT32(&MF_MT_AVG_BITRATE, plan.video_bps)?;
        out.SetUINT64(&MF_MT_FRAME_SIZE, pack(width, height))?;
        out.SetUINT64(&MF_MT_FRAME_RATE, pack(fps_n, fps_d.max(1)))?;
        out.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        out.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;

        if let Some(dir) = dst.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("can't create {}", dir.display()))?;
        }
        let mut wattrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut wattrs, 3)?;
        let wattrs = wattrs.context("no attributes")?;
        wattrs.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)?;
        // Throttling stays on: decoding outruns encoding, and without it every decoded frame would queue
        // up in memory.
        wattrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 0)?;
        let _ = std::fs::remove_file(dst);
        let writer: IMFSinkWriter = MFCreateSinkWriterFromURL(&HSTRING::from(dst.as_os_str()), None, &wattrs)
            .with_context(|| format!("can't create {}", dst.display()))?;
        let v_out = writer.AddStream(&out).context("no H.264 encoder")?;
        writer
            .SetInputMediaType(v_out, &raw, None)
            .context("the H.264 encoder doesn't take the decoded video")?;
        let a_out = match &audio {
            Some(a) => {
                let s = writer.AddStream(a)?;
                writer.SetInputMediaType(s, a, None)?;
                Some(s)
            }
            None => None,
        };
        writer.BeginWriting().context("can't start encoding")?;

        let mut video_done = false;
        let mut audio_done = audio.is_none();
        while !(video_done && audio_done) {
            let (mut actual, mut flags, mut time) = (0u32, 0u32, 0i64);
            let mut sample = None;
            reader.ReadSample(
                MF_SOURCE_READER_ANY_STREAM.0 as u32,
                0,
                Some(&mut actual),
                Some(&mut flags),
                Some(&mut time),
                Some(&mut sample),
            )?;
            let is_video = actual == video_index;
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                if is_video {
                    video_done = true;
                } else {
                    audio_done = true;
                }
                continue;
            }
            let Some(sample) = sample else { continue };
            if is_video {
                writer
                    .WriteSample(v_out, &sample)
                    .context("can't encode the video")?;
            } else if let Some(a_out) = a_out {
                // A fresh sample, like in `trim`: the reader's carry attributes that stretch the audio track.
                let mut packet = packet_from_sample(&sample)?;
                packet.pts = time;
                writer.WriteSample(a_out, &sample_from_packet(&packet, 0)?)?;
            }
        }
        drop(reader);
        writer.Finalize().context("can't finish the smaller copy")?;
    }
    Ok(())
}

/// The reader's index of its first video (`true`) or audio (`false`) stream, if it has one.
///
/// # Safety
/// `reader` must be a valid source reader.
unsafe fn stream_index(reader: &IMFSourceReader, video: bool) -> Result<Option<u32>> {
    let want = if video {
        MFMediaType_Video
    } else {
        MFMediaType_Audio
    };
    let mut index = 0u32;
    // SAFETY: valid reader (see the function's contract) and media types it returns.
    unsafe {
        while let Ok(t) = reader.GetNativeMediaType(index, 0) {
            if t.GetGUID(&MF_MT_MAJOR_TYPE)? == want {
                return Ok(Some(index));
            }
            index += 1;
        }
    }
    Ok(None)
}

#[cfg(test)]
mod shrink_tests {
    use super::*;

    #[test]
    fn plan_fits_the_budget_and_lowers_resolution_for_long_clips() {
        // 30 s into 10 MB with 160 kbit/s audio: ~2.35 Mbit/s video at 720p.
        let p = shrink_plan(10_000_000, 30.0, 160_000, 1080).unwrap();
        assert_eq!(p.height, 720);
        let total = (f64::from(p.video_bps) + 160_000.0) * 30.0 / 8.0;
        assert!(total <= 10_000_000.0 * 0.95, "{total}");
        // 2 minutes: 480p.
        assert_eq!(shrink_plan(10_000_000, 120.0, 160_000, 1080).unwrap().height, 480);
        // Short clip, big limit: full 1080p, never scaled up past the source.
        assert_eq!(shrink_plan(50_000_000, 30.0, 160_000, 1080).unwrap().height, 1080);
        assert_eq!(shrink_plan(50_000_000, 30.0, 160_000, 900).unwrap().height, 900);
        // Too long for the limit.
        assert_eq!(shrink_plan(10_000_000, 600.0, 160_000, 1080), None);
        assert_eq!(shrink_plan(10_000_000, 0.0, 160_000, 1080), None);
    }
}
