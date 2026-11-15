//! Full recordings: the running pipeline's packets stream into a fragmented MP4 on a writer thread.
//!
//! Recordings share the replay buffer's encoder, so they cost nothing extra on the GPU. Fragmented MP4 means
//! a crash or power cut keeps everything up to the last fragment (about a second).

use std::path::PathBuf;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Instant;

use anyhow::Result;
use clipr_buffer::Packet;
use clipr_mux::{LiveWriter, MediaFormat};

enum Msg {
    Video(Packet),
    Audio(usize, Packet),
    Mark(i64),
    Stop,
}

/// A running recording. Feed it packets; [`Recorder::stop`] finishes the file.
pub struct Recorder {
    tx: mpsc::Sender<Msg>,
    thread: JoinHandle<Result<PathBuf>>,
    pub started: Instant,
    /// Unix seconds, for the UI's timer.
    pub since_unix: u64,
}

impl Recorder {
    pub fn start(path: PathBuf, video: MediaFormat, audio: Vec<MediaFormat>) -> Self {
        let (tx, rx) = mpsc::channel::<Msg>();
        let file = path.clone();
        let thread = std::thread::Builder::new()
            .name("clipr-record".into())
            .spawn(move || write(file, &video, &audio, rx))
            .expect("can't start a thread");
        tracing::info!("recording to {}", path.display());
        Self {
            tx,
            thread,
            started: Instant::now(),
            since_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
        }
    }

    pub fn sink(&self) -> RecordSink {
        RecordSink(self.tx.clone())
    }

    pub fn mark(&self, time: i64) {
        let _ = self.tx.send(Msg::Mark(time));
    }

    /// Finishes the file. Blocks until the writer has flushed it (a fraction of a second).
    pub fn stop(self) -> Result<PathBuf> {
        let _ = self.tx.send(Msg::Stop);
        self.thread
            .join()
            .unwrap_or_else(|_| Err(anyhow::anyhow!("the recording thread crashed")))
    }
}

/// The pipelines' handle for sending packets to the recording.
#[derive(Clone)]
pub struct RecordSink(mpsc::Sender<Msg>);

impl RecordSink {
    pub fn video(&self, packet: &Packet) {
        let _ = self.0.send(Msg::Video(packet.clone()));
    }

    pub fn audio(&self, track: usize, packet: &Packet) {
        let _ = self.0.send(Msg::Audio(track, packet.clone()));
    }
}

fn write(
    path: PathBuf,
    video: &MediaFormat,
    audio: &[MediaFormat],
    rx: mpsc::Receiver<Msg>,
) -> Result<PathBuf> {
    let mut writer = LiveWriter::create(&path, video, audio)?;
    let mut markers = Vec::new();
    let mut first_error: Option<anyhow::Error> = None;
    let mut start: Option<i64> = None;
    for msg in rx {
        let result = match msg {
            Msg::Video(p) => {
                let r = writer.write_video(&p);
                if start.is_none() && writer.started() {
                    start = Some(p.pts);
                }
                r
            }
            Msg::Audio(track, p) => writer.write_audio(track, &p),
            Msg::Mark(t) => {
                markers.push(t);
                Ok(())
            }
            Msg::Stop => break,
        };
        if let Err(e) = result
            && first_error.is_none()
        {
            tracing::error!("recording write failed: {e:#}");
            first_error = Some(e);
        }
    }
    let path = writer.finish()?;
    if let Some(start) = start {
        super::save::write_markers(&path, start, i64::MAX, &markers);
    }
    match first_error {
        Some(e) => Err(e.context(format!("the recording {} may be incomplete", path.display()))),
        None => Ok(path),
    }
}
