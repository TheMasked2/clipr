//! Full recordings: the running pipeline's packets stream into a fragmented MP4 on a writer thread.
//!
//! Recordings share the replay buffer's encoder, so they cost nothing extra on the GPU. The file is written
//! as `<name>.mp4.part` and renamed when it's finished. Fragmented MP4 means a crash or power cut keeps
//! everything up to the last fragment (about a second); [`recover`] renames such leftovers at startup.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use anyhow::Result;
use clipr_buffer::Packet;
use clipr_mux::{LiveWriter, MediaFormat};

/// Packets the writer may fall behind by (about 20 s at 60 fps with audio) before the recording stops, so
/// a disk that can't keep up can't eat all the memory.
const QUEUE: usize = 4096;

enum Msg {
    Video(Packet),
    Audio(usize, Packet),
    Mark(i64),
    Stop,
}

/// What went wrong while recording, set by the writer thread or the sink; the supervisor checks it.
type Failure = Arc<Mutex<Option<String>>>;

fn set_failure(failure: &Failure, message: String) {
    let mut f = failure.lock().unwrap();
    if f.is_none() {
        tracing::error!("recording: {message}");
        *f = Some(message);
    }
}

/// A running recording. Feed it packets; [`Recorder::stop`] finishes the file.
pub struct Recorder {
    tx: mpsc::SyncSender<Msg>,
    thread: JoinHandle<Result<Option<PathBuf>>>,
    failure: Failure,
    pub started: Instant,
    /// Unix seconds, for the UI's timer.
    pub since_unix: u64,
}

impl Recorder {
    pub fn start(path: PathBuf, video: MediaFormat, audio: Vec<MediaFormat>) -> Self {
        let (tx, rx) = mpsc::sync_channel::<Msg>(QUEUE);
        let failure = Failure::default();
        let file = path.clone();
        let writer_failure = failure.clone();
        let thread = std::thread::Builder::new()
            .name("clipr-record".into())
            .spawn(move || write(file, &video, &audio, rx, &writer_failure))
            .expect("can't start a thread");
        tracing::info!("recording to {}", path.display());
        Self {
            tx,
            thread,
            failure,
            started: Instant::now(),
            since_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
        }
    }

    pub fn sink(&self) -> RecordSink {
        RecordSink {
            tx: self.tx.clone(),
            failure: self.failure.clone(),
        }
    }

    pub fn mark(&self, time: i64) {
        let _ = self.tx.try_send(Msg::Mark(time));
    }

    /// Why the recording can't continue, if it can't (the writer failed or fell too far behind).
    pub fn failure(&self) -> Option<String> {
        if self.thread.is_finished() && self.failure.lock().unwrap().is_none() {
            return Some("the recording stopped writing".into());
        }
        self.failure.lock().unwrap().clone()
    }

    /// Finishes the file. Blocks until the writer has flushed it (a fraction of a second). `None`: nothing
    /// was recorded yet (stopped before the first keyframe), so there's no file.
    pub fn stop(self) -> Result<Option<PathBuf>> {
        let _ = self.tx.send(Msg::Stop);
        self.thread
            .join()
            .unwrap_or_else(|_| Err(anyhow::anyhow!("the recording thread crashed")))
    }
}

/// The pipelines' handle for sending packets to the recording. It never blocks the capture.
#[derive(Clone)]
pub struct RecordSink {
    tx: mpsc::SyncSender<Msg>,
    failure: Failure,
}

impl RecordSink {
    pub fn video(&self, packet: &Packet) {
        self.send(Msg::Video(packet.clone()));
    }

    pub fn audio(&self, track: usize, packet: &Packet) {
        self.send(Msg::Audio(track, packet.clone()));
    }

    fn send(&self, msg: Msg) {
        if let Err(TrySendError::Full(_)) = self.tx.try_send(msg) {
            set_failure(&self.failure, "the disk can't keep up with the recording".into());
        }
    }
}

