//! Where clipr keeps its files.
//!
//! - Settings: `%APPDATA%\clipr\config.toml` (roams with the user profile).
//! - Logs and runtime files: `%LOCALAPPDATA%\clipr\` (stay on this PC).
//!
//! Setting the `CLIPR_HOME` environment variable puts everything under that one folder instead. This is used
//! for tests and, later, for the portable build.

use std::path::PathBuf;

use anyhow::{Context, Result};

/// The folders clipr uses. Build it once with [`Paths::detect`] and pass it around.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    /// Holds `config.toml`.
    pub config_dir: PathBuf,
    /// Holds `logs\` and runtime files such as the port file.
    pub data_dir: PathBuf,
}

impl Paths {
    /// Uses `CLIPR_HOME` when set, else the standard Windows folders.
    pub fn detect() -> Result<Self> {
        if let Some(home) = std::env::var_os("CLIPR_HOME").filter(|v| !v.is_empty()) {
            return Ok(Self::under(PathBuf::from(home)));
        }
        let config = dirs::config_dir().context("no config folder (%APPDATA%) found")?;
        let data = dirs::data_local_dir().context("no local data folder (%LOCALAPPDATA%) found")?;
        Ok(Self {
            config_dir: config.join("clipr"),
            data_dir: data.join("clipr"),
        })
    }

    /// Everything under one folder.
    pub fn under(root: PathBuf) -> Self {
        Self {
            config_dir: root.clone(),
            data_dir: root,
        }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    pub fn log_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }

    /// The port the running instance's web UI listens on, so a second launch (desktop shortcut, Start
    /// menu) can open the right address even after a port fallback.
    pub fn port_file(&self) -> PathBuf {
        self.data_dir.join("clipr.port")
    }
}
