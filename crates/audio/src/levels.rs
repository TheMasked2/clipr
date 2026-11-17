//! Per-source peak levels for UI meters, shared between the capture thread and whoever asks.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use clipr_core::status::Level;

/// The level reported for a source that delivered nothing.
pub const SILENCE_DB: f32 = -100.0;

const WINDOW: Duration = Duration::from_millis(100);

/// Latest peak levels (dBFS) per source: as delivered, and as recorded (after the filters and volume).
pub struct Levels {
    keys: Vec<String>,
    names: Vec<String>,
    db: Vec<AtomicU32>,
    after: Vec<AtomicU32>,
    /// The source's volume in dB, added to the after level (the mixer applies it later).
    gain_db: Vec<AtomicU32>,
}

impl Levels {
    /// One entry per source: its `audio.filters` key and its display name.
    pub fn new(sources: Vec<(String, String)>) -> Self {
        let atoms = |v: f32| sources.iter().map(|_| AtomicU32::new(v.to_bits())).collect();
        let (db, after, gain_db) = (atoms(SILENCE_DB), atoms(SILENCE_DB), atoms(0.0));
        let (keys, names) = sources.into_iter().unzip();
        Self {
            keys,
            names,
            db,
            after,
            gain_db,
        }
    }

    pub fn snapshot(&self) -> Vec<Level> {
        let get = |v: &AtomicU32| f32::from_bits(v.load(Ordering::Relaxed));
        (0..self.names.len())
            .map(|i| Level {
                source: self.keys[i].clone(),
                name: self.names[i].clone(),
                db: get(&self.db[i]),
                after: get(&self.after[i]),
            })
            .collect()
    }

    pub fn set(&self, index: usize, db: f32) {
        self.db[index].store(db.to_bits(), Ordering::Relaxed);
    }

    /// The level after the source's filters; the source's volume is added here.
    pub fn set_after(&self, index: usize, db: f32) {
        let db = if db <= SILENCE_DB {
            SILENCE_DB
        } else {
            db + f32::from_bits(self.gain_db[index].load(Ordering::Relaxed))
        };
        self.after[index].store(db.to_bits(), Ordering::Relaxed);
    }

    /// The source's linear volume (1.0 = unchanged).
    pub fn set_gain(&self, index: usize, gain: f32) {
        let db = if gain <= 0.0 {
            SILENCE_DB
        } else {
            20.0 * gain.log10()
        };
        self.gain_db[index].store(db.to_bits(), Ordering::Relaxed);
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
        let l = Levels::new(vec![
            ("system".into(), "System".into()),
            ("mic".into(), "Mic".into()),
        ]);
        l.set(1, -12.5);
        l.set_gain(1, 0.5);
        l.set_after(1, -20.0);
        let s = l.snapshot();
        assert_eq!((s[0].source.as_str(), s[0].name.as_str()), ("system", "System"));
        assert_eq!((s[0].db, s[0].after), (SILENCE_DB, SILENCE_DB));
        assert_eq!((s[1].source.as_str(), s[1].db), ("mic", -12.5));
        // Half the volume is about 6 dB less.
        assert!((s[1].after + 26.02).abs() < 0.01);
        // Silence stays silence whatever the volume.
        l.set_after(1, SILENCE_DB);
        assert_eq!(l.snapshot()[1].after, SILENCE_DB);
    }
}