fn write(
    path: PathBuf,
    video: &MediaFormat,
    audio: &[MediaFormat],
    rx: mpsc::Receiver<Msg>,
    failure: &Failure,
) -> Result<Option<PathBuf>> {
    let mut writer = match LiveWriter::create(&path, video, audio) {
        Ok(w) => w,
        Err(e) => {
            set_failure(failure, format!("{e:#}"));
            return Err(e);
        }
    };
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
            set_failure(failure, format!("writing failed: {e:#}"));
            first_error = Some(e);
        }
    }
    let Some(start) = start else {
        // Stopped before the first keyframe: no file worth keeping.
        drop(writer);
        let _ = std::fs::remove_file(part_of(&path));
        return Ok(None);
    };
    let path = writer.finish()?;
    make_regular(&path);
    super::save::write_markers(&path, start, i64::MAX, &markers);
    match first_error {
        Some(e) => Err(e.context(format!("the recording {} may be incomplete", path.display()))),
        None => Ok(Some(path)),
    }
}

fn part_of(path: &Path) -> PathBuf {
    let mut part = path.as_os_str().to_owned();
    part.push(".part");
    PathBuf::from(part)
}

/// Turns a finished recording into a regular MP4 (see [`clipr_mux::make_regular`]): long fragmented files
/// take minutes to open in a browser. A failure keeps the fragmented file, which still plays.
fn make_regular(path: &Path) {
    match clipr_mux::make_regular(path) {
        Ok(true) => tracing::info!("indexed the recording {}", path.display()),
        Ok(false) => {}
        Err(e) => tracing::warn!(
            "couldn't index the recording {} (it still plays): {e:#}",
            path.display()
        ),
    }
}

/// Recordings made before beta.4 are fragmented: index them once, in the background.
fn index_old_recordings(clips_dir: PathBuf) {
    let spawned = std::thread::Builder::new()
        .name("clipr-index".into())
        .spawn(move || {
            for path in recordings(&clips_dir) {
                if clipr_mux::is_fragmented(&path).unwrap_or(false) {
                    make_regular(&path);
                }
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("couldn't start indexing old recordings: {e}");
    }
}

/// Finished recordings (`*_recording.mp4`), one folder level per game.
fn recordings(clips_dir: &Path) -> Vec<PathBuf> {
    let Ok(games) = std::fs::read_dir(clips_dir) else {
        return Vec::new();
    };
    games
        .flatten()
        .filter_map(|g| std::fs::read_dir(g.path()).ok())
        .flat_map(|files| files.flatten().map(|f| f.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with("_recording.mp4") && !n.starts_with('.'))
        })
        .collect()
}

/// Renames recordings left as `*_recording.mp4.part` by a crash (one folder level per game) so they show
/// up in the gallery, then indexes them and older recordings in the background. Fragmented MP4 plays fine
/// up to the last complete fragment.
pub fn recover(clips_dir: &Path) {
    rename_parts(clips_dir);
    index_old_recordings(clips_dir.to_path_buf());
}

fn rename_parts(clips_dir: &Path) {
    let Ok(games) = std::fs::read_dir(clips_dir) else {
        return;
    };
    for game in games.flatten() {
        let Ok(files) = std::fs::read_dir(game.path()) else {
            continue;
        };
        for file in files.flatten() {
            let part = file.path();
            let name = part.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            let Some(done) = name.strip_suffix(".part") else {
                continue;
            };
            if !done.contains("_recording") || !done.ends_with(".mp4") {
                continue;
            }
            let target = part.with_file_name(done);
            if target.exists() {
                continue;
            }
            match std::fs::rename(&part, &target) {
                Ok(()) => tracing::info!("recovered an unfinished recording: {}", target.display()),
                Err(e) => tracing::warn!("couldn't recover {}: {e}", part.display()),
            }
        }
    }
}
