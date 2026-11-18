//! Turns a finished fragmented MP4 (a full recording) into a regular one, in place and without copying the
//! video.
//!
//! Recordings are written as fragmented MP4 so a crash keeps everything up to the last fragment. Browsers
//! play those badly once they get long: Chrome's MP4 reader walks every top-level box of a seekable
//! fragmented file to build its index before it plays, and a 4-hour recording has ~30,000 of them (one
//! range request each). A regular MP4 has one index (`moov`) instead, which players read in one go.
//!
//! [`make_regular`] reads the fragments' sample tables, appends a full `moov` at the end of the file, turns
//! the old `moov` into a `free` box, and rewrites the first fragment's header so that all fragments become
//! one `mdat` that ends where the new `moov` starts. The samples stay where they are: only a few MB of
//! index is written, whatever the file's size. Each step leaves a playable file, and running it again
//! finishes an interrupted run.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use anyhow::{Context, Result, ensure};

/// The largest index box read into memory.
const MAX_INDEX: u64 = 256 << 20;

#[derive(Clone, Copy, Debug)]
struct TopBox {
    typ: [u8; 4],
    start: u64,
    /// Header length: 8, or 16 with a 64-bit size.
    header: u64,
    size: u64,
}

impl TopBox {
    fn end(&self) -> u64 {
        self.start + self.size
    }
}

/// Makes the fragmented MP4 at `path` a regular one. `Ok(false)`: it already was (or isn't fragmented).
pub fn make_regular(path: &Path) -> Result<bool> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("can't open {}", path.display()))?;
    let len = file.metadata()?.len();
    let boxes = top_boxes(&mut file, len)?;

    let mut fragmented_moov = None;
    let mut regular_moov = None;
    for b in boxes.iter().filter(|b| &b.typ == b"moov") {
        let body = read_body(&mut file, b)?;
        if children(&body)?.iter().any(|(t, _)| t == b"mvex") {
            fragmented_moov.get_or_insert((*b, body));
        } else {
            regular_moov = Some(*b);
        }
    }
    let first_moof = boxes.iter().find(|b| &b.typ == b"moof").copied();

    // Where the regular index is (or will be): a run that was interrupted already wrote it as the last box.
    let index_start = match (regular_moov, &fragmented_moov) {
        (Some(m), _) if m.end() == len => m.start,
        (_, Some((_, body))) => {
            let moofs: Vec<TopBox> = boxes.iter().filter(|b| &b.typ == b"moof").copied().collect();
            ensure!(!moofs.is_empty(), "the recording has no fragments");
            let mut tracks = parse_moov(body)?;
            for moof in &moofs {
                let body = read_body(&mut file, moof)?;
                add_fragment(&mut tracks, &body, moof.start)?;
            }
            for t in &tracks {
                for c in &t.chunks {
                    let end = c.offset + c.sizes.iter().map(|&s| u64::from(s)).sum::<u64>();
                    ensure!(
                        c.offset >= moofs[0].start && end <= len,
                        "a sample lies outside the fragments"
                    );
                }
            }
            let moov = build_moov(body, &tracks)?;
            file.seek(SeekFrom::Start(len))?;
            file.write_all(&moov)?;
            file.sync_data()?;
            len
        }
        // No fragmented index and no index written by an earlier run: nothing to do.
        _ => return Ok(false),
    };

    let mut changed = index_start == len;
    if let Some((old, _)) = fragmented_moov {
        file.seek(SeekFrom::Start(old.start + 4))?;
        file.write_all(b"free")?;
        changed = true;
    }
    if let Some(moof) = first_moof.filter(|m| m.start < index_start) {
        let size = index_start - moof.start;
        file.seek(SeekFrom::Start(moof.start))?;
        match u32::try_from(size) {
            Ok(small) => {
                file.write_all(&small.to_be_bytes())?;
                file.write_all(b"mdat")?;
            }
            Err(_) => {
                file.write_all(&1u32.to_be_bytes())?;
                file.write_all(b"mdat")?;
                file.write_all(&size.to_be_bytes())?;
            }
        }
        changed = true;
    }
    file.sync_data()?;
    Ok(changed)
}

