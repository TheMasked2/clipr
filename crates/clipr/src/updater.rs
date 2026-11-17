//! The updater thread. It checks GitHub releases (by itself only when `general.auto_update` is on, and then
//! once per start, like Steam: shortly after clipr starts, never again until the next start; "Check now" in
//! the UI works either way), downloads the
//! installer with a verified checksum, and applies it only when no game runs and nothing is recording. To
//! apply, it stores the installer in [`take_pending`] and quits the main loop; `main` shuts everything down
//! as usual and then starts the installer, which waits for clipr to exit, installs and starts it again.
//!
//! Only an installed copy updates itself. A portable copy gets "Download" (the release page) instead.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clipr_core::config::GeneralConfig;
use clipr_core::status::{RecState, Status};
use clipr_update::Release;
use serde_json::{Value, json};

use crate::win;

/// When the startup check runs, counted from start. Right after a boot the network may not be up yet, so a
/// failed check is tried again at the next time; after the last one, nothing until the next start.
const STARTUP_CHECKS: [Duration; 3] = [
    Duration::from_secs(15),
    Duration::from_secs(60),
    Duration::from_secs(5 * 60),
];
/// How often an update that waits for the game to close looks again.
const BUSY_RECHECK: Duration = Duration::from_secs(30);
/// How often the thread looks at the settings when nothing else wakes it (auto-update may get switched on).
const IDLE_WAKE: Duration = Duration::from_secs(10 * 60);
/// "Check now" more often than this asks GitHub only once (it allows 60 requests an hour).
const MIN_CHECK_GAP: Duration = Duration::from_secs(10);

/// The verified installer to run after shutdown.
static PENDING: Mutex<Option<PathBuf>> = Mutex::new(None);

/// The installer the updater asked for, if it quit clipr to update. `main` runs it after shutdown.
pub fn take_pending() -> Option<PathBuf> {
    PENDING.lock().unwrap().take()
}

/// Starts the installer (after clipr has shut down; it waits for clipr's process to end).
pub fn run_installer(setup: &Path, log_dir: &Path) {
    let log = log_dir.join("update-setup.log");
    let result = std::process::Command::new(setup)
        .args(clipr_update::INSTALLER_ARGS)
        .arg(format!("/LOG={}", log.display()))
        .spawn();
    match result {
        Ok(_) => tracing::info!("started the installer {}", setup.display()),
        Err(e) => tracing::error!("can't start the installer {}: {e}", setup.display()),
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Phase {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available,
    Downloading,
    /// Downloaded and verified.
    Ready,
    /// An install is wanted, waiting for the game or recording to end (before downloading, too).
    Waiting,
    Installing,
    Error,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Checking => "checking",
            Self::UpToDate => "up_to_date",
            Self::Available => "available",
            Self::Downloading => "downloading",
            Self::Ready => "ready",
            Self::Waiting => "waiting",
            Self::Installing => "installing",
            Self::Error => "error",
        }
    }
}

#[derive(Default)]
struct Shared {
    phase: Phase,
    release: Option<Release>,
    /// The verified installer for `release`.
    setup: Option<PathBuf>,
    /// Unix seconds of the last finished check.
    checked_at: Option<u64>,
    last_check: Option<Instant>,
    error: Option<String>,
    done: u64,
    total: u64,
    /// Install as soon as nothing is running (set by "Update now", or by auto-update).
    want_apply: bool,
    /// `want_apply` came from auto-update, so switching auto-update off cancels it.
    auto_apply: bool,
}

enum Cmd {
    Check,
    Apply,
}

#[derive(Clone)]
pub struct Updater {
    shared: Arc<Mutex<Shared>>,
    tx: mpsc::Sender<Cmd>,
    status: Arc<Status>,
    installed: bool,
}

/// Why an update can't be installed right now, if something is running.
fn busy(status: &Status) -> Option<&'static str> {
    let a = status.activity();
    if a.state == RecState::Recording || a.recording_since.is_some() {
        Some("a recording is running")
    } else if a.game.is_some() {
        Some("a game is running")
    } else {
        None
    }
}

