//! What clipr knows about a saved clip when it hands it to the Discord and cloud queues.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// What clipr knows about a clip when it queues it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClipInfo {
    /// The library's id (for the UI).
    pub id: String,
    pub path: PathBuf,
    pub title: String,
    pub game: String,
    pub duration_secs: Option<f64>,
    pub size_bytes: u64,
    /// The library's thumbnail, if it has one yet.
    pub thumb: Option<PathBuf>,
    /// `clip` or `recording`.
    pub kind: String,
}
