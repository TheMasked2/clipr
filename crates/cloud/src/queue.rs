//! The upload queue: which clips go to the bucket, and when.
//!
//! A new clip becomes a job when cloud storage is on (full recordings only if the user wants them). Jobs
//! wait while a game runs unless the timing is "right away", so an upload never costs frames; with
//! "manual" only clips uploaded by hand go. One upload runs at a time. Jobs and the list of uploaded clips
//! live in a JSON file, so a restart doesn't lose them; a cut-off upload starts over.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clipr_core::clip::ClipInfo;
use clipr_core::config::{CloudConfig, UploadTiming};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::{bucket, public_url};

/// How many finished jobs (done, failed or cancelled) the list keeps.
const KEEP_FINISHED: usize = 50;
/// How often a waiting job checks whether the game has closed.
const RECHECK: Duration = Duration::from_secs(5);
/// How often the UI hears about upload progress.
const PROGRESS_EVERY: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    /// Will go up (after the game, unless `now` or the timing says right away).
    Queued,
    Uploading,
    Done,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: u64,
    pub clip: ClipInfo,
    /// Where it goes in the bucket.
    pub key: String,
    pub state: JobState,
    /// Upload even while a game runs (asked for by hand, or for a Discord post).
    #[serde(default)]
    pub now: bool,
    #[serde(default)]
    pub error: Option<String>,
    /// Unix seconds.
    pub created: u64,
    /// Bytes up so far, while uploading.
    #[serde(default, skip_deserializing)]
    pub done_bytes: u64,
}

impl Job {
    fn finished(&self) -> bool {
        matches!(
            self.state,
            JobState::Done | JobState::Failed | JobState::Cancelled
        )
    }
}

/// A clip that's in the bucket.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Uploaded {
    pub key: String,
    pub size: u64,
    /// Unix seconds.
    pub at: u64,
}

/// What the gallery shows for a clip.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CloudStatus {
    /// `uploaded`, `queued`, `uploading` or `failed`.
    pub state: &'static str,
    /// The public link, when uploaded and the bucket has a public address.
    pub url: Option<String>,
    /// 0–1 while uploading.
    pub progress: Option<f64>,
    pub error: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    next_id: u64,
    jobs: Vec<Job>,
    /// Clip id → where it is in the bucket.
    uploaded: BTreeMap<String, Uploaded>,
}

struct Inner {
    file: PathBuf,
    state: Mutex<Saved>,
    wake: Condvar,
    config: Box<dyn Fn() -> CloudConfig + Send + Sync>,
    in_game: Box<dyn Fn() -> bool + Send + Sync>,
    changed: broadcast::Sender<()>,
    /// The job being cancelled while it uploads (0 = none).
    cancel: AtomicU64,
}

/// The upload queue and its worker thread. Cheap to clone.
#[derive(Clone)]
pub struct Uploader {
    inner: Arc<Inner>,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Where `clip` goes in the bucket: the prefix, then its path in the clips folder.
pub fn key_for(config: &CloudConfig, clip_id: &str) -> String {
    format!("{}{clip_id}", crate::prefix(&config.prefix))
}

fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("mkv") => "video/x-matroska",
        _ => "video/mp4",
    }
}

