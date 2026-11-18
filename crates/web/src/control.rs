//! What the web API can ask the rest of clipr to do. `clipr.exe` implements [`Control`] on top of the capture
//! engine; tests use [`NoopControl`].

use std::path::Path;

use anyhow::Result;
use clipr_core::config::Config;

pub trait Control: Send + Sync + 'static {
    /// Saves the last `seconds` of the replay buffer (`None` = `replay.clip_secs`). Returns at once; the
    /// result arrives on the event bus as `Event::ClipSaved` or `Event::ClipFailed`.
    fn save_clip(&self, seconds: Option<u32>);

    /// Pauses (`false`) or resumes (`true`) the replay buffer.
    fn set_buffering(&self, on: bool);

    /// Starts (`true`) or stops (`false`) a full recording. The finished file arrives as `Event::ClipSaved`.
    fn set_recording(&self, on: bool);

    /// Adds a marker at this moment (to the running recording and to the next clip saved).
    fn mark(&self);

    /// Mutes (`true`) or unmutes the microphone in clips. The state shows in the status (`mic_muted`).
    fn set_mic_muted(&self, _muted: bool) {}

    /// Plays the save sound once at `volume` %, so the user can hear the setting before saving it.
    fn test_sound(&self, _volume: u32) {}

    /// The newest live preview of what is captured, as a JPEG (`None` while nothing is captured; `Err` with
    /// the reason when the GPU can't make one). Asking keeps the preview coming for a few seconds. It may take a
    /// few milliseconds (it encodes the image). `fps` is how many pictures a second the page wants (1–30).
    fn preview(&self, _fps: u32) -> Result<Option<std::sync::Arc<Vec<u8>>>, String> {
        Ok(None)
    }

    /// The current settings.
    fn config(&self) -> Config;

    /// Checks, saves (`config.toml`) and applies new settings, restarting capture when needed. The error
    /// message is shown to the user.
    fn set_config(&self, config: Config) -> Result<()>;

    /// Copies `start_secs..end_secs` of the MP4 `src` into a new MP4 `dst` without re-encoding. The start
    /// snaps back to the keyframe before it. Blocking: call it from `spawn_blocking`.
    fn trim(&self, src: &Path, dst: &Path, start_secs: f64, end_secs: f64) -> Result<()>;

    /// Opens Explorer with `path` selected (a file), or showing it (a folder).
    fn reveal(&self, path: &Path);

    /// Hardware facts for the settings page: GPUs, encoders, monitors. Free-form JSON.
    fn system_info(&self) -> serde_json::Value;

    /// The current level of each audio source in dBFS (-100 = silence, 0 = full scale), before and after
    /// its filters. Empty when audio isn't running.
    fn levels(&self) -> Vec<clipr_core::status::Level>;

    /// Crash reports from earlier runs, newest first, for `GET /api/crashes` (see `docs/api.md`). Empty
    /// unless `privacy.crash_reports` is on.
    fn crashes(&self) -> serde_json::Value {
        serde_json::json!([])
    }

    /// Deletes a crash report and its dump.
    fn dismiss_crash(&self, _id: &str) -> std::result::Result<(), String> {
        Err("No such crash report.".into())
    }

    /// Shows a crash's dump file in Explorer.
    fn reveal_crash(&self, _id: &str) -> std::result::Result<(), String> {
        Err("No such crash report.".into())
    }

    /// The updater's state for `GET /api/update`. Free-form JSON (see `docs/api.md`).
    fn update_status(&self) -> serde_json::Value {
        serde_json::json!({ "current": clipr_core::VERSION, "installed": false, "state": "idle" })
    }

    /// Asks GitHub for a newer release; the answer shows up in [`Control::update_status`].
    fn check_update(&self) {}

    /// Downloads and installs the update the last check found, which quits and restarts clipr. The error
    /// (shown to the user) says why it can't right now: a game or recording runs, a portable copy, nothing to
    /// install.
    fn apply_update(&self) -> std::result::Result<(), String> {
        Err("Updates aren't available here.".into())
    }
}

/// Does nothing; for tests and for running the web server on its own.
#[derive(Default)]
pub struct NoopControl {
    pub config: std::sync::Mutex<Config>,
}

impl Control for NoopControl {
    fn save_clip(&self, _seconds: Option<u32>) {}
    fn set_buffering(&self, _on: bool) {}
    fn set_recording(&self, _on: bool) {}
    fn mark(&self) {}
    fn config(&self) -> Config {
        self.config.lock().unwrap().clone()
    }
    fn set_config(&self, config: Config) -> Result<()> {
        *self.config.lock().unwrap() = config;
        Ok(())
    }
    fn trim(&self, _src: &Path, _dst: &Path, _start: f64, _end: f64) -> Result<()> {
        anyhow::bail!("trimming isn't available here")
    }
    fn reveal(&self, _path: &Path) {}
    fn system_info(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    fn levels(&self) -> Vec<clipr_core::status::Level> {
        Vec::new()
    }
}
