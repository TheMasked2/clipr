//! What clipr is doing right now: idle, buffering, or recording.
//!
//! One [`Status`] lives for the whole run. Writers call [`Status::set`]; the tray, the web UI and (later)
//! Discord Rich Presence subscribe and react to changes.

use std::time::Instant;

use serde::{Deserialize, Serialize};
use tokio::sync::watch;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecState {
    /// No game runs: nothing is captured and nothing uses the GPU.
    #[default]
    Idle,
    /// The replay buffer is running.
    Buffering,
    /// A full recording is running (the buffer may run alongside it).
    Recording,
}

/// The part of the status that changes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Activity {
    pub state: RecState,
    /// The game being captured, if any.
    pub game: Option<String>,
    /// The hardware encoder in use, e.g. `AMD HW H.264 Encoder MFT`.
    pub encoder: Option<String>,
    /// What went wrong last, for the user (cleared once capture runs again).
    pub error: Option<String>,
    /// When the running full recording started, in seconds since the Unix epoch.
    pub recording_since: Option<u64>,
    /// The user paused the buffer (tray, hotkey or web UI).
    pub paused: bool,
}

/// What `/api/status` returns and the WebSocket pushes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusSnapshot {
    pub version: String,
    pub state: RecState,
    pub game: Option<String>,
    pub encoder: Option<String>,
    pub error: Option<String>,
    pub recording_since: Option<u64>,
    #[serde(default)]
    pub paused: bool,
    pub uptime_secs: u64,
}

pub struct Status {
    tx: watch::Sender<Activity>,
    started: Instant,
}

impl Default for Status {
    fn default() -> Self {
        Self::new()
    }
}

impl Status {
    pub fn new() -> Self {
        Self {
            tx: watch::Sender::new(Activity::default()),
            started: Instant::now(),
        }
    }

    /// Changes the activity. Subscribers only wake when it actually differs.
    pub fn set(&self, activity: Activity) {
        self.tx.send_if_modified(|current| {
            if *current == activity {
                return false;
            }
            *current = activity;
            true
        });
    }

    pub fn activity(&self) -> Activity {
        self.tx.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<Activity> {
        self.tx.subscribe()
    }

    pub fn snapshot(&self) -> StatusSnapshot {
        let activity = self.activity();
        StatusSnapshot {
            version: crate::VERSION.to_string(),
            state: activity.state,
            game: activity.game,
            encoder: activity.encoder,
            error: activity.error,
            recording_since: activity.recording_since,
            paused: activity.paused,
            uptime_secs: self.started.elapsed().as_secs(),
        }
    }

    /// The one-line text the tray menu shows, e.g. `● Buffering Valorant`.
    pub fn tray_line(&self) -> String {
        let activity = self.activity();
        let game = activity.game.as_deref();
        match (activity.state, game) {
            (RecState::Idle, _) if activity.error.is_some() => "Capture stopped (see the web UI)".into(),
            (RecState::Idle, _) if activity.paused => "Buffer paused".into(),
            (RecState::Idle, _) => "Not buffering".into(),
            (RecState::Buffering, Some(game)) => format!("● Buffering {game}"),
            (RecState::Buffering, None) => "● Buffering".into(),
            (RecState::Recording, Some(game)) => format!("● Recording {game}"),
            (RecState::Recording, None) => "● Recording".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_serialises_like_the_api_contract() {
        let status = Status::new();
        status.set(Activity {
            state: RecState::Buffering,
            game: Some("Valorant".into()),
            ..Default::default()
        });
        let json = serde_json::to_value(status.snapshot()).unwrap();
        assert_eq!(json["state"], "buffering");
        assert_eq!(json["game"], "Valorant");
        assert_eq!(json["version"], crate::VERSION);
        assert!(json["uptime_secs"].is_u64());
    }

    #[test]
    fn set_only_notifies_on_change() {
        let status = Status::new();
        let mut rx = status.subscribe();
        status.set(Activity::default());
        assert!(!rx.has_changed().unwrap());
        status.set(Activity {
            state: RecState::Recording,
            ..Default::default()
        });
        assert!(rx.has_changed().unwrap());
        rx.mark_unchanged();
        assert_eq!(status.tray_line(), "● Recording");
    }
}
