//! The event bus: one-off things that happened (a clip was saved, a game started), for anyone who cares.
//!
//! Ongoing state lives in [`crate::status::Status`]; this is for discrete events. Slow subscribers miss old
//! events rather than slowing down the sender (a `broadcast` channel), since nothing on the hot path may wait
//! on the UI or Discord.

use tokio::sync::broadcast;

/// How many events a slow subscriber can fall behind before it starts missing them.
const CAPACITY: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Event {
    /// The user asked clipr to quit (tray menu, or a shutdown signal).
    ShutdownRequested,
    /// A clip or recording was written. `path` is absolute.
    ClipSaved { path: std::path::PathBuf },
    /// Saving a clip failed; `message` is for the user.
    ClipFailed { message: String },
    /// The settings were saved (any page, a fader, the overlay): open pages load them again, so one page
    /// doesn't later save its old copy over another's change.
    ConfigChanged,
}

#[derive(Clone)]
pub struct Bus {
    tx: broadcast::Sender<Event>,
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus {
    pub fn new() -> Self {
        Self {
            tx: broadcast::Sender::new(CAPACITY),
        }
    }

    /// Sends an event. Having no subscribers is fine.
    pub fn emit(&self, event: Event) {
        let _ = self.tx.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscribers_get_events_and_no_subscriber_is_fine() {
        let bus = Bus::new();
        bus.emit(Event::ShutdownRequested);
        let mut rx = bus.subscribe();
        bus.emit(Event::ShutdownRequested);
        assert_eq!(rx.try_recv().unwrap(), Event::ShutdownRequested);
        assert!(rx.try_recv().is_err());
    }
}
