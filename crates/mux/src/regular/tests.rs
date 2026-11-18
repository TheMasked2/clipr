use super::*;

fn bx(typ: &[u8; 4], body: &[u8]) -> Vec<u8> {
    boxed(typ, body)
}

fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}

fn be(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

/// A trak as Media Foundation writes it for a fragmented file: empty sample tables.
fn trak(id: u32, timescale: u32, handler: &[u8; 4]) -> Vec<u8> {
    let mut tkhd = full(0, 1);
    tkhd.extend([0; 8]); // creation, modification
    tkhd.extend(be(id));
    tkhd.extend([0; 4]); // reserved
    tkhd.extend([0; 4]); // duration
    tkhd.extend([0xAB; 60]); // the rest, kept as is
    let mut mdhd = full(0, 0);
    mdhd.extend([0; 8]);
    mdhd.extend(be(timescale));
    mdhd.extend([0; 4]);
    mdhd.extend([0x55, 0xc4, 0, 0]);
    let mut hdlr = full(0, 0);
    hdlr.extend([0; 4]);
    hdlr.extend(handler);
    hdlr.extend([0; 13]);
    let empty = |typ: &[u8; 4], words: usize| bx(typ, &vec![0; 4 * words]);
    let stbl = cat(&[
        bx(b"stsd", b"\0\0\0\0\0\0\0\x01dummy-sample-entry"),
        empty(b"stts", 2),
        empty(b"stsc", 2),
        empty(b"stsz", 3),
        empty(b"stco", 2),
    ]);
    let minf = cat(&[bx(b"vmhd", &[0; 12]), bx(b"dinf", &[1, 2, 3]), bx(b"stbl", &stbl)]);
    let mdia = cat(&[bx(b"mdhd", &mdhd), bx(b"hdlr", &hdlr), bx(b"minf", &minf)]);
    bx(b"trak", &cat(&[bx(b"tkhd", &tkhd), bx(b"mdia", &mdia)]))
}

fn trex(id: u32, duration: u32, size: u32, flags: u32) -> Vec<u8> {
    let mut b = full(0, 0);
    for v in [id, 1, duration, size, flags] {
        b.extend(be(v));
    }
    bx(b"trex", &b)
}

fn moov() -> Vec<u8> {
    let mut mvhd = full(0, 0);
    mvhd.extend([0; 8]);
    mvhd.extend(be(1000)); // movie timescale
    mvhd.extend([0; 4]);
    mvhd.extend([0xCD; 80]);
    let mvex = cat(&[trex(1, 0, 0, 0x0001_0000), trex(2, 1024, 0, 0)]);
    bx(
        b"moov",
        &cat(&[
            bx(b"mvhd", &mvhd),
            trak(1, 60_000, b"vide"),
            trak(2, 48_000, b"soun"),
            bx(b"mvex", &mvex),
        ]),
    )
}

/// Sample payloads: video sample `n` is `n` repeated, audio sample `n` is `100 + n` repeated.
fn payload(video: bool, n: u32, size: u32) -> Vec<u8> {
    vec![if video { n as u8 } else { 100 + n as u8 }; size as usize]
}

struct Frag {
    video: Vec<(u32, u32, bool)>, // (duration, size, sync)
    audio: Vec<u32>,              // sizes (duration comes from trex)
    explicit_base: bool,
}

/// One moof + mdat. Video uses per-sample duration, size and flags; audio only sizes, with the trex
/// default duration. With `explicit_base` the base data offset is written out; otherwise the video traf uses
/// default-base-is-moof and the audio traf continues after it.
fn fragment(at: u64, f: &Frag, first_video: u32, first_audio: u32) -> Vec<u8> {
    // Build once with zero offsets to learn the moof size, then for real.
    let build = |moof_len: u32| {
        let data_start = at + u64::from(moof_len) + 8;
        let mut video_trun = full(0, 0x701);
        video_trun.extend(be(f.video.len() as u32));
        let v_off = (data_start - at) as u32;
        video_trun.extend(be(if f.explicit_base { 0 } else { v_off }));
        for &(d, s, sync) in &f.video {
            video_trun.extend(be(d));
            video_trun.extend(be(s));
            video_trun.extend(be(if sync { 0 } else { 0x0001_0000 }));
        }
        let video_bytes: u32 = f.video.iter().map(|v| v.1).sum();
        let mut v_tfhd = full(0, if f.explicit_base { 0x1 } else { 0x2_0000 });
        v_tfhd.extend(be(1));
        if f.explicit_base {
            v_tfhd.extend(data_start.to_be_bytes());
        }
        let mut audio_trun = full(0, if f.explicit_base { 0x201 } else { 0x200 });
        audio_trun.extend(be(f.audio.len() as u32));
        if f.explicit_base {
            audio_trun.extend(be(0));
        }
        for &s in &f.audio {
            audio_trun.extend(be(s));
        }
        let mut a_tfhd = full(0, if f.explicit_base { 0x1 } else { 0 });
        a_tfhd.extend(be(2));
        if f.explicit_base {
            a_tfhd.extend((data_start + u64::from(video_bytes)).to_be_bytes());
        }
        let mut mfhd = full(0, 0);
        mfhd.extend(be(1));
        bx(
            b"moof",
            &cat(&[
                bx(b"mfhd", &mfhd),
                bx(b"traf", &cat(&[bx(b"tfhd", &v_tfhd), bx(b"trun", &video_trun)])),
                bx(b"traf", &cat(&[bx(b"tfhd", &a_tfhd), bx(b"trun", &audio_trun)])),
            ]),
        )
    };
    let moof = build(build(0).len() as u32);
    let mut data = Vec::new();
    for (i, &(_, s, _)) in f.video.iter().enumerate() {
        data.extend(payload(true, first_video + i as u32, s));
    }
    for (i, &s) in f.audio.iter().enumerate() {
        data.extend(payload(false, first_audio + i as u32, s));
    }
    cat(&[moof, bx(b"mdat", &data)])
}

fn fragmented_file() -> Vec<u8> {
    let mut file = cat(&[bx(b"ftyp", b"isom\0\0\0\0"), moov()]);
    let frags = [
        Frag {
            video: vec![(1000, 30, true), (1000, 10, false), (1000, 12, false)],
            audio: vec![7, 8],
            explicit_base: false,
        },
        Frag {
            video: vec![(1000, 25, true), (2000, 9, false)],
            audio: vec![6, 6, 6],
            explicit_base: true,
        },
    ];
    let (mut v, mut a) = (1, 1);
    for f in &frags {
        let at = file.len() as u64;
        file.extend(fragment(at, f, v, a));
        v += f.video.len() as u32;
        a += f.audio.len() as u32;
    }
    file
}

fn write_temp(bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a_recording.mp4");
    std::fs::write(&path, bytes).unwrap();
    (dir, path)
}

fn top(bytes: &[u8]) -> Vec<([u8; 4], usize, usize)> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos + 8 <= bytes.len() {
        let mut size = u32::from_be_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        if size == 1 {
            size = u64::from_be_bytes(bytes[pos + 8..pos + 16].try_into().unwrap()) as usize;
        }
        out.push((bytes[pos + 4..pos + 8].try_into().unwrap(), pos, size));
        pos += size;
    }
    out
}

