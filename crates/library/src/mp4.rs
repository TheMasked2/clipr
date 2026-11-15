//! A small MP4 reader: just enough to get the duration and the video size without decoding anything.
//!
//! Handles 64-bit box sizes, version 1 headers and fragmented files (recordings), whose `mvhd` duration may be
//! 0: then `mehd`, or the sum of the `trun` sample durations, is used. A truncated tail (a file still being
//! written) is tolerated.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{Context, Result};

/// Boxes bigger than this are skipped instead of read into memory.
const MAX_BOX: u64 = 256 << 20;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Mp4Info {
    pub duration_secs: Option<f64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

pub fn probe_file(path: &Path) -> Result<Mp4Info> {
    let file = File::open(path).with_context(|| format!("can't open {}", path.display()))?;
    let len = file.metadata()?.len();
    probe(&mut BufReader::new(file), len)
}

#[derive(Default)]
struct Track {
    id: u32,
    handler: Option<[u8; 4]>,
    timescale: u32,
    width: u32,
    height: u32,
}

#[derive(Default)]
struct Moov {
    timescale: u32,
    duration: Option<u64>,
    mehd: Option<u64>,
    tracks: Vec<Track>,
    trex_default_duration: HashMap<u32, u32>,
}

pub fn probe<R: Read + Seek>(r: &mut R, len: u64) -> Result<Mp4Info> {
    let mut pos = 0u64;
    let mut moov: Option<Moov> = None;
    let mut frag: HashMap<u32, u64> = HashMap::new();
    while pos.checked_add(8).is_some_and(|end| end <= len) {
        r.seek(SeekFrom::Start(pos))?;
        let mut head = [0u8; 16];
        let n = (len - pos).min(16) as usize;
        r.read_exact(&mut head[..n])?;
        let mut size = u64::from(u32::from_be_bytes(head[0..4].try_into()?));
        let kind: [u8; 4] = head[4..8].try_into()?;
        let mut header = 8u64;
        if size == 1 {
            if n < 16 {
                break;
            }
            size = u64::from_be_bytes(head[8..16].try_into()?);
            header = 16;
        } else if size == 0 {
            size = len - pos;
        }
        // Checked: a 64-bit size can be anything. Also guarantees the loop moves forward (size >= 8).
        if size < header || pos.checked_add(size).is_none_or(|end| end > len) {
            break; // truncated or garbage
        }
        let payload = size - header;
        if (&kind == b"moov" || &kind == b"moof") && payload <= MAX_BOX {
            r.seek(SeekFrom::Start(pos + header))?;
            let mut buf = vec![0u8; payload as usize];
            r.read_exact(&mut buf)?;
            if &kind == b"moov" {
                moov = Some(parse_moov(&buf));
            } else if let Some(m) = &moov {
                parse_moof(&buf, m, &mut frag);
            }
        }
        pos += size;
        if moov.as_ref().is_some_and(|m| m.duration.is_some_and(|d| d > 0)) {
            break; // a normal file: everything we need is known
        }
    }
    let Some(moov) = moov else {
        anyhow::bail!("no moov box: not a finished MP4");
    };
    Ok(summarize(&moov, &frag))
}

fn summarize(moov: &Moov, frag: &HashMap<u32, u64>) -> Mp4Info {
    let video = moov
        .tracks
        .iter()
        .find(|t| t.handler == Some(*b"vide"))
        .or_else(|| moov.tracks.iter().find(|t| t.width > 0 && t.height > 0));
    let secs = |units: u64, scale: u32| (scale > 0 && units > 0).then(|| units as f64 / f64::from(scale));
    let duration = moov
        .duration
        .and_then(|d| secs(d, moov.timescale))
        .or_else(|| moov.mehd.and_then(|d| secs(d, moov.timescale)))
        .or_else(|| {
            let mut best: Option<f64> = None;
            for t in &moov.tracks {
                if let Some(s) = frag.get(&t.id).and_then(|&u| secs(u, t.timescale)) {
                    best = Some(best.map_or(s, |b| b.max(s)));
                }
            }
            best
        });
    Mp4Info {
        duration_secs: duration,
        width: video.map(|t| t.width).filter(|&w| w > 0),
        height: video.map(|t| t.height).filter(|&h| h > 0),
    }
}

/// The child boxes of a box payload: `(type, payload)`.
fn children(buf: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while p + 8 <= buf.len() {
        let mut size = u64::from(be32(buf, p).unwrap_or(0));
        let kind: [u8; 4] = buf[p + 4..p + 8].try_into().unwrap_or_default();
        let mut header = 8usize;
        if size == 1 {
            let Some(big) = be64(buf, p + 8) else { break };
            size = big;
            header = 16;
        } else if size == 0 {
            size = (buf.len() - p) as u64;
        }
        let end = usize::try_from(size).ok().and_then(|s| p.checked_add(s));
        let Some(end) = end.filter(|&e| size >= header as u64 && e <= buf.len()) else {
            break;
        };
        out.push((kind, &buf[p + header..end]));
        p = end;
    }
    out
}

fn be32(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

fn be64(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

/// `(timescale, duration)` of an `mvhd` or `mdhd` payload (same layout).
fn time_header(p: &[u8]) -> Option<(u32, Option<u64>)> {
    if *p.first()? == 1 {
        let d = be64(p, 24)?;
        Some((be32(p, 20)?, (d != u64::MAX).then_some(d)))
    } else {
        let d = be32(p, 16)?;
        Some((be32(p, 12)?, (d != u32::MAX).then_some(u64::from(d))))
    }
}

fn parse_moov(buf: &[u8]) -> Moov {
    let mut moov = Moov::default();
    for (kind, body) in children(buf) {
        match &kind {
            b"mvhd" => {
                if let Some((ts, d)) = time_header(body) {
                    moov.timescale = ts;
                    moov.duration = d;
                }
            }
            b"trak" => moov.tracks.push(parse_trak(body)),
            b"mvex" => {
                for (k, b) in children(body) {
                    match &k {
                        b"mehd" => {
                            moov.mehd = if b.first() == Some(&1) {
                                be64(b, 4)
                            } else {
                                be32(b, 4).map(u64::from)
                            };
                        }
                        b"trex" => {
                            if let (Some(id), Some(d)) = (be32(b, 4), be32(b, 12)) {
                                moov.trex_default_duration.insert(id, d);
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    moov
}

fn parse_trak(buf: &[u8]) -> Track {
    let mut t = Track::default();
    for (kind, body) in children(buf) {
        match &kind {
            b"tkhd" => {
                let v1 = body.first() == Some(&1);
                let (id_off, w_off) = if v1 { (20, 88) } else { (12, 76) };
                t.id = be32(body, id_off).unwrap_or(0);
                t.width = be32(body, w_off).unwrap_or(0) >> 16;
                t.height = be32(body, w_off + 4).unwrap_or(0) >> 16;
            }
            b"mdia" => {
                for (k, b) in children(body) {
                    match &k {
                        b"mdhd" => t.timescale = time_header(b).map_or(0, |(ts, _)| ts),
                        b"hdlr" => t.handler = b.get(8..12).and_then(|h| h.try_into().ok()),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    t
}

fn parse_moof(buf: &[u8], moov: &Moov, frag: &mut HashMap<u32, u64>) {
    for (kind, body) in children(buf) {
        if &kind != b"traf" {
            continue;
        }
        let mut track_id = 0;
        let mut default_dur = None;
        for (k, b) in children(body) {
            match &k {
                b"tfhd" => {
                    let flags = be32(b, 0).unwrap_or(0) & 0x00ff_ffff;
                    track_id = be32(b, 4).unwrap_or(0);
                    let mut o = 8;
                    if flags & 0x1 != 0 {
                        o += 8;
                    }
                    if flags & 0x2 != 0 {
                        o += 4;
                    }
                    if flags & 0x8 != 0 {
                        default_dur = be32(b, o);
                    }
                }
                b"trun" => {
                    let flags = be32(b, 0).unwrap_or(0) & 0x00ff_ffff;
                    let count = be32(b, 4).unwrap_or(0) as usize;
                    let mut o = 8;
                    if flags & 0x1 != 0 {
                        o += 4;
                    }
                    if flags & 0x4 != 0 {
                        o += 4;
                    }
                    let fields = [0x100, 0x200, 0x400, 0x800]
                        .iter()
                        .filter(|f| flags & **f != 0)
                        .count();
                    let default = default_dur
                        .or_else(|| moov.trex_default_duration.get(&track_id).copied())
                        .unwrap_or(0);
                    let mut total = 0u64;
                    if flags & 0x100 != 0 {
                        for i in 0..count {
                            match be32(b, o + i * fields * 4) {
                                Some(d) => total += u64::from(d),
                                None => break,
                            }
                        }
                    } else {
                        total = u64::from(default) * count as u64;
                    }
                    *frag.entry(track_id).or_default() += total;
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(payload);
        v
    }

    fn boxed64(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut v = 1u32.to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(&((payload.len() + 16) as u64).to_be_bytes());
        v.extend_from_slice(payload);
        v
    }

    fn mvhd(v1: bool, timescale: u32, duration: u64) -> Vec<u8> {
        let mut p = vec![u8::from(v1), 0, 0, 0];
        if v1 {
            p.extend([0u8; 16]);
            p.extend(timescale.to_be_bytes());
            p.extend(duration.to_be_bytes());
        } else {
            p.extend([0u8; 8]);
            p.extend(timescale.to_be_bytes());
            p.extend((duration as u32).to_be_bytes());
        }
        p.extend([0u8; 80]);
        boxed(b"mvhd", &p)
    }

    fn trak(v1: bool, id: u32, w: u32, h: u32, handler: &[u8; 4], timescale: u32) -> Vec<u8> {
        // Payload layout: see `parse_trak`; the width sits at 76 (v0) or 88 (v1).
        let (id_off, w_off) = if v1 { (20, 88) } else { (12, 76) };
        let mut t = vec![0u8; w_off + 8];
        t[0] = u8::from(v1);
        t[id_off..id_off + 4].copy_from_slice(&id.to_be_bytes());
        t[w_off..w_off + 4].copy_from_slice(&(w << 16).to_be_bytes());
        t[w_off + 4..w_off + 8].copy_from_slice(&(h << 16).to_be_bytes());
        let mut hdlr = vec![0u8; 8];
        hdlr.extend(handler);
        hdlr.extend([0u8; 13]);
        let mut mdhd = vec![0u8; 12];
        mdhd.extend(timescale.to_be_bytes());
        mdhd.extend([0u8; 8]);
        let mdia = [boxed(b"mdhd", &mdhd), boxed(b"hdlr", &hdlr)].concat();
        boxed(b"trak", &[boxed(b"tkhd", &t), boxed(b"mdia", &mdia)].concat())
    }

    #[test]
    fn plain_file_with_64_bit_boxes() {
        for v1 in [false, true] {
            let moov = boxed(
                b"moov",
                &[
                    mvhd(v1, 1000, 30_500),
                    trak(v1, 1, 1920, 1080, b"vide", 90_000),
                    trak(v1, 2, 0, 0, b"soun", 48_000),
                ]
                .concat(),
            );
            let file = [boxed(b"ftyp", b"isom0000"), boxed64(b"mdat", &[0; 100]), moov].concat();
            let info = probe(&mut Cursor::new(&file), file.len() as u64).unwrap();
            assert_eq!(info.duration_secs, Some(30.5), "v1={v1}");
            assert_eq!((info.width, info.height), (Some(1920), Some(1080)), "v1={v1}");
        }
    }

    #[test]
    fn fragmented_file_sums_trun_durations() {
        let mut tfhd = vec![0, 0, 0, 0x08];
        tfhd.extend(1u32.to_be_bytes());
        tfhd.extend(1000u32.to_be_bytes()); // default duration
        let mut trun = vec![0, 0, 0x01, 0x00]; // sample durations present
        trun.extend(3u32.to_be_bytes());
        for d in [1000u32, 1000, 500] {
            trun.extend(d.to_be_bytes());
        }
        let traf = boxed(b"traf", &[boxed(b"tfhd", &tfhd), boxed(b"trun", &trun)].concat());
        let moof = boxed(b"moof", &traf);
        let moov = boxed(
            b"moov",
            &[mvhd(false, 1000, 0), trak(false, 1, 640, 360, b"vide", 1000)].concat(),
        );
        let file = [
            moov,
            moof.clone(),
            boxed(b"mdat", &[0; 10]),
            moof,
            boxed(b"mdat", &[0; 10]),
        ]
        .concat();
        let info = probe(&mut Cursor::new(&file), file.len() as u64).unwrap();
        assert_eq!(info.duration_secs, Some(5.0));
        assert_eq!(info.width, Some(640));
    }

    #[test]
    fn truncated_and_garbage_input() {
        assert!(probe(&mut Cursor::new(vec![1u8; 50]), 50).is_err());
        let mut file = boxed(b"moov", &mvhd(false, 1000, 2000));
        file.extend(boxed(b"mdat", &[0; 50]));
        file.truncate(file.len() - 20);
        let info = probe(&mut Cursor::new(&file), file.len() as u64).unwrap();
        assert_eq!(info.duration_secs, Some(2.0));
    }

    #[test]
    fn real_file_if_present() {
        let path = Path::new(
            r"C:\Users\piete\AppData\Local\Temp\claude\d109dc1bc7f1c277\1434c99c-b3c8-476a-83af-9c09a9d924d1\scratchpad\clipr_spike_30s_h264.mp4",
        );
        if !path.exists() {
            return;
        }
        let info = probe_file(path).unwrap();
        let d = info.duration_secs.expect("duration");
        assert!((25.0..35.0).contains(&d), "duration {d}");
        assert!(info.width.unwrap() >= 320 && info.height.unwrap() >= 240);
    }

    fn probe_bytes(bytes: &[u8]) -> Result<Mp4Info> {
        probe(&mut Cursor::new(bytes), bytes.len() as u64)
    }

    #[test]
    fn crafted_sizes_do_not_panic() {
        // A 64-bit size of u64::MAX at the top level, then inside moov.
        let mut huge = 1u32.to_be_bytes().to_vec();
        huge.extend(b"free");
        huge.extend(u64::MAX.to_be_bytes());
        assert!(probe_bytes(&huge).is_err());

        let mut inner = 1u32.to_be_bytes().to_vec();
        inner.extend(b"trak");
        inner.extend(u64::MAX.to_be_bytes());
        let moov = boxed(b"moov", &[mvhd(false, 1000, 5000), inner].concat());
        let info = probe_bytes(&moov).unwrap();
        assert_eq!(info.duration_secs, Some(5.0));

        // Sizes below the header size, and size 1 without room for the 64-bit field.
        for head in [[0, 0, 0, 2], [0, 0, 0, 7], [0, 0, 0, 1]] {
            let mut b = head.to_vec();
            b.extend(b"moov");
            b.extend([0u8; 4]);
            assert!(probe_bytes(&b).is_err());
        }
        // Size 1 with a 64-bit size smaller than 16.
        let mut small = 1u32.to_be_bytes().to_vec();
        small.extend(b"moov");
        small.extend(3u64.to_be_bytes());
        assert!(probe_bytes(&small).is_err());
        // Size 0 means "to the end"; with nothing after the header it must still end.
        let mut zero = vec![0, 0, 0, 0];
        zero.extend(b"mdat");
        assert!(probe_bytes(&zero).is_err());
        // Truncated tails and every prefix of a good file.
        let good = [boxed(b"ftyp", b"isom"), moov].concat();
        for n in 0..good.len() {
            let _ = probe_bytes(&good[..n]);
        }
        assert!(probe_bytes(&good).is_ok());
    }

    #[test]
    fn children_stop_on_bad_sizes() {
        let mut bad = 0xFFFF_FFFFu32.to_be_bytes().to_vec();
        bad.extend(b"abcd");
        assert!(children(&bad).is_empty());
        let mut big = 1u32.to_be_bytes().to_vec();
        big.extend(b"abcd");
        big.extend(u64::MAX.to_be_bytes());
        assert!(children(&big).is_empty());
        let ok = [boxed(b"abcd", b"xy"), bad].concat();
        assert_eq!(children(&ok).len(), 1);
    }
}