/// The version the updater compares releases with. `CLIPR_UPDATE_PRETEND_VERSION` overrides it, so testers
/// can update an installed copy to the release it already is (a reinstall); anything it finds is still
/// checksum-verified.
fn current_version() -> String {
    std::env::var("CLIPR_UPDATE_PRETEND_VERSION")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| clipr_core::VERSION.to_string())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Updater {
    /// Starts the updater thread. `settings` reads the current general settings; downloads go to `dir`.
    pub fn start(
        status: Arc<Status>,
        settings: impl Fn() -> GeneralConfig + Send + 'static,
        dir: PathBuf,
        main_thread: u32,
    ) -> Self {
        let installed = std::env::current_exe().is_ok_and(|exe| clipr_update::is_installed(&exe));
        let (tx, rx) = mpsc::channel();
        let updater = Self {
            shared: Arc::default(),
            tx,
            status,
            installed,
        };
        let worker = Worker {
            updater: updater.clone(),
            settings: Box::new(settings),
            dir,
            main_thread,
        };
        std::thread::Builder::new()
            .name("clipr-updater".into())
            .spawn(move || worker.run(&rx))
            .expect("can't start a thread");
        updater
    }

    /// What the UI shows: `GET /api/update`.
    pub fn status_json(&self) -> Value {
        let s = self.shared.lock().unwrap();
        json!({
            "current": current_version(),
            "installed": self.installed,
            "state": s.phase.name(),
            "release": s.release,
            "checked_at": s.checked_at,
            "error": s.error,
            "progress": (s.phase == Phase::Downloading).then(|| json!({ "done": s.done, "total": s.total })),
            "busy": busy(&self.status),
            "will_install": s.want_apply,
        })
    }

    /// Asks GitHub now (the answer shows up in [`Self::status_json`]).
    pub fn check(&self) {
        let _ = self.tx.send(Cmd::Check);
    }

    /// Downloads (if needed) and installs the update found by the last check. Refuses while a game runs or
    /// something records, on a portable copy, and when there's nothing to install.
    pub fn apply(&self) -> Result<(), String> {
        if !self.installed {
            return Err(
                "This copy of clipr isn't installed (it's the portable zip), so it can't update itself. \
                 Download the new version from GitHub instead."
                    .into(),
            );
        }
        {
            let s = self.shared.lock().unwrap();
            let Some(release) = &s.release else {
                return Err("There's no update to install. Check for updates first.".into());
            };
            if release.setup.is_none() {
                return Err("This release has no installer. Download it from GitHub instead.".into());
            }
            if s.phase == Phase::Installing {
                return Ok(());
            }
        }
        if let Some(why) = busy(&self.status) {
            return Err(format!("Can't update while {why}. Try again when it's over."));
        }
        let _ = self.tx.send(Cmd::Apply);
        Ok(())
    }

    fn update(&self, f: impl FnOnce(&mut Shared)) {
        f(&mut self.shared.lock().unwrap());
    }
}

struct Worker {
    updater: Updater,
    settings: Box<dyn Fn() -> GeneralConfig + Send>,
    dir: PathBuf,
    main_thread: u32,
}