impl Uploader {
    /// Loads the queue from `file` and starts the worker. `config` returns the current cloud settings;
    /// `in_game` whether a game runs.
    pub fn start(
        file: PathBuf,
        config: impl Fn() -> CloudConfig + Send + Sync + 'static,
        in_game: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Self {
        let uploader = Self::load(file, config, in_game);
        let worker = uploader.clone();
        std::thread::Builder::new()
            .name("clipr-cloud".into())
            .spawn(move || worker.run())
            .expect("can't start a thread");
        uploader
    }

    fn load(
        file: PathBuf,
        config: impl Fn() -> CloudConfig + Send + Sync + 'static,
        in_game: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Self {
        let mut saved: Saved = std::fs::read(&file)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        // An upload cut off by quitting starts over.
        for job in &mut saved.jobs {
            if job.state == JobState::Uploading {
                job.state = JobState::Queued;
            }
        }
        Self {
            inner: Arc::new(Inner {
                file,
                state: Mutex::new(saved),
                wake: Condvar::new(),
                config: Box::new(config),
                in_game: Box::new(in_game),
                changed: broadcast::Sender::new(16),
                cancel: AtomicU64::new(0),
            }),
        }
    }

    /// Fires whenever the jobs or the uploaded clips change, and with upload progress (for the UI).
    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.inner.changed.subscribe()
    }

    /// Every job, newest first.
    pub fn jobs(&self) -> Vec<Job> {
        let mut jobs = self.lock().jobs.clone();
        jobs.reverse();
        jobs
    }

    /// The cloud state of every clip that has one.
    pub fn statuses(&self) -> BTreeMap<String, CloudStatus> {
        let config = (self.inner.config)();
        let s = self.lock();
        let mut out: BTreeMap<String, CloudStatus> = s
            .uploaded
            .iter()
            .map(|(id, u)| {
                (
                    id.clone(),
                    CloudStatus {
                        state: "uploaded",
                        url: public_url(&config, &u.key),
                        progress: None,
                        error: None,
                    },
                )
            })
            .collect();
        // The newest job for a clip says what's going on with it.
        let mut newest: BTreeMap<&str, &Job> = BTreeMap::new();
        for job in &s.jobs {
            newest.insert(job.clip.id.as_str(), job);
        }
        for (id, job) in newest {
            let (state, progress) = match job.state {
                JobState::Queued => ("queued", None),
                JobState::Uploading => (
                    "uploading",
                    Some(if job.clip.size_bytes == 0 {
                        0.0
                    } else {
                        (job.done_bytes as f64 / job.clip.size_bytes as f64).min(1.0)
                    }),
                ),
                JobState::Failed if !s.uploaded.contains_key(id) => ("failed", None),
                _ => continue,
            };
            out.insert(
                id.to_string(),
                CloudStatus {
                    state,
                    url: None,
                    progress,
                    error: job.error.clone(),
                },
            );
        }
        out
    }

    /// A new clip was saved: queue it if cloud storage is on and the timing isn't manual.
    pub fn clip_saved(&self, clip: &ClipInfo) {
        let config = (self.inner.config)();
        if !config.ready() || config.upload == UploadTiming::Manual {
            return;
        }
        if clip.kind == "recording" && !config.recordings {
            return;
        }
        let mut s = self.lock();
        if s.uploaded.contains_key(&clip.id) {
            return;
        }
        let key = key_for(&config, &clip.id);
        Self::push(&mut s, clip.clone(), key, false);
        self.save_and_wake(s);
    }

    /// The user uploads `clip` by hand (or retries it): it goes right away, game or not.
    pub fn upload(&self, clip: ClipInfo) -> Result<u64> {
        let config = (self.inner.config)();
        if !config.ready() {
            bail!("Set up cloud storage first, in Settings → Cloud");
        }
        let mut s = self.lock();
        if s.uploaded.contains_key(&clip.id) {
            bail!("This clip is already in the cloud");
        }
        if let Some(job) = s
            .jobs
            .iter_mut()
            .find(|j| j.clip.id == clip.id && matches!(j.state, JobState::Queued | JobState::Uploading))
        {
            job.now = true;
            let id = job.id;
            self.save_and_wake(s);
            return Ok(id);
        }
        let key = key_for(&config, &clip.id);
        let id = Self::push(&mut s, clip, key, true);
        self.save_and_wake(s);
        Ok(id)
    }

    /// Stops a queued or running upload.
    pub fn cancel(&self, job_id: u64) -> Result<()> {
        let mut s = self.lock();
        let job = s
            .jobs
            .iter_mut()
            .find(|j| j.id == job_id)
            .context("That upload isn't in the list any more")?;
        match job.state {
            JobState::Queued => job.state = JobState::Cancelled,
            // The worker stops at its next read and marks it cancelled.
            JobState::Uploading => self.inner.cancel.store(job_id, Ordering::SeqCst),
            _ => bail!("That upload already finished"),
        }
        self.save_and_wake(s);
        Ok(())
    }

    /// Removes `clip_id` from the bucket (the file on this PC stays). Blocks on the network.
    pub fn remove(&self, clip_id: &str) -> Result<()> {
        let config = (self.inner.config)();
        let Some(uploaded) = self.lock().uploaded.get(clip_id).cloned() else {
            bail!("This clip isn't in the cloud");
        };
        if !config.ready() {
            bail!("Cloud storage is off or not set up, so the clip can't be removed from the bucket");
        }
        bucket(&config).delete(&uploaded.key)?;
        let mut s = self.lock();
        s.uploaded.remove(clip_id);
        self.save_and_wake(s);
        Ok(())
    }

    /// A clip was renamed (its id and path change): its cloud copy and open jobs follow it. The object in
    /// the bucket keeps its name, so links already shared keep working.
    pub fn clip_renamed(&self, old_id: &str, clip: &ClipInfo) {
        let mut s = self.lock();
        let mut changed = false;
        if let Some(u) = s.uploaded.remove(old_id) {
            s.uploaded.insert(clip.id.clone(), u);
            changed = true;
        }
        for job in s
            .jobs
            .iter_mut()
            .filter(|j| j.clip.id == old_id && j.state == JobState::Queued)
        {
            job.clip = clip.clone();
            changed = true;
        }
        if changed {
            self.save_and_wake(s);
        }
    }

    /// A clip's file was replaced (trimmed in place): its copy in the bucket is out of date, so it goes up
    /// again under the same name (shared links keep working and show the new version).
    pub fn clip_replaced(&self, clip: &ClipInfo) {
        let config = (self.inner.config)();
        let mut s = self.lock();
        let Some(old) = s.uploaded.remove(&clip.id) else {
            return;
        };
        if config.ready() {
            Self::push(&mut s, clip.clone(), old.key, false);
        }
        self.save_and_wake(s);
    }

    /// The public link for `clip` for a Discord post: uploads it now if it isn't up yet and waits (up to
    /// `wait`). `None` when cloud storage isn't set up or the bucket has no public address.
    pub fn link(&self, clip: &ClipInfo, wait: Duration) -> Result<Option<String>> {
        let config = (self.inner.config)();
        if !config.ready() || config.public_url.trim().is_empty() {
            return Ok(None);
        }
        let job_id = {
            let s = self.lock();
            if let Some(u) = s.uploaded.get(&clip.id) {
                return Ok(public_url(&config, &u.key));
            }
            drop(s);
            self.upload(clip.clone())?
        };
        let until = Instant::now() + wait;
        let mut s = self.lock();
        loop {
            if let Some(u) = s.uploaded.get(&clip.id) {
                return Ok(public_url(&config, &u.key));
            }
            match s.jobs.iter().find(|j| j.id == job_id) {
                Some(j) if j.state == JobState::Failed => {
                    bail!(
                        "Uploading to the cloud failed: {}",
                        j.error.as_deref().unwrap_or("unknown error")
                    )
                }
                Some(j) if j.state == JobState::Cancelled => bail!("The upload to the cloud was cancelled"),
                None => bail!("The upload to the cloud went missing"),
                _ => {}
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                bail!("Uploading to the cloud is taking too long");
            }
            s = self
                .inner
                .wake
                .wait_timeout(s, left.min(RECHECK))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    fn push(s: &mut Saved, clip: ClipInfo, key: String, now: bool) -> u64 {
        s.next_id += 1;
        let id = s.next_id;
        s.jobs.push(Job {
            id,
            clip,
            key,
            state: JobState::Queued,
            now,
            error: None,
            created: now_secs(),
            done_bytes: 0,
        });
        let finished = s.jobs.iter().filter(|j| j.finished()).count();
        let mut drop = finished.saturating_sub(KEEP_FINISHED);
        s.jobs.retain(|j| {
            if drop > 0 && j.finished() {
                drop -= 1;
                false
            } else {
                true
            }
        });
        id
    }

    fn lock(&self) -> MutexGuard<'_, Saved> {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save_and_wake(&self, s: MutexGuard<'_, Saved>) {
        if let Err(e) = save(&self.inner.file, &s) {
            tracing::warn!("can't save the cloud upload queue: {e:#}");
        }
        drop(s);
        self.inner.wake.notify_all();
        let _ = self.inner.changed.send(());
    }

    /// Whether `job` may go now.
    fn may_go(job: &Job, config: &CloudConfig, in_game: bool) -> bool {
        job.state == JobState::Queued
            && config.ready()
            && (job.now
                || match config.upload {
                    UploadTiming::Now => true,
                    UploadTiming::AfterGame => !in_game,
                    UploadTiming::Manual => false,
                })
    }

    /// The next job to upload now, marked as uploading.
    fn take(&self) -> (Job, CloudConfig) {
        let mut s = self.lock();
        loop {
            let config = (self.inner.config)();
            let in_game = (self.inner.in_game)();
            // Asked-for uploads (by hand, for Discord) go first.
            let pick = s
                .jobs
                .iter()
                .position(|j| j.now && Self::may_go(j, &config, in_game))
                .or_else(|| s.jobs.iter().position(|j| Self::may_go(j, &config, in_game)));
            if let Some(i) = pick {
                s.jobs[i].state = JobState::Uploading;
                s.jobs[i].done_bytes = 0;
                let job = s.jobs[i].clone();
                self.inner.cancel.store(0, Ordering::SeqCst);
                self.save_and_wake(s);
                return (job, config);
            }
            s = self
                .inner
                .wake
                .wait_timeout(s, RECHECK)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    fn run(&self) {
        loop {
            let (job, config) = self.take();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.send(&job, &config)))
                .unwrap_or_else(|_| {
                    Err(anyhow::anyhow!(
                        "clipr hit an internal error while uploading; try again"
                    ))
                });
            let cancelled = self.inner.cancel.swap(0, Ordering::SeqCst) == job.id;
            let mut s = self.lock();
            let size = std::fs::metadata(&job.clip.path).map_or(job.clip.size_bytes, |m| m.len());
            match &result {
                Ok(()) => {
                    tracing::info!("uploaded {} to the cloud", job.clip.title);
                    // A rename while it uploaded moved the clip: file it under its current id.
                    let id = s
                        .jobs
                        .iter()
                        .find(|j| j.id == job.id)
                        .map_or(job.clip.id.clone(), |j| j.clip.id.clone());
                    s.uploaded.insert(
                        id,
                        Uploaded {
                            key: job.key.clone(),
                            size,
                            at: now_secs(),
                        },
                    );
                }
                Err(e) if cancelled => tracing::info!("upload of {} cancelled ({e:#})", job.clip.title),
                Err(e) => tracing::warn!("couldn't upload {} to the cloud: {e:#}", job.clip.title),
            }
            if let Some(j) = s.jobs.iter_mut().find(|j| j.id == job.id) {
                j.done_bytes = 0;
                match &result {
                    Ok(()) => {
                        j.state = JobState::Done;
                        j.error = None;
                    }
                    Err(_) if cancelled => j.state = JobState::Cancelled,
                    Err(e) => {
                        j.state = JobState::Failed;
                        j.error = Some(format!("{e:#}"));
                    }
                }
            }
            self.save_and_wake(s);
        }
    }

    fn send(&self, job: &Job, config: &CloudConfig) -> Result<()> {
        if !job.clip.path.is_file() {
            bail!("The clip isn't there any more");
        }
        let mut last = Instant::now();
        let mut progress = |done: u64| {
            if last.elapsed() >= PROGRESS_EVERY {
                last = Instant::now();
                if let Some(j) = self.lock().jobs.iter_mut().find(|j| j.id == job.id) {
                    j.done_bytes = done;
                }
                let _ = self.inner.changed.send(());
            }
        };
        let cancelled = || self.inner.cancel.load(Ordering::SeqCst) == job.id;
        bucket(config).put_file(
            &job.key,
            &job.clip.path,
            content_type(&job.clip.path),
            u64::from(config.max_mbps) * 125_000,
            &mut progress,
            &cancelled,
        )
    }
}

/// Writes the queue atomically (temporary file, then rename).
fn save(file: &Path, s: &Saved) -> Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(s)?)?;
    std::fs::rename(&tmp, file)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready() -> CloudConfig {
        CloudConfig {
            enabled: true,
            endpoint: "https://acc.r2.cloudflarestorage.com".into(),
            bucket: "clips".into(),
            access_key_id: "k".into(),
            secret_access_key: "s".into(),
            public_url: "https://pub-1.r2.dev".into(),
            ..CloudConfig::default()
        }
    }