fn table(stbl: &[u8], typ: &[u8; 4]) -> Vec<u32> {
    let b = child(stbl, typ).unwrap();
    b[4..]
        .chunks(4)
        .map(|c| u32::from_be_bytes(c.try_into().unwrap()))
        .collect()
}

fn stbl_of(moov: &[u8], n: usize) -> Vec<u8> {
    let trak = children(moov)
        .unwrap()
        .into_iter()
        .filter(|(t, _)| t == b"trak")
        .nth(n)
        .unwrap()
        .1;
    child(child(child(trak, b"mdia").unwrap(), b"minf").unwrap(), b"stbl")
        .unwrap()
        .to_vec()
}

#[test]
fn a_fragmented_recording_becomes_a_regular_mp4_with_the_same_samples() {
    let original = fragmented_file();
    let (_dir, path) = write_temp(&original);
    assert!(is_fragmented(&path).unwrap());
    assert!(make_regular(&path).unwrap());
    assert!(!is_fragmented(&path).unwrap());

    let out = std::fs::read(&path).unwrap();
    // The samples weren't moved: the original bytes are still there, only headers changed.
    assert!(out.len() > original.len());
    let kinds: Vec<[u8; 4]> = top(&out).iter().map(|b| b.0).collect();
    assert_eq!(kinds, [*b"ftyp", *b"free", *b"mdat", *b"moov"]);
    let boxes = top(&out);
    let (_, moov_at, moov_len) = boxes[3];
    assert_eq!(moov_at, original.len());
    assert_eq!(
        boxes[2].1 + boxes[2].2,
        moov_at,
        "the mdat ends where the new index starts"
    );
    let moov = &out[moov_at + 8..moov_at + moov_len];
    assert!(children(moov).unwrap().iter().all(|(t, _)| t != b"mvex"));

    // Video: 5 samples, sync at 1 and 4, durations 1000 ×4 and 2000.
    let v = stbl_of(moov, 0);
    let sizes = table(&v, b"stsz");
    assert_eq!(&sizes[..2], &[0, 5]);
    assert_eq!(&sizes[2..], &[30, 10, 12, 25, 9]);
    assert_eq!(table(&v, b"stss"), vec![2, 1, 4]);
    assert_eq!(table(&v, b"stts"), vec![2, 4, 1000, 1, 2000]);
    // Audio: 5 samples, all sync (no stss), the trex default duration.
    let a = stbl_of(moov, 1);
    assert_eq!(&table(&a, b"stsz")[2..], &[7, 8, 6, 6, 6]);
    assert!(child(&a, b"stss").is_err());
    assert_eq!(table(&a, b"stts"), vec![1, 5, 1024]);

    // Every chunk offset points at the right sample bytes.
    for (n, video) in [(0, true), (1, false)] {
        let stbl = stbl_of(moov, n);
        let co64 = child(&stbl, b"co64").unwrap();
        let count = u32::from_be_bytes(co64[4..8].try_into().unwrap()) as usize;
        let stsc = table(&stbl, b"stsc");
        let sizes = &table(&stbl, b"stsz")[2..];
        let per_chunk: Vec<u32> = (0..count)
            .map(|c| {
                let runs: Vec<&[u32]> = stsc[1..].chunks(3).collect();
                runs.iter().rev().find(|r| r[0] as usize <= c + 1).unwrap()[1]
            })
            .collect();
        let mut sample = 0usize;
        for (c, &k) in per_chunk.iter().enumerate() {
            let mut at = u64::from_be_bytes(co64[8 + 8 * c..16 + 8 * c].try_into().unwrap()) as usize;
            for _ in 0..k {
                let s = sizes[sample] as usize;
                assert_eq!(out[at..at + s], payload(video, sample as u32 + 1, s as u32)[..]);
                at += s;
                sample += 1;
            }
        }
        assert_eq!(sample, sizes.len());
    }

    // Durations: video 6000/60000 s = 100 ms; audio 5120/48000 s ≈ 106 ms; the movie is the longer one.
    let mut c = Cursor::new(child(moov, b"mvhd").unwrap());
    assert_eq!(c.u32().unwrap() >> 24, 1);
    c.take(16).unwrap();
    assert_eq!(c.u32().unwrap(), 1000);
    assert_eq!(c.u64().unwrap(), 106);

    // Running it again changes nothing.
    assert!(!make_regular(&path).unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), out);
}

