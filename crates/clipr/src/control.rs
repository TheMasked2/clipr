//! The web API's view of the app: [`clipr_web::Control`] on top of the engine, the settings file and the
//! main thread (hotkeys live there).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, mpsc};

use anyhow::{Context, Result};
use clipr_core::config::{Config, HotkeyConfig};
use clipr_library::Library;
use clipr_web::Control;
use serde_json::json;

use crate::engine::{Command, Engine, preset::Preset};
use crate::win;

pub struct AppControl {
    pub engine: Engine,
    pub config: Arc<Mutex<Config>>,
    pub config_file: PathBuf,
    pub library: Arc<Library>,
    /// New hotkeys go to the main thread, which owns the registrations.
    pub hotkeys: mpsc::Sender<HotkeyConfig>,
    pub main_thread: u32,
    pub updater: crate::updater::Updater,
    pub crash_dir: PathBuf,
    pub log_dir: PathBuf,
    pub known_games: Arc<dyn Fn() -> usize + Send + Sync>,
}

/// The auto preset, measured once in the background at startup.
static PRESET: OnceLock<Option<Preset>> = OnceLock::new();

pub fn measure_preset_in_background() {
    std::thread::Builder::new()
        .name("clipr-preset".into())
        .spawn(|| {
            let preset = crate::engine::preset::detect()
                .inspect_err(|e| tracing::warn!("auto preset failed: {e:#}"))
                .ok();
            if let Some(p) = &preset {
                tracing::info!("auto preset: {p:?}");
            }
            let _ = PRESET.set(preset);
        })
        .expect("can't start a thread");
}

impl Control for AppControl {
    fn save_clip(&self, seconds: Option<u32>) {
        self.engine.send(Command::SaveClip(seconds));
    }

    fn set_buffering(&self, on: bool) {
        self.engine.send(Command::SetBuffering(on));
    }

    fn set_recording(&self, on: bool) {
        self.engine.send(Command::SetRecording(on));
    }

    fn set_mic_muted(&self, muted: bool) {
        self.engine.send(Command::SetMicMuted(muted));
    }

    fn test_sound(&self, volume: u32) {
        crate::sound::play(crate::sound::Cue::Saved, volume);
    }

    fn preview(&self, fps: u32) -> Result<Option<std::sync::Arc<Vec<u8>>>, String> {
        crate::preview::jpeg(fps)
    }

    fn mark(&self) {
        self.engine.send(Command::Mark);
    }

    fn config(&self) -> Config {
        self.config.lock().unwrap().clone()
    }

    fn set_config(&self, config: Config) -> Result<()> {
        // The step that can fail goes first, so a failure changes nothing.
        let before = self.config();
        if before.general.start_with_windows != config.general.start_with_windows {
            crate::autostart::set(config.general.start_with_windows)
                .context("couldn't change starting with Windows")?;
        }
        if let Err(e) = config.save(&self.config_file) {
            if before.general.start_with_windows != config.general.start_with_windows {
                let _ = crate::autostart::set(before.general.start_with_windows);
            }
            return Err(e);
        }
        let old = std::mem::replace(&mut *self.config.lock().unwrap(), config.clone());
        if old.hotkeys != config.hotkeys {
            let _ = self.hotkeys.send(config.hotkeys.clone());
            win::wake(self.main_thread);
        }
        crate::crash::set_enabled(config.privacy.crash_reports);
        if old.clips.dir() != config.clips.dir() {
            self.library.set_clips_dir(config.clips.dir());
        }
        self.library
            .set_recordings_dir(config.recording.own_dir(&config.clips));
        self.engine.send(Command::Apply(Box::new(config)));
        Ok(())
    }

    fn trim(&self, src: &Path, dst: &Path, start_secs: f64, end_secs: f64) -> Result<()> {
        clipr_mux::trim(src, dst, start_secs, end_secs)
    }

    fn reveal(&self, path: &Path) {
        win::reveal(path);
    }

    fn levels(&self) -> Vec<clipr_core::status::Level> {
        self.engine.levels()
    }

    fn watch_levels(&self) {
        self.engine.watch_levels();
    }

    fn pick_folder(&self, title: &str, start: &str) -> std::result::Result<Option<String>, String> {
        let start = match start {
            "" => self.config().clips.dir(),
            s => s.into(),
        };
        Ok(crate::picker::pick_folder(title, &start)?.map(|p| p.to_string_lossy().into_owned()))
    }

    fn perf(&self) -> serde_json::Value {
        serde_json::to_value(self.engine.perf()).unwrap_or_default()
    }

    fn crashes(&self) -> serde_json::Value {
        if !self.config().privacy.crash_reports {
            return json!([]);
        }
        let records = crate::crash::list(&self.crash_dir);
        if records.is_empty() {
            return json!([]);
        }
        static MACHINE: OnceLock<crate::crash::Machine> = OnceLock::new();
        let machine = MACHINE.get_or_init(crate::crash::Machine::detect);
        let scrub = crate::crash::Scrub::from_env();
        records
            .iter()
            .map(|(id, r)| {
                let has_dump = crate::crash::dump_path(&self.crash_dir, id).is_some();
                crate::crash::to_json(id, r, machine, &self.log_dir, &scrub, has_dump)
            })
            .collect()
    }

    fn dismiss_crash(&self, id: &str) -> std::result::Result<(), String> {
        crate::crash::dismiss(&self.crash_dir, id)
    }

    fn reveal_crash(&self, id: &str) -> std::result::Result<(), String> {
        let path = crate::crash::dump_path(&self.crash_dir, id)
            .ok_or_else(|| "This crash has no dump file.".to_string())?;
        win::reveal(&path);
        Ok(())
    }

    fn update_status(&self) -> serde_json::Value {
        self.updater.status_json()
    }

    fn check_update(&self) {
        self.updater.check();
    }

    fn apply_update(&self) -> std::result::Result<(), String> {
        self.updater.apply()
    }

    fn system_info(&self) -> serde_json::Value {
        let gpus: Vec<_> = clipr_encode::probe::list_adapters()
            .unwrap_or_default()
            .into_iter()
            .filter(|a| !a.software && a.vendor_id != 0x1414)
            .map(|a| json!({ "name": a.name, "vendor": format!("{:?}", a.vendor) }))
            .collect();
        let mut encoders: Vec<String> = clipr_encode::probe::list_encoders()
            .unwrap_or_default()
            .into_iter()
            .filter(|e| e.hardware)
            .map(|e| match e.codec {
                clipr_encode::Codec::H264 => "H.264".to_string(),
                clipr_encode::Codec::Hevc => "HEVC".to_string(),
                clipr_encode::Codec::Av1 => "AV1".to_string(),
            })
            .collect();
        encoders.dedup();
        let monitors: Vec<_> = clipr_capture::list_monitors()
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(i, m)| {
                json!({
                    "index": i + 1,
                    "name": m.name,
                    "width": m.rect.right - m.rect.left,
                    "height": m.rect.bottom - m.rect.top,
                    "primary": m.primary,
                })
            })
            .collect();
        // Each GPU lists the codecs found; with one encoder set per machine that's close enough.
        let gpus: Vec<_> = gpus
            .into_iter()
            .map(|mut g| {
                g["encoders"] = json!(encoders);
                g
            })
            .collect();
        json!({
            "gpus": gpus,
            "monitors": monitors,
            "known_games": (self.known_games)(),
            "elevated": crate::win::is_elevated(),
            "preset": PRESET.get().cloned().flatten(),
        })
    }
}