/// Whether the MP4 at `path` is fragmented (its index has an `mvex`), without reading more than its start.
pub fn is_fragmented(path: &Path) -> Result<bool> {
    let mut file = File::open(path).with_context(|| format!("can't open {}", path.display()))?;
    let len = file.metadata()?.len();
    let mut pos = 0;
    while pos + 8 <= len {
        let b = read_header(&mut file, pos, len)?;
        if &b.typ == b"moov" {
            let body = read_body(&mut file, &b)?;
            return Ok(children(&body)?.iter().any(|(t, _)| t == b"mvex"));
        }
        if &b.typ == b"moof" || &b.typ == b"mdat" {
            return Ok(false);
        }
        pos = b.end();
    }
    Ok(false)
}

// ---- reading --------------------------------------------------------------------------------------------

fn read_header(file: &mut File, pos: u64, len: u64) -> Result<TopBox> {
    let mut h = [0u8; 16];
    file.seek(SeekFrom::Start(pos))?;
    let n = file.read(&mut h)?;
    ensure!(n >= 8, "a box header is cut off at {pos}");
    let small = u64::from(u32::from_be_bytes(h[0..4].try_into().unwrap()));
    let typ: [u8; 4] = h[4..8].try_into().unwrap();
    let (header, size) = match small {
        0 => (8, len - pos),
        1 => {
            ensure!(n >= 16, "a box header is cut off at {pos}");
            (16, u64::from_be_bytes(h[8..16].try_into().unwrap()))
        }
        s => (8, s),
    };
    ensure!(
        size >= header && pos + size <= len,
        "a box at {pos} runs past the end of the file"
    );
    Ok(TopBox {
        typ,
        start: pos,
        header,
        size,
    })
}

fn top_boxes(file: &mut File, len: u64) -> Result<Vec<TopBox>> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos + 8 <= len {
        let b = read_header(file, pos, len)?;
        out.push(b);
        pos = b.end();
    }
    Ok(out)
}

fn read_body(file: &mut File, b: &TopBox) -> Result<Vec<u8>> {
    let n = b.size - b.header;
    ensure!(
        n <= MAX_INDEX,
        "a {} box is too big",
        String::from_utf8_lossy(&b.typ)
    );
    let mut body = vec![0u8; n as usize];
    file.seek(SeekFrom::Start(b.start + b.header))?;
    file.read_exact(&mut body)?;
    Ok(body)
}

/// The child boxes of a container's body.
fn children(buf: &[u8]) -> Result<Vec<([u8; 4], &[u8])>> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos + 8 <= buf.len() {
        let small = u32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap()) as usize;
        let typ: [u8; 4] = buf[pos + 4..pos + 8].try_into().unwrap();
        let (header, size) = match small {
            0 => (8, buf.len() - pos),
            1 => {
                ensure!(pos + 16 <= buf.len(), "a box header is cut off");
                let s = u64::from_be_bytes(buf[pos + 8..pos + 16].try_into().unwrap());
                (16, usize::try_from(s)?)
            }
            s => (8, s),
        };
        ensure!(
            size >= header && pos + size <= buf.len(),
            "a box runs past its parent"
        );
        out.push((typ, &buf[pos + header..pos + size]));
        pos += size;
    }
    Ok(out)
}

fn child<'a>(buf: &'a [u8], typ: &[u8; 4]) -> Result<&'a [u8]> {
    children(buf)?
        .into_iter()
        .find(|(t, _)| t == typ)
        .map(|(_, b)| b)
        .with_context(|| format!("no {} box", String::from_utf8_lossy(typ)))
}

/// Reads big-endian numbers from a box body.
struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        ensure!(self.pos + n <= self.buf.len(), "a box is shorter than its fields");
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
}

/// One run of contiguous samples (a `trun`), which becomes one chunk.
#[derive(Debug, Default)]
struct Chunk {
    offset: u64,
    description: u32,
    sizes: Vec<u32>,
}