#[test]
fn an_interrupted_run_is_finished_by_the_next_one() {
    let original = fragmented_file();
    let (_dir, path) = write_temp(&original);
    make_regular(&path).unwrap();
    let done = std::fs::read(&path).unwrap();
    let new_index = &done[original.len()..];
    let old_moov_at = top(&original)[1].1;

    // Stopped after appending the index.
    let mut appended = original.clone();
    appended.extend_from_slice(new_index);
    // …and after freeing the old index too.
    let mut freed = appended.clone();
    freed[old_moov_at + 4..old_moov_at + 8].copy_from_slice(b"free");
    for state in [appended, freed] {
        std::fs::write(&path, &state).unwrap();
        assert!(make_regular(&path).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), done);
    }
}

#[test]
fn regular_files_are_left_alone() {
    let regular = cat(&[
        bx(b"ftyp", b"isom\0\0\0\0"),
        bx(b"moov", &bx(b"mvhd", &[0; 100])),
        bx(b"mdat", &[1; 50]),
    ]);
    let (_dir, path) = write_temp(&regular);
    assert!(!is_fragmented(&path).unwrap());
    assert!(!make_regular(&path).unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), regular);
}

#[test]
fn a_cut_off_recording_keeps_its_complete_fragments() {
    let full = fragmented_file();
    let boxes = top(&full);
    let second_moof = boxes.iter().filter(|b| &b.0 == b"moof").nth(1).unwrap().1;
    // Cut inside the last mdat, inside the last moof, and inside an appended index.
    let mut appended = full.clone();
    {
        let (_d, p) = write_temp(&full);
        make_regular(&p).unwrap();
        let done = std::fs::read(&p).unwrap();
        appended.extend_from_slice(&done[full.len()..done.len() - 5]);
    }
    for (cut, frags) in [
        (full[..full.len() - 3].to_vec(), 1),
        (full[..second_moof + 20].to_vec(), 1),
        (appended, 2),
    ] {
        let (_dir, path) = write_temp(&cut);
        assert!(make_regular(&path).unwrap());
        let out = std::fs::read(&path).unwrap();
        let kinds: Vec<[u8; 4]> = top(&out).iter().map(|b| b.0).collect();
        assert_eq!(kinds, [*b"ftyp", *b"free", *b"mdat", *b"moov"]);
        let moov_at = top(&out)[3].1;
        let sizes = table(&stbl_of(&out[moov_at + 8..], 0), b"stsz");
        assert_eq!(sizes[1], if frags == 1 { 3 } else { 5 });
    }
}

#[test]
fn a_file_cut_before_its_first_fragment_is_left_alone() {
    let full = fragmented_file();
    let first_moof = top(&full).iter().find(|b| &b.0 == b"moof").unwrap().1;
    let cut = full[..first_moof + 10].to_vec();
    let (_dir, path) = write_temp(&cut);
    assert!(make_regular(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), cut);
}
