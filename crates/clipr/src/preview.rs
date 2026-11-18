//! The live capture preview. While a page shows it (and keeps asking), the video thread leaves a small
//! snapshot of what it captures here a few times a second; the web server hands the newest out as a JPEG.
//! Nobody watching costs nothing: the video thread only checks a flag.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use clipr_gpu::Snapshot;

/// How often the video thread takes a snapshot while someone watches, unless the page asks for another
/// rate (5 a second).
const DEFAULT_INTERVAL: Duration = Duration::from_millis(200);

/// Picture width when the page doesn't say (a small card), and the range a page may ask for.
const DEFAULT_WIDTH: u32 = 640;
const MIN_WIDTH: u32 = 320;
const MAX_WIDTH: u32 = 1920;
/// The widest picture above 15 pictures a second.
const FAST_MAX_WIDTH: u32 = 1280;

/// How long one request keeps snapshots coming. The page asks far more often, so closing it stops them soon.
const WATCH: Duration = Duration::from_secs(3);

struct Frame {
    snapshot: Snapshot,
    /// Made on the first request for this frame and shared with the next ones (more tabs, a slow poll).
    jpeg: OnceLock<Option<Arc<Vec<u8>>>>,
}

struct State {
    watch_until: Option<Instant>,
    /// The fastest rate a page asked for within the last [`WATCH`], so two pages (the PC at 30 fps, a phone at
    /// 2) don't take turns; a slower one takes over once the fast one stops asking.
    interval: Duration,
    interval_until: Option<Instant>,
    /// The widest picture a page asked for within the last [`WATCH`] (the same rule as `interval`), so Home on
    /// a big screen gets a sharp picture and a phone a small one.
    width: u32,
    width_until: Option<Instant>,
    latest: Option<Arc<Frame>>,
    /// Why the GPU can't make snapshots right now (the video thread tries again a little later).
    error: Option<String>,
}

static STATE: Mutex<State> = Mutex::new(State {
    watch_until: None,
    interval: DEFAULT_INTERVAL,
    interval_until: None,
    width: DEFAULT_WIDTH,
    width_until: None,
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

/// The biggest snapshot the pages want now: their width, 16:9 (a wider screen is fitted in by width).
pub fn max_size() -> (u32, u32) {
    let w = state().width;
    (w, (w * 9 / 16) & !1)
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
/// `width` is how wide the page shows it, in screen pixels (clamped to 320–1920).
pub fn jpeg(fps: u32, width: u32) -> Result<Option<Arc<Vec<u8>>>, String> {
    let frame = {
        let mut s = state();
        let now = Instant::now();
        s.watch_until = Some(now + WATCH);
        let interval = Duration::from_millis(1000 / u64::from(fps.clamp(1, 30)));
        if interval <= s.interval || s.interval_until.is_none_or(|t| now >= t) {
            s.interval = interval;
            s.interval_until = Some(now + WATCH);
        }
        // Big and fast together costs the PC the most (a full-HD JPEG 30 times a second): fast stays at 1280.
        let cap = if fps > 15 { FAST_MAX_WIDTH } else { MAX_WIDTH };
        let width = width.clamp(MIN_WIDTH, cap) & !1;
        if width >= s.width || s.width_until.is_none_or(|t| now >= t) {
            s.width = width;
            s.width_until = Some(now + WATCH);
        }
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
        assert_eq!(jpeg(5, 640), Ok(None));
        assert!(wanted());
        let snapshot = || Snapshot {
            width: 4,
            height: 2,
            bgra: vec![128; 4 * 2 * 4],
        };
        publish(snapshot());
        let a = jpeg(5, 640).unwrap().expect("a frame");
        assert_eq!(&a[..2], [0xFF, 0xD8]);
        // The same frame isn't encoded twice.
        assert!(Arc::ptr_eq(&a, &jpeg(5, 640).unwrap().unwrap()));
        // A failure replaces the frame until the next snapshot.
        fail("no BGRA".into());
        assert_eq!(jpeg(5, 640), Err("no BGRA".into()));
        publish(snapshot());
        assert!(jpeg(5, 640).unwrap().is_some());
        // The asking page sets how often frames are taken.
        assert_eq!(interval(), Duration::from_millis(200));
        jpeg(30, 640).unwrap();
        assert_eq!(interval(), Duration::from_millis(33));
        jpeg(100, 640).unwrap();
        assert_eq!(interval(), Duration::from_millis(33));
        // A slower page doesn't slow down a faster one that still watches.
        jpeg(2, 640).unwrap();
        assert_eq!(interval(), Duration::from_millis(33));
        // The widest page sets the size, within 320–1920; a narrower one doesn't shrink it.
        jpeg(5, 1280).unwrap();
        assert_eq!(max_size(), (1280, 720));
        jpeg(5, 900).unwrap();
        assert_eq!(max_size(), (1280, 720));
        jpeg(5, 9000).unwrap();
        assert_eq!(max_size(), (1920, 1080));
        // Fast asks are held to 1280 (the slow full-HD ask above still counts for its 3 s).
        clear();
        STATE.lock().unwrap().width_until = None;
        jpeg(30, 1920).unwrap();
        assert_eq!(max_size(), (1280, 720));
        drop(ClearOnDrop);
        assert_eq!(jpeg(5, 640), Ok(None));
    }
}