#[derive(Debug, Default)]
struct Track {
    id: u32,
    timescale: u32,
    // trex defaults
    default_description: u32,
    default_duration: u32,
    default_size: u32,
    default_flags: u32,
    chunks: Vec<Chunk>,
    durations: Vec<u32>,
    /// Composition offsets; `None` while all are zero.
    cts: Option<Vec<i32>>,
    /// 1-based numbers of the sync samples.
    sync: Vec<u32>,
    samples: u32,
    /// Decode time of the next sample (tfdt resets it).
    dts: u64,
    /// Decode time of the first sample (an edit list delays the track by this much).
    start: Option<u64>,
}

fn parse_moov(moov: &[u8]) -> Result<Vec<Track>> {
    let mut tracks = Vec::new();
    for (typ, trak) in children(moov)? {
        if &typ != b"trak" {
            continue;
        }
        let tkhd = child(trak, b"tkhd")?;
        let mut c = Cursor::new(tkhd);
        let version = c.u32()? >> 24;
        c.take(if version == 1 { 16 } else { 8 })?;
        let id = c.u32()?;
        let mdhd = child(child(trak, b"mdia")?, b"mdhd")?;
        let mut c = Cursor::new(mdhd);
        let version = c.u32()? >> 24;
        c.take(if version == 1 { 16 } else { 8 })?;
        let timescale = c.u32()?;
        ensure!(timescale > 0, "a track has no timescale");
        tracks.push(Track {
            id,
            timescale,
            default_description: 1,
            ..Default::default()
        });
    }
    ensure!(!tracks.is_empty(), "the recording has no tracks");
    for (typ, trex) in children(child(moov, b"mvex")?)? {
        if &typ != b"trex" {
            continue;
        }
        let mut c = Cursor::new(trex);
        c.u32()?;
        let id = c.u32()?;
        if let Some(t) = tracks.iter_mut().find(|t| t.id == id) {
            t.default_description = c.u32()?;
            t.default_duration = c.u32()?;
            t.default_size = c.u32()?;
            t.default_flags = c.u32()?;
        }
    }
    Ok(tracks)
}

/// Adds one `moof`'s samples. `moof_start` is the file offset of the `moof` box.
fn add_fragment(tracks: &mut [Track], moof: &[u8], moof_start: u64) -> Result<()> {
    // Where the previous track fragment's data ended (the implicit base of the next one).
    let mut previous_end: Option<u64> = None;
    for (typ, traf) in children(moof)? {
        if &typ != b"traf" {
            continue;
        }
        let mut c = Cursor::new(child(traf, b"tfhd")?);
        let flags = c.u32()? & 0x00ff_ffff;
        let id = c.u32()?;
        let track = tracks
            .iter_mut()
            .find(|t| t.id == id)
            .with_context(|| format!("a fragment names an unknown track {id}"))?;
        let explicit_base = if flags & 0x1 != 0 { Some(c.u64()?) } else { None };
        let description = if flags & 0x2 != 0 {
            c.u32()?
        } else {
            track.default_description
        };
        let duration = if flags & 0x8 != 0 {
            c.u32()?
        } else {
            track.default_duration
        };
        let size = if flags & 0x10 != 0 {
            c.u32()?
        } else {
            track.default_size
        };
        let sample_flags = if flags & 0x20 != 0 {
            c.u32()?
        } else {
            track.default_flags
        };
        let base = explicit_base.unwrap_or(if flags & 0x2_0000 != 0 {
            moof_start
        } else {
            previous_end.unwrap_or(moof_start)
        });

        if let Ok(tfdt) = child(traf, b"tfdt") {
            let mut c = Cursor::new(tfdt);
            let decode_time = if c.u32()? >> 24 == 1 {
                c.u64()?
            } else {
                u64::from(c.u32()?)
            };
            track.set_decode_time(decode_time);
        }

        let mut next = base;
        for (typ, trun) in children(traf)? {
            if &typ != b"trun" {
                continue;
            }
            let mut c = Cursor::new(trun);
            let vf = c.u32()?;
            let (version, tflags) = (vf >> 24, vf & 0x00ff_ffff);
            let count = c.u32()?;
            let offset = if tflags & 0x1 != 0 {
                base.checked_add_signed(i64::from(c.u32()? as i32))
                    .context("a fragment's data offset is out of range")?
            } else {
                next
            };
            let first_flags = if tflags & 0x4 != 0 { Some(c.u32()?) } else { None };
            let mut chunk = Chunk {
                offset,
                description,
                sizes: Vec::with_capacity(count as usize),
            };
            for i in 0..count {
                let d = if tflags & 0x100 != 0 { c.u32()? } else { duration };
                let s = if tflags & 0x200 != 0 { c.u32()? } else { size };
                let f = if tflags & 0x400 != 0 {
                    c.u32()?
                } else if i == 0 {
                    first_flags.unwrap_or(sample_flags)
                } else {
                    sample_flags
                };
                let cts = if tflags & 0x800 != 0 {
                    let raw = c.u32()?;
                    if version == 0 {
                        i32::try_from(raw).unwrap_or(i32::MAX)
                    } else {
                        raw as i32
                    }
                } else {
                    0
                };
                track.push_sample(d, f, cts);
                chunk.sizes.push(s);
            }
            next = offset + chunk.sizes.iter().map(|&s| u64::from(s)).sum::<u64>();
            if !chunk.sizes.is_empty() {
                track.chunks.push(chunk);
            }
        }
        previous_end = Some(next);
    }
    Ok(())
}

