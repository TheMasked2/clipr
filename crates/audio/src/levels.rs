//! Per-source peak levels for UI meters, shared between the capture thread and whoever asks.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use clipr_core::status::Level;

/// The level reported for a source that delivered nothing.
pub const SILENCE_DB: f32 = -100.0;

const WINDOW: Duration = Duration::from_millis(100);

/// Latest peak levels (dBFS) per source and channel (left, right): as delivered, and as recorded (after the
/// filters and volume).
pub struct Levels {
    keys: Vec<String>,
    names: Vec<String>,
    db: Vec<[AtomicU32; 2]>,
    after: Vec<[AtomicU32; 2]>,
    /// The source's volume in dB, added to the after level (the mixer applies it later).
    gain_db: Vec<AtomicU32>,
}

impl Levels {
    /// One entry per source: its `audio.filters` key and its display name.
    pub fn new(sources: Vec<(String, String)>) -> Self {
        let atom = |v: f32| AtomicU32::new(v.to_bits());
        let pairs = || {
            sources
                .iter()
                .map(|_| [atom(SILENCE_DB), atom(SILENCE_DB)])
                .collect()
        };
        let (db, after) = (pairs(), pairs());
        let gain_db = sources.iter().map(|_| atom(0.0)).collect();
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
        let get = |v: &[AtomicU32; 2]| v.each_ref().map(|a| f32::from_bits(a.load(Ordering::Relaxed)));
        (0..self.names.len())
            .map(|i| {
                let (db, after) = (get(&self.db[i]), get(&self.after[i]));
                Level {
                    source: self.keys[i].clone(),
                    name: self.names[i].clone(),
                    db: db[0].max(db[1]),
                    after: after[0].max(after[1]),
                    db_lr: db,
                    after_lr: after,
                }
            })
            .collect()
    }

    /// The level as delivered, per channel (left, right).
    pub fn set(&self, index: usize, db: [f32; 2]) {
        for (a, v) in self.db[index].iter().zip(db) {
            a.store(v.to_bits(), Ordering::Relaxed);
        }
    }

    /// The level after the source's filters, per channel; the source's volume is added here.
    pub fn set_after(&self, index: usize, db: [f32; 2]) {
        let gain = f32::from_bits(self.gain_db[index].load(Ordering::Relaxed));
        for (a, v) in self.after[index].iter().zip(db) {
            let v = if v <= SILENCE_DB { SILENCE_DB } else { v + gain };
            a.store(v.to_bits(), Ordering::Relaxed);
        }
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

/// Tracks the peak of one source's left and right channel over ~100 ms windows.
pub struct Meter {
    peak: [f32; 2],
    window_start: Instant,
}

impl Meter {
    pub fn new() -> Self {
        Self {
            peak: [0.0; 2],
            window_start: Instant::now(),
        }
    }

    /// Interleaved stereo samples (left, right).
    pub fn add(&mut self, samples: &[f32]) {
        for f in samples.chunks_exact(2) {
            self.peak[0] = self.peak[0].max(f[0].abs());
            self.peak[1] = self.peak[1].max(f[1].abs());
        }
    }

    /// The finished window's peaks in dBFS (left, right), once ~100 ms have passed.
    pub fn take(&mut self, now: Instant) -> Option<[f32; 2]> {
        if now.duration_since(self.window_start) < WINDOW {
            return None;
        }
        let db = self.peak.map(peak_db);
        self.peak = [0.0; 2];
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
        // Left peaks at 0.5, right at 0.25.
        m.add(&[0.1, -0.25, -0.5, 0.2]);
        assert_eq!(m.take(t0 + Duration::from_millis(50)), None);
        let [l, r] = m.take(t0 + Duration::from_millis(100)).unwrap();
        assert!((l + 6.02).abs() < 0.01);
        assert!((r + 12.04).abs() < 0.01);
        // Next window with no data is silence.
        assert_eq!(m.take(t0 + Duration::from_millis(200)), Some([SILENCE_DB; 2]));
    }

    #[test]
    fn levels_round_trip() {
        let l = Levels::new(vec![
            ("system".into(), "System".into()),
            ("mic".into(), "Mic".into()),
        ]);
        l.set(1, [-12.5, -30.0]);
        l.set_gain(1, 0.5);
        l.set_after(1, [-20.0, -40.0]);
        let s = l.snapshot();
        assert_eq!((s[0].source.as_str(), s[0].name.as_str()), ("system", "System"));
        assert_eq!((s[0].db, s[0].after), (SILENCE_DB, SILENCE_DB));
        // The single number is the louder channel.
        assert_eq!(
            (s[1].source.as_str(), s[1].db, s[1].db_lr),
            ("mic", -12.5, [-12.5, -30.0])
        );
        // Half the volume is about 6 dB less, on both channels.
        assert!((s[1].after + 26.02).abs() < 0.01);
        assert!((s[1].after_lr[1] + 46.02).abs() < 0.01);
        // Silence stays silence whatever the volume.
        l.set_after(1, [SILENCE_DB, -20.0]);
        assert_eq!(l.snapshot()[1].after_lr[0], SILENCE_DB);
    }
}