impl Worker {
    fn run(self, rx: &mpsc::Receiver<Cmd>) {
        if current_version() != clipr_core::VERSION {
            tracing::warn!(
                "updater pretends to be clipr {} (CLIPR_UPDATE_PRETEND_VERSION)",
                current_version()
            );
        }
        // The installer that just updated this copy may still be finishing: retry until it's gone.
        let mut cleaned = self.clean_downloads();
        // Only with auto-update on at start: switching it on later waits for the next start (or "Check now").
        let started = Instant::now();
        let mut startup_checks = if (self.settings)().auto_update {
            &STARTUP_CHECKS[..]
        } else {
            &[][..]
        };
        loop {
            let auto = (self.settings)().auto_update;
            let want_apply = self.updater.shared.lock().unwrap().want_apply;
            let next_check = startup_checks.first().map(|&at| started + at);
            let wait = if want_apply {
                BUSY_RECHECK
            } else if let Some(at) = next_check {
                at.saturating_duration_since(Instant::now()).min(IDLE_WAKE)
            } else {
                IDLE_WAKE
            };
            match rx.recv_timeout(wait) {
                Ok(Cmd::Check) => {
                    if self.check(auto) {
                        startup_checks = &[]; // answered: the startup check isn't needed any more
                    }
                }
                Ok(Cmd::Apply) => self.updater.update(|s| {
                    s.want_apply = true;
                    s.auto_apply = false;
                }),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if next_check.is_some_and(|at| Instant::now() >= at) {
                        let answered = auto && self.check(true);
                        // Answered, switched off meanwhile, or the last try: no more checks this run.
                        startup_checks = if answered || !auto {
                            &[]
                        } else {
                            &startup_checks[1..]
                        };
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
            if !cleaned {
                let holding = {
                    let s = self.updater.shared.lock().unwrap();
                    s.setup.is_some() || s.want_apply
                };
                if !holding {
                    cleaned = self.clean_downloads();
                }
            }
            let auto_now = (self.settings)().auto_update;
            let apply = {
                let mut s = self.updater.shared.lock().unwrap();
                if s.want_apply && s.auto_apply && !auto_now {
                    s.want_apply = false;
                    if s.phase == Phase::Waiting {
                        let ready = s.setup.as_ref().is_some_and(|p| p.is_file());
                        s.phase = if ready { Phase::Ready } else { Phase::Available };
                    }
                }
                s.want_apply
            };
            if apply && self.try_apply() {
                return;
            }
        }
    }

    /// Removes installers left from earlier updates. True when nothing is left.
    fn clean_downloads(&self) -> bool {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return true; // no folder, nothing to clean
        };
        let mut all = true;
        for entry in entries.flatten() {
            all &= std::fs::remove_file(entry.path()).is_ok();
        }
        all
    }

    /// Asks GitHub. With `auto` on, a found update installs by itself when nothing runs. False when GitHub
    /// couldn't be reached (true when the check was skipped because one just ran or a download is going).
    fn check(&self, auto: bool) -> bool {
        {
            let mut s = self.updater.shared.lock().unwrap();
            if s.last_check.is_some_and(|t| t.elapsed() < MIN_CHECK_GAP)
                || matches!(s.phase, Phase::Downloading | Phase::Installing)
            {
                return true;
            }
            s.phase = Phase::Checking;
            s.error = None;
        }
        let beta = (self.settings)().update_beta;
        let result = clipr_update::check(&current_version(), beta);
        let mut s = self.updater.shared.lock().unwrap();
        s.last_check = Some(Instant::now());
        s.checked_at = Some(unix_now());
        match result {
            Ok(Some(release)) => {
                if s.release.as_ref().is_some_and(|r| r.version != release.version) {
                    s.setup = None; // an older download is for another version
                }
                tracing::info!("update available: clipr {}", release.version);
                let ready = s.setup.as_ref().is_some_and(|p| p.is_file());
                s.phase = if ready { Phase::Ready } else { Phase::Available };
                if auto && self.updater.installed && release.setup.is_some() && !s.want_apply {
                    s.want_apply = true;
                    s.auto_apply = true;
                }
                s.release = Some(release);
            }
            Ok(None) => {
                s.phase = Phase::UpToDate;
                s.release = None;
                s.setup = None;
                s.want_apply = false;
            }
            Err(e) => {
                tracing::warn!("update check failed: {e:#}");
                s.phase = Phase::Error;
                s.error = Some(format!("Couldn't check for updates: {e:#}"));
                return false;
            }
        }
        true
    }

    /// Downloads the installer if needed, then quits clipr to install it when nothing runs. Returns true
    /// when clipr is quitting.
    fn try_apply(&self) -> bool {
        let (release, setup) = {
            let s = self.updater.shared.lock().unwrap();
            (s.release.clone(), s.setup.clone())
        };
        let Some(asset) = release.and_then(|r| r.setup) else {
            self.updater.update(|s| s.want_apply = false);
            return false;
        };
        // Not even the download while a game runs: it would compete with the game for the network.
        if busy(&self.updater.status).is_some() {
            self.updater.update(|s| s.phase = Phase::Waiting);
            return false;
        }
        let path = match setup.filter(|p| p.is_file()) {
            Some(path) => path,
            None => {
                self.updater.update(|s| {
                    s.phase = Phase::Downloading;
                    s.error = None;
                    (s.done, s.total) = (0, asset.size);
                });
                let shared = &self.updater.shared;
                let result = clipr_update::download(&asset, &self.dir, |done, total| {
                    let mut s = shared.lock().unwrap();
                    (s.done, s.total) = (done, total);
                });
                match result {
                    Ok(path) => {
                        tracing::info!("downloaded and verified {}", path.display());
                        self.updater.update(|s| s.setup = Some(path.clone()));
                        path
                    }
                    Err(e) => {
                        tracing::warn!("update download failed: {e:#}");
                        self.updater.update(|s| {
                            s.phase = Phase::Error;
                            s.error = Some(format!("Couldn't download the update: {e:#}"));
                            s.want_apply = false;
                        });
                        return false;
                    }
                }
            }
        };
        if busy(&self.updater.status).is_some() {
            self.updater.update(|s| s.phase = Phase::Waiting);
            return false;
        }
        tracing::info!("quitting to install the update");
        *PENDING.lock().unwrap() = Some(path);
        self.updater.update(|s| {
            s.phase = Phase::Installing;
            s.want_apply = false;
        });
        win::quit_thread(self.main_thread);
        true
    }
}