impl Track {
    fn push_sample(&mut self, duration: u32, flags: u32, cts: i32) {
        self.start.get_or_insert(self.dts);
        self.samples += 1;
        // sample_is_non_sync_sample is bit 16 of the sample flags.
        if flags & 0x0001_0000 == 0 {
            self.sync.push(self.samples);
        }
        if cts != 0 && self.cts.is_none() {
            self.cts = Some(vec![0; self.samples as usize - 1]);
        }
        if let Some(c) = self.cts.as_mut() {
            c.push(cts);
        }
        self.durations.push(duration);
        self.dts += u64::from(duration);
    }

    /// A `tfdt` says when the fragment's first sample decodes: a gap or overlap is absorbed by the
    /// previous sample's duration, so the samples keep their times.
    fn set_decode_time(&mut self, t: u64) {
        match self.durations.last_mut() {
            None => self.dts = t,
            Some(last) => {
                let prev_start = self.dts - u64::from(*last);
                let wanted = t.saturating_sub(prev_start).clamp(1, u64::from(u32::MAX));
                *last = wanted as u32;
                self.dts = prev_start + wanted;
            }
        }
    }

    fn media_duration(&self) -> u64 {
        self.durations.iter().map(|&d| u64::from(d)).sum()
    }
}

// ---- writing --------------------------------------------------------------------------------------------

fn boxed(typ: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 16);
    match u32::try_from(body.len() + 8) {
        Ok(size) => {
            out.extend_from_slice(&size.to_be_bytes());
            out.extend_from_slice(typ);
        }
        Err(_) => {
            out.extend_from_slice(&1u32.to_be_bytes());
            out.extend_from_slice(typ);
            out.extend_from_slice(&(body.len() as u64 + 16).to_be_bytes());
        }
    }
    out.extend_from_slice(body);
    out
}

fn full(version: u8, flags: u32) -> Vec<u8> {
    ((u32::from(version) << 24) | (flags & 0x00ff_ffff))
        .to_be_bytes()
        .to_vec()
}

fn movie_time(media: u64, media_scale: u32, movie_scale: u32) -> u64 {
    (u128::from(media) * u128::from(movie_scale) / u128::from(media_scale.max(1))) as u64
}

