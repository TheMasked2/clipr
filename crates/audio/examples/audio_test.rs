//! Records system audio (plus the microphone, per-app tracks, ...) into an MP4 and checks the packet timing.
//!
//! `cargo run -p clipr-audio --example audio_test -- [seconds] [--mic] [--separate] [--app exe]...
//! [--no-system] [--ptt KEY] [--filter KIND]... [--out file.mp4]`

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use clipr_audio::{AudioCapture, AudioPacket};
use clipr_core::clock::now_100ns;
use clipr_core::config::{AudioConfig, AudioFilter, FilterKind, MIC_SOURCE};
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, MF_SINK_WRITER_DISABLE_THROTTLING, MF_VERSION, MFCreateAttributes, MFCreateMemoryBuffer,
    MFCreateSample, MFCreateSinkWriterFromURL, MFSTARTUP_FULL, MFShutdown, MFStartup,
};
use windows::core::HSTRING;

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    let mut seconds = 10.0f64;
    let mut config = AudioConfig::default();
    let mut out = None;
    let mut mic_filters = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--mic" => config.mic = true,
            "--separate" => config.separate_tracks = true,
            "--no-system" => config.system = false,
            // Mic filters, in the order given: --filter noise_suppression --filter limiter ...
            "--filter" => {
                let name = args.next().context("--filter needs a kind")?;
                let kind = FilterKind::defaults(&name).with_context(|| format!("no filter called {name}"))?;
                mic_filters.push(AudioFilter { enabled: true, kind });
            }
            "--app" => config.apps.push(args.next().context("--app needs an exe name")?),
            "--ptt" => config.push_to_talk = args.next().context("--ptt needs a key")?,
            "--out" => out = args.next(),
            n => {
                seconds = n
                    .parse()
                    .context("usage: audio_test [seconds] [options] [--out file.mp4]")?
            }
        }
    }
    config.filters.insert(MIC_SOURCE.into(), mic_filters);
    let out = out.unwrap_or_else(|| "audio_test.mp4".into());

    let packets = Arc::new(Mutex::new(Vec::<AudioPacket>::new()));
    let sink = packets.clone();
    let before = now_100ns();
    let capture = AudioCapture::start(&config, move |p| sink.lock().unwrap().push(p))?;
    let started = now_100ns();
    println!("start() took {:.1} ms", (started - before) as f64 / 1e4);
    let tracks = capture.tracks();
    println!("tracks: {tracks:?}");

    // SAFETY: balanced by MFShutdown at the end of main.
    unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }?;
    let media_types = (0..tracks.len())
        .map(|t| capture.media_type(t))
        .collect::<Result<Vec<_>>>()?;
    // Print the meters a few times while recording.
    let end = std::time::Instant::now() + Duration::from_secs_f64(seconds);
    let mut next_meter = 0;
    while std::time::Instant::now() < end {
        std::thread::sleep(Duration::from_millis(100));
        next_meter += 1;
        if next_meter % 10 == 0 {
            let levels: Vec<String> = capture
                .levels()
                .iter()
                .map(|l| format!("{} {:.0} → {:.0} dB", l.name, l.db, l.after))
                .collect();
            println!("levels: {}", levels.join(", "));
        }
    }
    let stopped = now_100ns();
    capture.stop();

    let packets = std::mem::take(&mut *packets.lock().unwrap());
    anyhow::ensure!(!packets.is_empty(), "no packets");
    let first = packets.iter().map(|p| p.pts_100ns).min().unwrap();
    println!(
        "first pts is {:.1} ms after the call to start()",
        (first - before) as f64 / 1e4
    );
    for (t, name) in tracks.iter().enumerate() {
        let mine: Vec<&AudioPacket> = packets.iter().filter(|p| p.track == t).collect();
        anyhow::ensure!(!mine.is_empty(), "track {name} has no packets");
        let gaps = mine
            .windows(2)
            .filter(|w| (w[1].pts_100ns - (w[0].pts_100ns + w[0].duration_100ns)).abs() > 1)
            .count();
        let last = mine.last().unwrap();
        let span = (last.pts_100ns + last.duration_100ns - mine[0].pts_100ns) as f64 / 1e7;
        let bytes: usize = mine.iter().map(|p| p.data.len()).sum();
        println!(
            "track {t} ({name}): {} packets, {span:.3} s, {:.0} kbps, {gaps} discontinuities, ends {:.1} ms after stop()",
            mine.len(),
            bytes as f64 * 8.0 / span / 1000.0,
            ((last.pts_100ns + last.duration_100ns) - stopped) as f64 / 1e4
        );
    }

    // SAFETY: valid URL and types; the same type in and out makes a passthrough stream.
    unsafe {
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 1)?;
        let attrs = attrs.context("no attributes")?;
        attrs.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;
        let writer = MFCreateSinkWriterFromURL(&HSTRING::from(out.as_str()), None, &attrs)?;
        let mut streams = Vec::new();
        for t in &media_types {
            let stream = writer.AddStream(t)?;
            writer.SetInputMediaType(stream, t, None)?;
            streams.push(stream);
        }
        writer.BeginWriting()?;
        let mut ordered: Vec<&AudioPacket> = packets.iter().collect();
        ordered.sort_by_key(|p| p.pts_100ns);
        for p in ordered {
            let buffer = MFCreateMemoryBuffer(p.data.len() as u32)?;
            let mut data = std::ptr::null_mut();
            buffer.Lock(&mut data, None, None)?;
            std::ptr::copy_nonoverlapping(p.data.as_ptr(), data, p.data.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(p.data.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(p.pts_100ns - first)?;
            sample.SetSampleDuration(p.duration_100ns)?;
            writer.WriteSample(streams[p.track], &sample)?;
        }
        writer.Finalize()?;
        // Release everything that came from Media Foundation before shutting it down.
        drop(writer);
        drop(media_types);
        MFShutdown()?;
    }
    println!("wrote {out}");
    Ok(())
}