    fn clip(id: &str, kind: &str) -> ClipInfo {
        ClipInfo {
            id: id.into(),
            path: PathBuf::from("C:/nope").join(id),
            title: id.into(),
            game: "Game".into(),
            duration_secs: Some(10.0),
            size_bytes: 1000,
            thumb: None,
            kind: kind.into(),
        }
    }

    fn uploader(dir: &Path, config: CloudConfig) -> Uploader {
        let config = Arc::new(Mutex::new(config));
        Uploader::load(
            dir.join("q.json"),
            move || config.lock().unwrap().clone(),
            || true,
        )
    }

    #[test]
    fn new_clips_queue_by_the_settings() {
        let dir = tempfile::tempdir().unwrap();
        let up = uploader(dir.path(), ready());
        up.clip_saved(&clip("Game/a.mp4", "clip"));
        up.clip_saved(&clip("Game/r.mp4", "recording"));
        let jobs = up.jobs();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].key, "clipr/Game/a.mp4");
        assert_eq!(up.statuses()["Game/a.mp4"].state, "queued");

        let off = uploader(
            dir.path(),
            CloudConfig {
                enabled: false,
                ..ready()
            },
        );
        off.clip_saved(&clip("Game/b.mp4", "clip"));
        assert!(off.jobs().iter().all(|j| j.clip.id != "Game/b.mp4"));
        let manual = uploader(
            tempfile::tempdir().unwrap().path(),
            CloudConfig {
                upload: UploadTiming::Manual,
                ..ready()
            },
        );
        manual.clip_saved(&clip("Game/c.mp4", "clip"));
        assert!(manual.jobs().is_empty());
        assert!(
            manual.upload(clip("Game/c.mp4", "clip")).is_ok(),
            "by hand still works"
        );
    }

    #[test]
    fn timing_decides_what_may_go() {
        let job = |now| Job {
            id: 1,
            clip: clip("a", "clip"),
            key: "k".into(),
            state: JobState::Queued,
            now,
            error: None,
            created: 0,
            done_bytes: 0,
        };
        let after = ready();
        assert!(!Uploader::may_go(&job(false), &after, true), "waits for the game");
        assert!(Uploader::may_go(&job(false), &after, false));
        assert!(Uploader::may_go(&job(true), &after, true), "by hand goes now");
        let now = CloudConfig {
            upload: UploadTiming::Now,
            ..ready()
        };
        assert!(Uploader::may_go(&job(false), &now, true));
        let manual = CloudConfig {
            upload: UploadTiming::Manual,
            ..ready()
        };
        assert!(!Uploader::may_go(&job(false), &manual, false));
        let off = CloudConfig {
            enabled: false,
            ..ready()
        };
        assert!(
            !Uploader::may_go(&job(true), &off, false),
            "nothing goes while it's off"
        );
    }

    #[test]
    fn uploaded_clips_follow_renames_and_survive_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let up = uploader(dir.path(), ready());
        {
            let mut s = up.lock();
            s.uploaded.insert(
                "Game/a.mp4".into(),
                Uploaded {
                    key: "clipr/Game/a.mp4".into(),
                    size: 5,
                    at: 1,
                },
            );
            up.save_and_wake(s);
        }
        up.clip_renamed("Game/a.mp4", &clip("Game/b c.mp4", "clip"));
        let st = &up.statuses()["Game/b c.mp4"];
        assert_eq!(st.state, "uploaded");
        // The object keeps its name, so shared links keep working.
        assert_eq!(st.url.as_deref(), Some("https://pub-1.r2.dev/clipr/Game/a.mp4"));
        assert!(up.upload(clip("Game/b c.mp4", "clip")).is_err(), "already up");

        let again = uploader(dir.path(), ready());
        assert_eq!(again.statuses()["Game/b c.mp4"].state, "uploaded");
        let link = again
            .link(&clip("Game/b c.mp4", "clip"), Duration::from_secs(1))
            .unwrap();
        assert_eq!(link.as_deref(), Some("https://pub-1.r2.dev/clipr/Game/a.mp4"));
        let private = uploader(
            dir.path(),
            CloudConfig {
                public_url: String::new(),
                ..ready()
            },
        );
        assert_eq!(
            private
                .link(&clip("Game/b c.mp4", "clip"), Duration::from_secs(1))
                .unwrap(),
            None
        );
    }

    #[test]
    fn cancel_and_retry() {
        let dir = tempfile::tempdir().unwrap();
        let up = uploader(dir.path(), ready());
        up.clip_saved(&clip("Game/a.mp4", "clip"));
        let id = up.jobs()[0].id;
        up.cancel(id).unwrap();
        assert_eq!(up.jobs()[0].state, JobState::Cancelled);
        assert!(!up.statuses().contains_key("Game/a.mp4"));
        assert!(up.cancel(id).is_err());
        let again = up.upload(clip("Game/a.mp4", "clip")).unwrap();
        assert_ne!(again, id);
        assert!(up.jobs()[0].now);
        // Asking again doesn't queue it twice.
        assert_eq!(up.upload(clip("Game/a.mp4", "clip")).unwrap(), again);

        // A cut-off upload starts over after a restart.
        {
            let mut s = up.lock();
            s.jobs.last_mut().unwrap().state = JobState::Uploading;
            up.save_and_wake(s);
        }
        let after = uploader(dir.path(), ready());
        assert_eq!(after.jobs()[0].state, JobState::Queued);
    }
}
