//! The live capture preview. While a page shows it (and keeps asking), the video thread leaves a small
//! snapshot of what it captures here a few times a second; the web server hands the newest out as a JPEG.
//! Nobody watching costs nothing: the video thread only checks a flag.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use clipr_gpu::Snapshot;

/// How often the video thread takes a snapshot while someone watches (5 a second).
pub const INTERVAL: Duration = Duration::from_millis(200);

/// The biggest snapshot: about what a dashboard card shows.
pub const MAX_SIZE: (u32, u32) = (640, 360);

/// How long one request keeps snapshots coming. The page asks far more often, so closing it stops them soon.
const WATCH: Duration = Duration::from_secs(3);

struct Frame {
    snapshot: Snapshot,
    /// Made on the first request for this frame and shared with the next ones (more tabs, a slow poll).
    jpeg: OnceLock<Option<Arc<Vec<u8>>>>,
}

struct State {
    watch_until: Option<Instant>,
    latest: Option<Arc<Frame>>,
}

static STATE: Mutex<State> = Mutex::new(State {
    watch_until: None,
    latest: None,
});

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Whether a page watches the preview now (the video thread asks on every tick).
pub fn wanted() -> bool {
    state().watch_until.is_some_and(|t| Instant::now() < t)
}

/// The video thread's newest snapshot.
pub fn publish(snapshot: Snapshot) {
    state().latest = Some(Arc::new(Frame {
        snapshot,
        jpeg: OnceLock::new(),
    }));
}

/// The video pipeline stopped: there's nothing to show until it runs again.
pub fn clear() {
    state().latest = None;
}

/// Keeps snapshots coming for a while and returns the newest one as a JPEG (`None` while nothing is
/// captured, or before the first snapshot arrives).
pub fn jpeg() -> Option<Arc<Vec<u8>>> {
    let frame = {
        let mut s = state();
        s.watch_until = Some(Instant::now() + WATCH);
        s.latest.clone()
    }?;
    frame
        .jpeg
        .get_or_init(|| {
            let s = &frame.snapshot;
            clipr_library::jpeg(s.width, s.height, &s.bgra)
                .map_err(|e| tracing::warn!("can't make the preview image: {e:#}"))
                .ok()
                .map(Arc::new)
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    // One test, since the state is global.
    #[test]
    fn asking_starts_snapshots_and_returns_the_newest() {
        clear();
        assert!(jpeg().is_none());
        assert!(wanted());
        publish(Snapshot {
            width: 4,
            height: 2,
            bgra: vec![128; 4 * 2 * 4],
        });
        let a = jpeg().expect("a frame");
        assert_eq!(&a[..2], [0xFF, 0xD8]);
        // The same frame isn't encoded twice.
        assert!(Arc::ptr_eq(&a, &jpeg().unwrap()));
        clear();
        assert!(jpeg().is_none());
    }
}
