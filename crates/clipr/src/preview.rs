//! The live capture preview. While a page shows it (and keeps asking), the video thread leaves a small
//! snapshot of what it captures here a few times a second; the web server hands the newest out as a JPEG.
//! Nobody watching costs nothing: the video thread only checks a flag.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use clipr_gpu::Snapshot;

/// How often the video thread takes a snapshot while someone watches, unless the page asks for another
/// rate (5 a second).
const DEFAULT_INTERVAL: Duration = Duration::from_millis(200);

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
    /// The rate the page asked for last.
    interval: Duration,
    latest: Option<Arc<Frame>>,
    /// Why the GPU can't make snapshots right now (the video thread tries again a little later).
    error: Option<String>,
}

static STATE: Mutex<State> = Mutex::new(State {
    watch_until: None,
    interval: DEFAULT_INTERVAL,
    latest: None,
    error: None,
});

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// How often the video thread takes a snapshot now.
pub fn interval() -> Duration {
    state().interval
}

/// Whether a page watches the preview now (the video thread asks on every tick).
pub fn wanted() -> bool {
    state().watch_until.is_some_and(|t| Instant::now() < t)
}

/// The video thread's newest snapshot.
pub fn publish(snapshot: Snapshot) {
    let mut s = state();
    s.error = None;
    s.latest = Some(Arc::new(Frame {
        snapshot,
        jpeg: OnceLock::new(),
    }));
}

/// The GPU couldn't make a snapshot: the page says why instead of waiting forever.
pub fn fail(error: String) {
    let mut s = state();
    s.latest = None;
    s.error = Some(error);
}

/// The video pipeline stopped: there's nothing to show until it runs again.
pub fn clear() {
    let mut s = state();
    s.latest = None;
    s.error = None;
}

/// Clears the preview when dropped, so a video thread that ends any way (even a panic) leaves no old frame.
pub struct ClearOnDrop;

impl Drop for ClearOnDrop {
    fn drop(&mut self) {
        clear();
    }
}

/// Keeps snapshots coming for a while and returns the newest one as a JPEG: `Ok(None)` while nothing is
/// captured or before the first snapshot arrives, `Err` with the reason when the GPU can't make them.
pub fn jpeg(fps: u32) -> Result<Option<Arc<Vec<u8>>>, String> {
    let frame = {
        let mut s = state();
        s.watch_until = Some(Instant::now() + WATCH);
        s.interval = Duration::from_millis(1000 / u64::from(fps.clamp(1, 30)));
        match (&s.latest, &s.error) {
            (Some(frame), _) => frame.clone(),
            (None, Some(error)) => return Err(error.clone()),
            (None, None) => return Ok(None),
        }
    };
    let jpeg = frame
        .jpeg
        .get_or_init(|| {
            let s = &frame.snapshot;
            clipr_library::jpeg(s.width, s.height, &s.bgra)
                .map_err(|e| tracing::warn!("can't make the preview image: {e:#}"))
                .ok()
                .map(Arc::new)
        })
        .clone();
    Ok(jpeg)
}

#[cfg(test)]
mod tests {
    use super::*;

    // One test, since the state is global.
    #[test]
    fn asking_starts_snapshots_and_returns_the_newest() {
        clear();
        assert_eq!(jpeg(5), Ok(None));
        assert!(wanted());
        let snapshot = || Snapshot {
            width: 4,
            height: 2,
            bgra: vec![128; 4 * 2 * 4],
        };
        publish(snapshot());
        let a = jpeg(5).unwrap().expect("a frame");
        assert_eq!(&a[..2], [0xFF, 0xD8]);
        // The same frame isn't encoded twice.
        assert!(Arc::ptr_eq(&a, &jpeg(5).unwrap().unwrap()));
        // A failure replaces the frame until the next snapshot.
        fail("no BGRA".into());
        assert_eq!(jpeg(5), Err("no BGRA".into()));
        publish(snapshot());
        assert!(jpeg(5).unwrap().is_some());
        // The asking page sets how often frames are taken.
        assert_eq!(interval(), Duration::from_millis(200));
        jpeg(30).unwrap();
        assert_eq!(interval(), Duration::from_millis(33));
        jpeg(100).unwrap();
        assert_eq!(interval(), Duration::from_millis(33));
        drop(ClearOnDrop);
        assert_eq!(jpeg(5), Ok(None));
    }
}