/// `mvhd`, `tkhd` and `mdhd` all start with creation and modification times, then (for mvhd and mdhd) the
/// timescale and the duration; tkhd has the track ID and a reserved word where the others have the
/// timescale. This rewrites one as version 1 with a new duration, keeping every other field.
fn with_duration(body: &[u8], kind: &[u8; 4], duration: u64) -> Result<Vec<u8>> {
    let mut c = Cursor::new(body);
    let vf = c.u32()?;
    let (version, flags) = ((vf >> 24) as u8, vf & 0x00ff_ffff);
    let (created, modified) = if version == 1 {
        (c.u64()?, c.u64()?)
    } else {
        (u64::from(c.u32()?), u64::from(c.u32()?))
    };
    let middle = c.take(if kind == b"tkhd" { 8 } else { 4 })?;
    if version == 1 {
        c.u64()?
    } else {
        u64::from(c.u32()?)
    };
    let rest = &body[c.pos..];
    let mut out = full(1, flags);
    out.extend_from_slice(&created.to_be_bytes());
    out.extend_from_slice(&modified.to_be_bytes());
    out.extend_from_slice(middle);
    out.extend_from_slice(&duration.to_be_bytes());
    out.extend_from_slice(rest);
    Ok(out)
}

fn movie_timescale(moov: &[u8]) -> Result<u32> {
    let mut c = Cursor::new(child(moov, b"mvhd")?);
    let version = c.u32()? >> 24;
    c.take(if version == 1 { 16 } else { 8 })?;
    Ok(c.u32()?.max(1))
}

fn build_moov(old: &[u8], tracks: &[Track]) -> Result<Vec<u8>> {
    let movie_scale = movie_timescale(old)?;
    let span = |t: &Track| {
        movie_time(
            t.start.unwrap_or(0) + t.media_duration(),
            t.timescale,
            movie_scale,
        )
    };
    let movie_duration = tracks.iter().map(span).max().unwrap_or(0);
    let mut body = Vec::new();
    let mut traks = tracks.iter();
    for (typ, b) in children(old)? {
        match &typ {
            b"mvhd" => body.extend(boxed(b"mvhd", &with_duration(b, b"mvhd", movie_duration)?)),
            b"mvex" => {}
            b"trak" => {
                let track = traks.next().context("more tracks than expected")?;
                body.extend(build_trak(b, track, movie_scale)?);
            }
            _ => body.extend(boxed(&typ, b)),
        }
    }
    Ok(boxed(b"moov", &body))
}

fn build_trak(old: &[u8], t: &Track, movie_scale: u32) -> Result<Vec<u8>> {
    let delay = t.start.unwrap_or(0);
    let media = t.media_duration();
    let mut body = Vec::new();
    for (typ, b) in children(old)? {
        match &typ {
            b"tkhd" => {
                let d = movie_time(delay + media, t.timescale, movie_scale);
                body.extend(boxed(b"tkhd", &with_duration(b, b"tkhd", d)?));
                if delay > 0 {
                    body.extend(edit_list(delay, media, t.timescale, movie_scale));
                }
            }
            // A fragmented file's edit list described the fragments; the new one (above) replaces it.
            b"edts" => {}
            b"mdia" => body.extend(boxed(b"mdia", &build_mdia(b, t)?)),
            _ => body.extend(boxed(&typ, b)),
        }
    }
    Ok(boxed(b"trak", &body))
}

/// An empty edit for the time before the track's first sample, then the whole media.
fn edit_list(delay: u64, media: u64, media_scale: u32, movie_scale: u32) -> Vec<u8> {
    let mut elst = full(1, 0);
    elst.extend_from_slice(&2u32.to_be_bytes());
    elst.extend_from_slice(&movie_time(delay, media_scale, movie_scale).to_be_bytes());
    elst.extend_from_slice(&(-1i64).to_be_bytes());
    elst.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    elst.extend_from_slice(&movie_time(media, media_scale, movie_scale).to_be_bytes());
    elst.extend_from_slice(&0i64.to_be_bytes());
    elst.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    boxed(b"edts", &boxed(b"elst", &elst))
}

