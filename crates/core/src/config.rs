//! The settings file, `config.toml`.
//!
//! Every field has a default, so a missing file, a missing section or an unknown old key never stops clipr
//! from starting. The web UI edits the same struct; hand edits keep working too.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The web UI's preferred port. If it's taken, clipr uses the next free one (see `clipr-web`).
pub const DEFAULT_PORT: u16 = 7845;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub web: WebConfig,
    pub ui: UiConfig,
    pub general: GeneralConfig,
    pub privacy: PrivacyConfig,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WebConfig {
    /// First port to try for the web UI.
    pub port: u16,
    /// Open the web UI to the local network (phone at home). Off by default; needs `lan_pin` once built.
    pub lan: bool,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            lan: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    /// `dark-sleek` (default), later also `minimal`, `playful`, `system`.
    pub theme: String,
    /// Accent colour as `#rrggbb`.
    pub accent: String,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "dark-sleek".into(),
            accent: "#8B5CF6".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    /// Start clipr when Windows starts.
    pub start_with_windows: bool,
    /// Open the web UI when you start clipr yourself, so you can see it's running. Never happens when
    /// Windows starts it (`--background`).
    pub open_ui_on_start: bool,
    /// Check GitHub releases and update by itself. Opt-in.
    pub auto_update: bool,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            start_with_windows: false,
            open_ui_on_start: true,
            auto_update: false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrivacyConfig {
    /// Send anonymous crash reports. Opt-in.
    pub crash_reports: bool,
}

impl Config {
    /// Reads the file, or returns the defaults if it doesn't exist yet.
    pub fn load(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).with_context(|| format!("can't read {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("can't open {}", path.display())),
        }
    }

    /// Like [`Config::load`], but a broken file is kept aside as `config.toml.broken` and the defaults are
    /// used, so a typo never stops clipr from starting.
    pub fn load_or_recover(path: &Path) -> Self {
        match Self::load(path) {
            Ok(config) => config,
            Err(err) => {
                tracing::warn!("{err:#}; starting with default settings");
                let aside = path.with_extension("toml.broken");
                if let Err(e) = fs::rename(path, &aside) {
                    tracing::warn!("couldn't keep the broken settings file aside: {e}");
                }
                Self::default()
            }
        }
    }

    /// Writes the file atomically: a temporary file first, then a rename, so a crash mid-save never leaves a
    /// half-written config.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).with_context(|| format!("can't create {}", dir.display()))?;
        }
        let text = toml::to_string_pretty(self).context("can't serialise settings")?;
        let tmp = path.with_extension("toml.tmp");
        fs::write(&tmp, text).with_context(|| format!("can't write {}", tmp.display()))?;
        fs::rename(&tmp, path).with_context(|| format!("can't replace {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(&dir.path().join("config.toml")).unwrap();
        assert_eq!(config, Config::default());
        assert_eq!(config.web.port, DEFAULT_PORT);
        assert!(!config.general.auto_update, "auto-update must be opt-in");
        assert!(!config.privacy.crash_reports, "crash reports must be opt-in");
        assert!(!config.web.lan, "LAN access must be opt-in");
        assert!(
            config.general.open_ui_on_start,
            "a manual start should show where clipr went"
        );
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("config.toml");
        let mut config = Config::default();
        config.web.port = 9000;
        config.ui.accent = "#22D3EE".into();
        config.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), config);
        assert!(!path.with_extension("toml.tmp").exists());
    }

    #[test]
    fn partial_file_fills_in_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "[web]\nport = 8000\n\n[unknown_old_section]\nx = 1\n").unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.web.port, 8000);
        assert!(!config.web.lan);
        assert_eq!(config.ui, UiConfig::default());
    }

    #[test]
    fn broken_file_is_kept_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "this is = = not toml").unwrap();
        assert!(Config::load(&path).is_err());
        assert_eq!(Config::load_or_recover(&path), Config::default());
        assert!(path.with_extension("toml.broken").exists());
        assert!(!path.exists());
    }
}
