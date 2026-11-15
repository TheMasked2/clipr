//! Per-source peak levels for UI meters, shared between the capture thread and whoever asks.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// The level reported for a source that delivered nothing.
pub const SILENCE_DB: f32 = -100.0;

const WINDOW: Duration = Duration::from_millis(100);

/// Latest peak level (dBFS) per source.
pub struct Levels {
    names: Vec<String>,
    db: Vec<AtomicU32>,
}

impl Levels {
    pub fn new(names: Vec<String>) -> Self {
        let db = names
            .iter()
            .map(|_| AtomicU32::new(SILENCE_DB.to_bits()))
            .collect();
        Self { names, db }
    }

    pub fn snapshot(&self) -> Vec<(String, f32)> {
        self.names
            .iter()
            .zip(&self.db)
            .map(|(n, v)| (n.clone(), f32::from_bits(v.load(Ordering::Relaxed))))
            .collect()
    }

    pub fn set(&self, index: usize, db: f32) {
        self.db[index].store(db.to_bits(), Ordering::Relaxed);
    }
}

/// Tracks the peak of one source over ~100 ms windows.
pub struct Meter {
    peak: f32,
    window_start: Instant,
}

impl Meter {
    pub fn new() -> Self {
        Self {
            peak: 0.0,
            window_start: Instant::now(),
        }
    }

    pub fn add(&mut self, samples: &[f32]) {
        self.peak = samples.iter().fold(self.peak, |p, s| p.max(s.abs()));
    }

    /// The finished window's peak in dBFS, once ~100 ms have passed.
    pub fn take(&mut self, now: Instant) -> Option<f32> {
        if now.duration_since(self.window_start) < WINDOW {
            return None;
        }
        let db = peak_db(self.peak);
        self.peak = 0.0;
        self.window_start = now;
        Some(db)
    }
}

impl Default for Meter {
    fn default() -> Self {
        Self::new()
    }
}

pub fn peak_db(peak: f32) -> f32 {
    if peak <= 1e-5 {
        SILENCE_DB
    } else {
        20.0 * peak.log10()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meter_reports_the_peak_per_window() {
        let mut m = Meter::new();
        let t0 = m.window_start;
        m.add(&[0.1, -0.5, 0.2]);
        assert_eq!(m.take(t0 + Duration::from_millis(50)), None);
        let db = m.take(t0 + Duration::from_millis(100)).unwrap();
        assert!((db + 6.02).abs() < 0.01);
        // Next window with no data is silence.
        assert_eq!(m.take(t0 + Duration::from_millis(200)), Some(SILENCE_DB));
    }

    #[test]
    fn levels_round_trip() {
        let l = Levels::new(vec!["System".into(), "Mic".into()]);
        l.set(1, -12.5);
        let s = l.snapshot();
        assert_eq!(s[0], ("System".into(), SILENCE_DB));
        assert_eq!(s[1], ("Mic".into(), -12.5));
    }
}