fn build_mdia(old: &[u8], t: &Track) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    for (typ, b) in children(old)? {
        match &typ {
            b"mdhd" => body.extend(boxed(b"mdhd", &with_duration(b, b"mdhd", t.media_duration())?)),
            b"minf" => {
                let mut minf = Vec::new();
                for (typ, b) in children(b)? {
                    if &typ == b"stbl" {
                        minf.extend(boxed(b"stbl", &build_stbl(b, t)?));
                    } else {
                        minf.extend(boxed(&typ, b));
                    }
                }
                body.extend(boxed(b"minf", &minf));
            }
            _ => body.extend(boxed(&typ, b)),
        }
    }
    Ok(body)
}

fn build_stbl(old: &[u8], t: &Track) -> Result<Vec<u8>> {
    let mut body = boxed(b"stsd", child(old, b"stsd")?);

    // stts: runs of equal durations.
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for &d in &t.durations {
        match runs.last_mut() {
            Some((n, last)) if *last == d => *n += 1,
            _ => runs.push((1, d)),
        }
    }
    let mut stts = full(0, 0);
    stts.extend_from_slice(&(runs.len() as u32).to_be_bytes());
    for (n, d) in runs {
        stts.extend_from_slice(&n.to_be_bytes());
        stts.extend_from_slice(&d.to_be_bytes());
    }
    body.extend(boxed(b"stts", &stts));

    if let Some(cts) = &t.cts {
        let mut runs: Vec<(u32, i32)> = Vec::new();
        for &o in cts {
            match runs.last_mut() {
                Some((n, last)) if *last == o => *n += 1,
                _ => runs.push((1, o)),
            }
        }
        let negative = cts.iter().any(|&o| o < 0);
        let mut ctts = full(u8::from(negative), 0);
        ctts.extend_from_slice(&(runs.len() as u32).to_be_bytes());
        for (n, o) in runs {
            ctts.extend_from_slice(&n.to_be_bytes());
            ctts.extend_from_slice(&o.to_be_bytes());
        }
        body.extend(boxed(b"ctts", &ctts));
    }

    // stss only when some samples aren't sync samples (audio: all are, so no table).
    if t.sync.len() < t.samples as usize {
        let mut stss = full(0, 0);
        stss.extend_from_slice(&(t.sync.len() as u32).to_be_bytes());
        for &n in &t.sync {
            stss.extend_from_slice(&n.to_be_bytes());
        }
        body.extend(boxed(b"stss", &stss));
    }

    // stsc: runs of chunks with the same sample count and description.
    let mut stsc_runs: Vec<(u32, u32, u32)> = Vec::new();
    for (i, c) in t.chunks.iter().enumerate() {
        let (n, d) = (c.sizes.len() as u32, c.description);
        if !matches!(stsc_runs.last(), Some(&(_, ln, ld)) if ln == n && ld == d) {
            stsc_runs.push((i as u32 + 1, n, d));
        }
    }
    let mut stsc = full(0, 0);
    stsc.extend_from_slice(&(stsc_runs.len() as u32).to_be_bytes());
    for (first, n, d) in stsc_runs {
        stsc.extend_from_slice(&first.to_be_bytes());
        stsc.extend_from_slice(&n.to_be_bytes());
        stsc.extend_from_slice(&d.to_be_bytes());
    }
    body.extend(boxed(b"stsc", &stsc));

    let mut stsz = full(0, 0);
    stsz.extend_from_slice(&0u32.to_be_bytes());
    stsz.extend_from_slice(&t.samples.to_be_bytes());
    for c in &t.chunks {
        for &s in &c.sizes {
            stsz.extend_from_slice(&s.to_be_bytes());
        }
    }
    body.extend(boxed(b"stsz", &stsz));

    // co64: recordings pass 4 GB, so always 64-bit offsets.
    let mut co64 = full(0, 0);
    co64.extend_from_slice(&(t.chunks.len() as u32).to_be_bytes());
    for c in &t.chunks {
        co64.extend_from_slice(&c.offset.to_be_bytes());
    }
    body.extend(boxed(b"co64", &co64));
    Ok(body)
}

#[cfg(test)]
mod tests;
