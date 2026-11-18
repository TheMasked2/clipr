//! The post queue: which clips go to which webhook, and when.
//!
//! A new clip becomes a job for each webhook that wants it: `Ask` jobs wait for a yes in the UI, `Auto`
//! jobs are posted. Posting waits while a game runs (unless the user allows it), so an upload never costs
//! frames. A clip over the upload limit is sent as a smaller copy made by [`clipr_mux::shrink`]. Jobs live
//! in a JSON file, so a restart doesn't lose them. The queue names its webhook; the URL is looked up when
//! posting, so it isn't copied into the queue file.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{Context, Result, bail};
pub use clipr_core::clip::ClipInfo;
use clipr_core::config::{DiscordConfig, PostMode};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::webhook;

/// How many finished jobs (posted, skipped or failed) the list keeps.
const KEEP_FINISHED: usize = 50;

/// How often a waiting job checks whether the game has closed.
const RECHECK: Duration = Duration::from_secs(5);

/// Room left under the upload limit for the thumbnail and the message itself.
const POST_OVERHEAD: u64 = 256 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    /// Waiting for the user's yes.
    Ask,
    /// Will be posted (after the game, unless `now`).
    Queued,
    /// Being shrunk or uploaded.
    Posting,
    Posted,
    /// The user said no.
    Skipped,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: u64,
    pub clip: ClipInfo,
    /// The webhook's name in the settings.
    pub webhook: String,
    pub state: JobState,
    /// Post even while a game runs (the user shared it by hand).
    #[serde(default)]
    pub now: bool,
    /// Why it failed, for the user.
    #[serde(default)]
    pub error: Option<String>,
    /// Unix seconds.
    pub created: u64,
}

impl Job {
    fn finished(&self) -> bool {
        matches!(
            self.state,
            JobState::Posted | JobState::Skipped | JobState::Failed
        )
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    next_id: u64,
    jobs: Vec<Job>,
}

/// Finds a clip's cloud link (see [`Inner::link`]).
type LinkFn = dyn Fn(&ClipInfo) -> Result<Option<String>> + Send + Sync;

struct Inner {
    file: PathBuf,
    /// Where smaller copies are made (and removed after posting).
    work_dir: PathBuf,
    state: Mutex<Saved>,
    wake: Condvar,
    config: Box<dyn Fn() -> DiscordConfig + Send + Sync>,
    /// Whether a game runs right now.
    in_game: Box<dyn Fn() -> bool + Send + Sync>,
    /// The clip's link in the user's cloud bucket (uploading it first), when cloud storage is set up.
    link: Box<LinkFn>,
    changed: broadcast::Sender<()>,
}

/// The post queue and its worker thread. Cheap to clone.
#[derive(Clone)]
pub struct Poster {
    inner: Arc<Inner>,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Poster {
    /// Loads the queue from `file` and starts the worker. `config` returns the current Discord settings;
    /// `in_game` whether a game runs; `link` the clip's cloud link, if it has one (see [`Inner::link`]).
    pub fn start(
        file: PathBuf,
        work_dir: PathBuf,
        config: impl Fn() -> DiscordConfig + Send + Sync + 'static,
        in_game: impl Fn() -> bool + Send + Sync + 'static,
        link: impl Fn(&ClipInfo) -> Result<Option<String>> + Send + Sync + 'static,
    ) -> Self {
        // Smaller copies left by a crash or a kill mid-post: nothing refers to them any more.
        if let Ok(dir) = std::fs::read_dir(&work_dir) {
            for entry in dir.flatten() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        let mut poster = Self::load(file, work_dir, config, in_game);
        Arc::get_mut(&mut poster.inner).expect("not shared yet").link = Box::new(link);
        let worker = poster.clone();
        std::thread::Builder::new()
            .name("clipr-discord".into())
            .spawn(move || worker.run())
            .expect("can't start a thread");
        poster
    }

    fn load(
        file: PathBuf,
        work_dir: PathBuf,
        config: impl Fn() -> DiscordConfig + Send + Sync + 'static,
        in_game: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Self {
        let mut saved: Saved = std::fs::read(&file)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        // A post cut off by quitting goes again.
        for job in &mut saved.jobs {
            if job.state == JobState::Posting {
                job.state = JobState::Queued;
            }
        }
        Self {
            inner: Arc::new(Inner {
                file,
                work_dir,
                state: Mutex::new(saved),
                wake: Condvar::new(),
                config: Box::new(config),
                in_game: Box::new(in_game),
                link: Box::new(|_| Ok(None)),
                changed: broadcast::Sender::new(16),
            }),
        }
    }

    /// Fires whenever the job list changes (for the UI).
    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.inner.changed.subscribe()
    }

    /// Every job, newest first.
    pub fn jobs(&self) -> Vec<Job> {
        let mut jobs = self.lock().jobs.clone();
        jobs.reverse();
        jobs
    }

    /// A new clip was saved: queue it for every webhook that wants clips of its game. Full recordings are
    /// only shared by hand (they're long, and rarely meant for a channel).
    pub fn clip_saved(&self, clip: &ClipInfo) {
        if clip.kind == "recording" {
            return;
        }
        let config = (self.inner.config)();
        let wanted: Vec<_> = config
            .webhooks
            .iter()
            .filter(|w| w.wants(&clip.game) && !w.url.trim().is_empty())
            .map(|w| {
                let state = if w.mode == PostMode::Auto {
                    JobState::Queued
                } else {
                    JobState::Ask
                };
                (w.name.clone(), state)
            })
            .collect();
        if wanted.is_empty() {
            return;
        }
        let mut s = self.lock();
        for (webhook, state) in wanted {
            Self::push(&mut s, clip.clone(), webhook, state, false);
        }
        self.save_and_wake(s);
    }

    /// The user shares `clip` to `webhook` by hand: it's posted right away, game or not.
    pub fn share(&self, clip: ClipInfo, webhook: &str) -> Result<u64> {
        let config = (self.inner.config)();
        match config.webhooks.iter().find(|w| w.name == webhook) {
            None => bail!("There's no Discord channel called {webhook:?} in the settings"),
            Some(w) if w.url.trim().is_empty() => {
                bail!("{webhook} has no webhook URL yet; add it in Settings → Discord")
            }
            Some(_) => {}
        }
        let mut s = self.lock();
        let id = Self::push(&mut s, clip, webhook.to_string(), JobState::Queued, true);
        self.save_and_wake(s);
        Ok(id)
    }

    /// A clip was renamed (its id and path change): jobs that haven't run yet follow it.
    pub fn clip_renamed(&self, old_id: &str, clip: &ClipInfo) {
        let mut s = self.lock();
        let mut changed = false;
        for job in s.jobs.iter_mut().filter(|j| j.clip.id == old_id && !j.finished()) {
            job.clip = clip.clone();
            changed = true;
        }
        if changed {
            self.save_and_wake(s);
        }
    }

    /// Answers an `Ask` job (`true`: post it), or retries a failed one (`true`).
    pub fn answer(&self, id: u64, yes: bool) -> Result<()> {
        let mut s = self.lock();
        let job = s
            .jobs
            .iter_mut()
            .find(|j| j.id == id)
            .context("That post isn't in the list any more")?;
        match (job.state, yes) {
            (JobState::Ask | JobState::Failed, true) => {
                job.state = JobState::Queued;
                job.error = None;
                // Answering in the UI means the user is at their desk: post now.
                job.now = true;
            }
            (JobState::Ask | JobState::Queued | JobState::Failed, false) => job.state = JobState::Skipped,
            _ => bail!("That post can't be changed now"),
        }
        self.save_and_wake(s);
        Ok(())
    }

    fn push(s: &mut Saved, clip: ClipInfo, webhook: String, state: JobState, now: bool) -> u64 {
        s.next_id += 1;
        let id = s.next_id;
        s.jobs.push(Job {
            id,
            clip,
            webhook,
            state,
            now,
            error: None,
            created: now_secs(),
        });
        // Keep the newest finished jobs; never drop open ones.
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
            tracing::warn!("can't save the Discord post queue: {e:#}");
        }
        drop(s);
        self.inner.wake.notify_all();
        let _ = self.inner.changed.send(());
    }

    /// The next job to post now, marked as posting.
    fn take(&self) -> Option<Job> {
        let mut s = self.lock();
        loop {
            let in_game = (self.inner.in_game)();
            let during = (self.inner.config)().post_during_game;
            if let Some(job) = s
                .jobs
                .iter_mut()
                .find(|j| j.state == JobState::Queued && (j.now || during || !in_game))
            {
                job.state = JobState::Posting;
                let job = job.clone();
                self.save_and_wake(s);
                return Some(job);
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
        while let Some(job) = self.take() {
            // A panic in the encoder or the upload must not stop the queue for good.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.post(&job)))
                .unwrap_or_else(|_| {
                    Err(anyhow::anyhow!(
                        "clipr hit an internal error while posting; try again"
                    ))
                });
            let mut s = self.lock();
            if let Some(j) = s.jobs.iter_mut().find(|j| j.id == job.id) {
                match result {
                    Ok(()) => {
                        tracing::info!("posted {} to Discord ({})", job.clip.title, job.webhook);
                        j.state = JobState::Posted;
                        j.error = None;
                    }
                    Err(e) => {
                        tracing::warn!("couldn't post {} to Discord: {e:#}", job.clip.title);
                        j.state = JobState::Failed;
                        j.error = Some(format!("{e:#}"));
                    }
                }
            }
            self.save_and_wake(s);
        }
    }

    fn post(&self, job: &Job) -> Result<()> {
        let config = (self.inner.config)();
        let hook = config
            .webhooks
            .iter()
            .find(|w| w.name == job.webhook)
            .context("This Discord channel was removed from the settings")?;
        let clip = &job.clip;
        if !clip.path.is_file() {
            bail!("The clip isn't there any more");
        }
        fn post<'a>(clip: &'a ClipInfo, video: &'a Path) -> webhook::Post<'a> {
            webhook::Post {
                title: &clip.title,
                game: &clip.game,
                duration_secs: clip.duration_secs,
                video,
                thumb: clip.thumb.as_deref(),
            }
        }
        // With cloud storage, Discord gets a link to the full clip instead of a (smaller) file.
        match (self.inner.link)(clip) {
            Ok(Some(link)) => return webhook::post_link(&hook.url, &post(clip, &clip.path), &link),
            Ok(None) => {}
            Err(e) => tracing::warn!("posting {} as a file: {e:#}", clip.title),
        }
        let limit = (u64::from(config.upload_limit_mb.max(1)) * 1_000_000).saturating_sub(POST_OVERHEAD);
        let size = std::fs::metadata(&clip.path).map_or(clip.size_bytes, |m| m.len());
        let small = self.inner.work_dir.join(format!("discord-{}.mp4", job.id));
        let video = if size <= limit {
            clip.path.clone()
        } else {
            let duration = clip
                .duration_secs
                .context("The clip's length is unknown, so it can't be made smaller")?;
            tracing::info!("{} is over the upload limit; making a smaller copy", clip.title);
            clipr_mux::shrink(&clip.path, &small, limit, duration)?;
            let got = std::fs::metadata(&small).map_or(u64::MAX, |m| m.len());
            if got > limit {
                let _ = std::fs::remove_file(&small);
                bail!("The smaller copy still doesn't fit the upload limit; trim the clip first");
            }
            small.clone()
        };
        let result = webhook::post(&hook.url, &post(clip, &video));
        if video == small {
            let _ = std::fs::remove_file(&small);
        }
        result
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
    use clipr_core::config::Webhook;

    fn hook(name: &str, mode: PostMode, games: &[&str]) -> Webhook {
        Webhook {
            name: name.into(),
            url: "https://discord.com/api/webhooks/1/abcdefghijklmnopqrstuvwxyz".into(),
            mode,
            games: games.iter().map(|g| g.to_string()).collect(),
        }
    }

    fn clip(game: &str, kind: &str) -> ClipInfo {
        ClipInfo {
            id: "VALORANT/a.mp4".into(),
            path: PathBuf::from("a.mp4"),
            title: "a".into(),
            game: game.into(),
            duration_secs: Some(30.0),
            size_bytes: 1000,
            thumb: None,
            kind: kind.into(),
        }
    }

    fn poster(dir: &Path, hooks: Vec<Webhook>) -> Poster {
        let config = DiscordConfig {
            webhooks: hooks,
            ..Default::default()
        };
        // Not started: the tests drive the queue without a worker posting anything.
        Poster::load(
            dir.join("q.json"),
            dir.to_path_buf(),
            move || config.clone(),
            || true,
        )
    }

    #[test]
    fn new_clips_follow_each_webhooks_mode_and_games() {
        let dir = tempfile::tempdir().unwrap();
        let p = poster(
            dir.path(),
            vec![
                hook("ask", PostMode::Ask, &[]),
                hook("auto-val", PostMode::Auto, &["valorant"]),
                hook("off", PostMode::Off, &[]),
            ],
        );
        p.clip_saved(&clip("VALORANT", "clip"));
        let jobs = p.jobs();
        assert_eq!(jobs.len(), 2);
        assert_eq!((jobs[1].webhook.as_str(), jobs[1].state), ("ask", JobState::Ask));
        assert_eq!(
            (jobs[0].webhook.as_str(), jobs[0].state),
            ("auto-val", JobState::Queued)
        );
        p.clip_saved(&clip("Minecraft", "clip"));
        assert_eq!(p.jobs().len(), 3, "only the Ask webhook takes every game");
        p.clip_saved(&clip("VALORANT", "recording"));
        assert_eq!(p.jobs().len(), 3, "recordings are only shared by hand");
    }

    #[test]
    fn answers_and_shares() {
        let dir = tempfile::tempdir().unwrap();
        let p = poster(dir.path(), vec![hook("ask", PostMode::Ask, &[])]);
        p.clip_saved(&clip("VALORANT", "clip"));
        let id = p.jobs()[0].id;
        p.answer(id, false).unwrap();
        assert_eq!(p.jobs()[0].state, JobState::Skipped);
        assert!(p.answer(id, true).is_err(), "a skipped post stays skipped");

        let id = p.share(clip("VALORANT", "recording"), "ask").unwrap();
        let job = p.jobs().into_iter().find(|j| j.id == id).unwrap();
        assert!(job.now && job.state == JobState::Queued);
        assert!(p.share(clip("VALORANT", "clip"), "nope").is_err());
    }

    #[test]
    fn waiting_posts_follow_a_renamed_clip_and_blank_urls_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut blank = hook("blank", PostMode::Ask, &[]);
        blank.url.clear();
        let p = poster(dir.path(), vec![hook("ask", PostMode::Ask, &[]), blank]);
        assert!(
            p.share(clip("VALORANT", "clip"), "blank")
                .unwrap_err()
                .to_string()
                .contains("no webhook URL")
        );
        p.clip_saved(&clip("VALORANT", "clip"));
        let mut done = clip("VALORANT", "clip");
        done.title = "done".into();
        let done_id = p.share(done, "ask").unwrap();
        p.answer(done_id, false).unwrap();

        let mut renamed = clip("VALORANT", "clip");
        renamed.id = "VALORANT/ace.mp4".into();
        renamed.title = "ace".into();
        p.clip_renamed("VALORANT/a.mp4", &renamed);
        let jobs = p.jobs();
        let waiting = jobs.iter().find(|j| j.state == JobState::Ask).unwrap();
        assert_eq!(
            (waiting.clip.id.as_str(), waiting.clip.title.as_str()),
            ("VALORANT/ace.mp4", "ace")
        );
        let finished = jobs.iter().find(|j| j.id == done_id).unwrap();
        assert_eq!(finished.clip.title, "done", "finished posts keep what was posted");
    }

    #[test]
    fn take_waits_for_the_game_unless_shared_by_hand() {
        let dir = tempfile::tempdir().unwrap();
        let p = poster(dir.path(), vec![hook("auto", PostMode::Auto, &[])]);
        p.clip_saved(&clip("VALORANT", "clip"));
        // In game (the test poster always is), nothing is taken: check without blocking.
        let s = p.lock();
        let ready = s.jobs.iter().any(|j| j.state == JobState::Queued && j.now);
        drop(s);
        assert!(!ready);
        let id = p.share(clip("VALORANT", "clip"), "auto").unwrap();
        let job = p.take().unwrap();
        assert_eq!(job.id, id, "the hand-shared one goes first, in game");
        assert_eq!(
            p.jobs().iter().find(|j| j.id == id).unwrap().state,
            JobState::Posting
        );
    }

    #[test]
    fn the_queue_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let p = poster(dir.path(), vec![hook("auto", PostMode::Auto, &[])]);
        p.clip_saved(&clip("VALORANT", "clip"));
        p.share(clip("VALORANT", "clip"), "auto").unwrap();
        p.take().unwrap(); // left "posting" by a quit
        let again = poster(dir.path(), vec![]);
        let jobs = again.jobs();
        assert_eq!(jobs.len(), 2);
        assert!(jobs.iter().all(|j| j.state == JobState::Queued), "{jobs:?}");
        let text = std::fs::read_to_string(dir.path().join("q.json")).unwrap();
        assert!(
            !text.contains("api/webhooks"),
            "the webhook URL isn't copied into the queue"
        );
    }

    #[test]
    fn old_finished_jobs_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let p = poster(dir.path(), vec![hook("ask", PostMode::Ask, &[])]);
        for _ in 0..(KEEP_FINISHED + 10) {
            p.clip_saved(&clip("VALORANT", "clip"));
            let id = p.jobs()[0].id;
            p.answer(id, false).unwrap();
        }
        p.clip_saved(&clip("VALORANT", "clip"));
        let jobs = p.jobs();
        assert_eq!(jobs.len(), KEEP_FINISHED + 1);
        assert_eq!(jobs[0].state, JobState::Ask, "the open one is kept");
    }
}
