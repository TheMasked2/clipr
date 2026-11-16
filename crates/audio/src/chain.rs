//! Per-source filter chains, like OBS: each source runs its own list of filters (noise suppression, noise
//! gate, compressor, limiter, gain) in the order the user set, then push-to-talk for the microphone.
//!
//! All pure sample processing at 48 kHz, so it is unit tested without a device. Chains work on interleaved
//! stereo; the microphone is first averaged to mono (one channel), which halves the work and keeps both
//! sides equal.

use std::collections::VecDeque;

use clipr_core::config::{AudioFilter, FilterKind};

use crate::mix::RATE;

/// RNNoise's frame size: 10 ms at 48 kHz.
const NS_FRAME: usize = nnnoiseless::DenoiseState::FRAME_SIZE;
/// How late a suppressed sample comes out: RNNoise's own overlap-add holds one frame back, and the FIFO
/// that turns arbitrary packet sizes into whole frames adds another.
const NS_DELAY: usize = 2 * NS_FRAME;

const SR: f32 = RATE as f32;

fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// A one-pole smoothing coefficient for a time constant of `ms`.
fn coef(ms: u32) -> f32 {
    let samples = (ms.max(1) as f32 / 1000.0) * SR;
    (-1.0 / samples).exp()
}

/// The peak of one frame across its channels: linked detection, so stereo filters act on both sides alike.
fn frame_peak(frame: &[f32]) -> f32 {
    frame.iter().fold(0.0f32, |p, s| p.max(s.abs()))
}

trait Filter: Send {
    /// Processes interleaved samples with `channels` channels in place.
    fn process(&mut self, samples: &mut [f32], channels: usize);
    /// How many frames later than its input the output is.
    fn delay_frames(&self) -> usize {
        0
    }
}

/// RNNoise for one channel behind a FIFO: it works on frames of 480 samples, so the output lags by exactly
/// [`NS_DELAY`] (20 ms), and always has as many samples as the input.
struct DenoiseChannel {
    state: Box<nnnoiseless::DenoiseState<'static>>,
    input: Vec<f32>,
    wet: VecDeque<f32>,
    /// The unprocessed input, delayed by the same frame, for blending at less than full strength.
    dry: VecDeque<f32>,
}

impl DenoiseChannel {
    fn new() -> Self {
        Self {
            state: nnnoiseless::DenoiseState::new(),
            input: Vec::new(),
            // The FIFO's one-frame head start: the output never runs dry. RNNoise adds the other frame.
            wet: std::iter::repeat_n(0.0, NS_FRAME).collect(),
            // The original, delayed like the cleaned sound so the two line up when blended.
            dry: std::iter::repeat_n(0.0, NS_DELAY).collect(),
        }
    }
}

struct Suppressor {
    channels: Vec<DenoiseChannel>,
    strength: f32,
}

impl Suppressor {
    fn new(strength: u32) -> Self {
        Self {
            channels: Vec::new(),
            strength: strength.min(100) as f32 / 100.0,
        }
    }
}

impl Filter for Suppressor {
    fn process(&mut self, samples: &mut [f32], channels: usize) {
        while self.channels.len() < channels {
            self.channels.push(DenoiseChannel::new());
        }
        let mut frame = [0.0f32; NS_FRAME];
        for (c, ch) in self.channels.iter_mut().enumerate().take(channels) {
            // RNNoise expects 16-bit scale.
            for s in samples.iter().skip(c).step_by(channels) {
                ch.input.push(s * 32768.0);
                ch.dry.push_back(*s);
            }
            while ch.input.len() >= NS_FRAME {
                ch.state.process_frame(&mut frame, &ch.input[..NS_FRAME]);
                ch.wet.extend(frame.iter().map(|s| s / 32768.0));
                ch.input.drain(..NS_FRAME);
            }
            for s in samples.iter_mut().skip(c).step_by(channels) {
                let wet = ch.wet.pop_front().unwrap_or(0.0);
                let dry = ch.dry.pop_front().unwrap_or(0.0);
                *s = wet * self.strength + dry * (1.0 - self.strength);
            }
        }
    }

    fn delay_frames(&self) -> usize {
        NS_DELAY
    }
}

/// A noise gate with hysteresis: opens when the level passes `open`, and once it drops below `close`
/// stays open for the hold time, then fades out over the release time.
struct Gate {
    open: f32,
    close: f32,
    hold: u32,
    attack_step: f32,
    release_step: f32,
    /// Fast-attack peak follower (~40 ms decay).
    env: f32,
    is_open: bool,
    hold_left: u32,
    gain: f32,
}

impl Gate {
    const DECAY: f32 = 0.9995;

    fn new(open_db: i32, close_db: i32, attack_ms: u32, hold_ms: u32, release_ms: u32) -> Self {
        // The close level can't be above the open level, or the gate would chatter.
        let close_db = close_db.min(open_db);
        Self {
            open: db_to_lin(open_db as f32),
            close: db_to_lin(close_db as f32),
            hold: RATE * hold_ms.min(10_000) / 1000,
            attack_step: 1.0 / (SR * attack_ms.max(1) as f32 / 1000.0),
            release_step: 1.0 / (SR * release_ms.max(1) as f32 / 1000.0),
            env: 0.0,
            is_open: false,
            hold_left: 0,
            gain: 0.0,
        }
    }
}

impl Filter for Gate {
    fn process(&mut self, samples: &mut [f32], channels: usize) {
        for frame in samples.chunks_exact_mut(channels) {
            self.env = frame_peak(frame).max(self.env * Self::DECAY);
            if self.env >= self.open {
                self.is_open = true;
                self.hold_left = self.hold;
            } else if self.env >= self.close && self.is_open {
                self.hold_left = self.hold;
            } else if self.hold_left > 0 {
                self.hold_left -= 1;
            } else {
                self.is_open = false;
            }
            self.gain = if self.is_open {
                (self.gain + self.attack_step).min(1.0)
            } else {
                (self.gain - self.release_step).max(0.0)
            };
            frame.iter_mut().for_each(|s| *s *= self.gain);
        }
    }
}

/// A feed-forward compressor with makeup gain.
struct Compressor {
    threshold_db: f32,
    slope: f32,
    makeup_db: f32,
    attack: f32,
    release: f32,
    env_db: f32,
}

impl Compressor {
    fn new(threshold_db: i32, ratio: u32, attack_ms: u32, release_ms: u32, makeup_db: i32) -> Self {
        let ratio = ratio.clamp(1, 20) as f32;
        Self {
            threshold_db: threshold_db as f32,
            slope: 1.0 - 1.0 / ratio,
            makeup_db: makeup_db.clamp(-30, 30) as f32,
            attack: coef(attack_ms),
            release: coef(release_ms),
            env_db: -120.0,
        }
    }
}

impl Filter for Compressor {
    fn process(&mut self, samples: &mut [f32], channels: usize) {
        for frame in samples.chunks_exact_mut(channels) {
            let level = 20.0 * (frame_peak(frame) + 1e-9).log10();
            let c = if level > self.env_db {
                self.attack
            } else {
                self.release
            };
            self.env_db = c * self.env_db + (1.0 - c) * level;
            let over = self.env_db - self.threshold_db;
            let reduction = if over > 0.0 { -over * self.slope } else { 0.0 };
            let g = db_to_lin(reduction + self.makeup_db);
            frame.iter_mut().for_each(|s| *s *= g);
        }
    }
}

/// A peak limiter: the gain drops at once to keep every sample at or below the ceiling, then recovers
/// over the release time. No look-ahead, so it adds no delay.
struct Limiter {
    ceiling: f32,
    release: f32,
    gain: f32,
}

impl Limiter {
    fn new(ceiling_db: i32, release_ms: u32) -> Self {
        Self {
            ceiling: db_to_lin(ceiling_db.clamp(-60, 0) as f32),
            release: coef(release_ms),
            gain: 1.0,
        }
    }
}

impl Filter for Limiter {
    fn process(&mut self, samples: &mut [f32], channels: usize) {
        for frame in samples.chunks_exact_mut(channels) {
            // Recover toward unity, then cut back down if this frame needs it.
            self.gain = 1.0 - (1.0 - self.gain) * self.release;
            let peak = frame_peak(frame);
            if peak * self.gain > self.ceiling {
                self.gain = self.ceiling / peak;
            }
            frame.iter_mut().for_each(|s| *s *= self.gain);
        }
    }
}

struct Gain(f32);

impl Filter for Gain {
    fn process(&mut self, samples: &mut [f32], _channels: usize) {
        samples.iter_mut().for_each(|s| *s *= self.0);
    }
}

fn build(kind: &FilterKind) -> Box<dyn Filter> {
    match *kind {
        FilterKind::NoiseSuppression { strength } => Box::new(Suppressor::new(strength)),
        FilterKind::NoiseGate {
            open_db,
            close_db,
            attack_ms,
            hold_ms,
            release_ms,
        } => Box::new(Gate::new(open_db, close_db, attack_ms, hold_ms, release_ms)),
        FilterKind::Compressor {
            threshold_db,
            ratio,
            attack_ms,
            release_ms,
            makeup_db,
        } => Box::new(Compressor::new(
            threshold_db,
            ratio,
            attack_ms,
            release_ms,
            makeup_db,
        )),
        FilterKind::Limiter {
            ceiling_db,
            release_ms,
        } => Box::new(Limiter::new(ceiling_db, release_ms)),
        FilterKind::Gain { db } => Box::new(Gain(db_to_lin(db.clamp(-60, 30) as f32))),
    }
}

/// Push-to-talk: open while the key is held and for `release_ms` after it's let go.
pub struct Ptt {
    release_ms: u64,
    last_held: Option<u64>,
}

impl Ptt {
    pub fn new(release_ms: u32) -> Self {
        Self {
            release_ms: release_ms as u64,
            last_held: None,
        }
    }

    /// Feeds the key state at `now_ms`; returns whether the microphone is open.
    pub fn update(&mut self, held: bool, now_ms: u64) -> bool {
        if held {
            self.last_held = Some(now_ms);
            return true;
        }
        self.last_held
            .is_some_and(|t| now_ms.saturating_sub(t) < self.release_ms)
    }
}

/// The virtual-key code of a push-to-talk key: a single key named as in `clipr_core::hotkey` (modifiers
/// are ignored), or `Mouse4`, `Mouse5`, `MouseMiddle`. `Ok(None)` for an empty name.
pub fn ptt_vk(name: &str) -> anyhow::Result<Option<i32>> {
    let key = name.rsplit('+').next().unwrap_or("").trim();
    match key.to_ascii_lowercase().as_str() {
        "mouse4" | "xbutton1" => Ok(Some(0x05)),
        "mouse5" | "xbutton2" => Ok(Some(0x06)),
        "mousemiddle" | "mouse3" => Ok(Some(0x04)),
        _ => Ok(clipr_core::hotkey::parse(key)?.map(|h| h.vk as i32)),
    }
}

/// One source's processing: its enabled filters in order, then the push-to-talk fade.
pub struct SourceChain {
    /// The enabled filters' settings, to rebuild them after a gap.
    kinds: Vec<FilterKind>,
    filters: Vec<Box<dyn Filter>>,
    /// Average to mono before filtering (the microphone).
    mono: bool,
    /// Push-to-talk fade (0 = muted, 1 = open), moved 10 ms per full swing.
    ptt_gain: f32,
    buf: Vec<f32>,
}

impl SourceChain {
    pub fn new(filters: &[AudioFilter], mono: bool) -> Self {
        let kinds: Vec<FilterKind> = filters
            .iter()
            .filter(|f| f.enabled)
            // Noise suppression at 0 % changes nothing but would still delay the source by 20 ms.
            .filter(|f| !matches!(f.kind, FilterKind::NoiseSuppression { strength: 0 }))
            .map(|f| f.kind.clone())
            .collect();
        Self {
            filters: kinds.iter().map(build).collect(),
            kinds,
            mono,
            ptt_gain: 1.0,
            buf: Vec::new(),
        }
    }

    /// Whether the chain changes anything for a source without push-to-talk.
    pub fn is_empty(&self) -> bool {
        self.filters.is_empty() && !self.mono
    }

    /// How many frames later than its input the output is (noise suppression works in 10 ms frames).
    pub fn delay_frames(&self) -> usize {
        self.filters.iter().map(|f| f.delay_frames()).sum()
    }

    /// Pushes out what the filters still hold (the last [`SourceChain::delay_frames`] frames of the sound so
    /// far), appending them to `out` as stereo, then starts the filters fresh. For a gap in the input.
    pub fn flush(&mut self, open: bool, out: &mut Vec<f32>) {
        let zeros = vec![0.0f32; self.delay_frames() * 2];
        self.process(&zeros, open, out);
        self.filters = self.kinds.iter().map(build).collect();
    }

    /// Runs interleaved stereo `input` through the chain, appending the same number of stereo frames to
    /// `out`. `open` is the push-to-talk state (true when there is no push-to-talk).
    pub fn process(&mut self, input: &[f32], open: bool, out: &mut Vec<f32>) {
        self.buf.clear();
        let channels = if self.mono {
            self.buf
                .extend(input.chunks_exact(2).map(|f| (f[0] + f[1]) * 0.5));
            1
        } else {
            self.buf.extend_from_slice(input);
            2
        };
        for f in &mut self.filters {
            f.process(&mut self.buf, channels);
        }
        let target = if open { 1.0 } else { 0.0 };
        let step = 1.0 / (SR * 0.010);
        out.reserve(input.len());
        for frame in self.buf.chunks_exact(channels) {
            self.ptt_gain = if self.ptt_gain < target {
                (self.ptt_gain + step).min(target)
            } else {
                (self.ptt_gain - step).max(target)
            };
            let (l, r) = if channels == 1 {
                (frame[0], frame[0])
            } else {
                (frame[0], frame[1])
            };
            out.push(l * self.ptt_gain);
            out.push(r * self.ptt_gain);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(amp: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| amp * (i as f32 * 2.0 * std::f32::consts::PI * 440.0 / SR).sin())
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    fn peak(x: &[f32]) -> f32 {
        frame_peak(x)
    }

    fn on(kind: FilterKind) -> AudioFilter {
        AudioFilter { enabled: true, kind }
    }

    fn defaults(name: &str) -> FilterKind {
        FilterKind::defaults(name).unwrap()
    }

    #[test]
    fn gate_opens_for_loud_and_closes_after_hold_and_release() {
        let mut g = Gate::new(-45, -45, 10, 150, 100);
        let mut quiet = vec![0.001f32; 4800]; // -60 dBFS
        g.process(&mut quiet, 1);
        assert!(quiet.iter().all(|&v| v == 0.0), "closed at the start");

        let mut loud = vec![0.5f32; 2400];
        g.process(&mut loud, 1);
        assert!(loud[0] < 0.1, "attack ramps");
        assert!(loud[1000] > 0.49, "open after the 10 ms attack");

        // 50 ms later the follower and hold keep it open.
        let mut tail = vec![0.001f32; 2400];
        g.process(&mut tail, 1);
        assert!(tail[2399] > 0.0009);
        // Well past follower decay + 150 ms hold + 100 ms release: closed.
        let mut later = vec![0.001f32; 48_000];
        g.process(&mut later, 1);
        assert!(later[47_999] == 0.0);
    }

    #[test]
    fn gate_hysteresis_keeps_it_open_between_the_levels() {
        // Opens at -20 dB, closes below -40 dB: a -30 dB signal after a loud one keeps it open.
        let mut g = Gate::new(-20, -40, 1, 0, 1);
        let mut loud = vec![0.5f32; 4800];
        g.process(&mut loud, 1);
        let mut mid = vec![0.0316f32; 48_000]; // -30 dB
        g.process(&mut mid, 1);
        assert!(mid[47_999] > 0.03, "stays open above the close level");
        // A fresh gate never opens for the same -30 dB signal.
        let mut g = Gate::new(-20, -40, 1, 0, 1);
        let mut mid = vec![0.0316f32; 48_000];
        g.process(&mut mid, 1);
        assert_eq!(mid[47_999], 0.0);
    }

    #[test]
    fn compressor_squeezes_loud_and_lifts_quiet() {
        let mut c = Compressor::new(-20, 3, 5, 80, 4);
        let mut loud = vec![0.5f32; 24_000]; // -6 dBFS
        c.process(&mut loud, 1);
        // 14 dB over: -9.33 dB reduction + 4 dB makeup = -5.33 dB.
        let want = 0.5 * db_to_lin(-14.0 * 2.0 / 3.0 + 4.0);
        assert!((loud[23_999] - want).abs() < 0.01, "{} vs {want}", loud[23_999]);

        let mut c = Compressor::new(-20, 3, 5, 80, 4);
        let mut quiet = vec![0.01f32; 24_000]; // -40 dBFS: below threshold, makeup only
        c.process(&mut quiet, 1);
        assert!((quiet[23_999] - 0.01 * db_to_lin(4.0)).abs() < 1e-4);
    }

    #[test]
    fn limiter_holds_loud_peaks_under_the_ceiling() {
        let mut l = Limiter::new(-1, 100);
        let mut loud = tone(1.8, 48_000); // a hot source: would clip hard
        l.process(&mut loud, 1);
        let ceiling = db_to_lin(-1.0);
        assert!(peak(&loud) <= ceiling + 1e-6, "peak {}", peak(&loud));
        assert!(peak(&loud) > 0.85, "still loud, not squashed");
    }

    #[test]
    fn limiter_leaves_quiet_audio_alone_and_recovers() {
        let mut l = Limiter::new(-1, 100);
        let mut quiet = tone(0.5, 4800);
        let before = quiet.clone();
        l.process(&mut quiet, 1);
        assert_eq!(quiet, before, "under the ceiling: untouched");

        let mut spike = vec![1.0f32];
        l.process(&mut spike, 1);
        assert!((spike[0] - db_to_lin(-1.0)).abs() < 1e-6);
        // Half a second later the gain is back at unity.
        let mut after = tone(0.5, 24_000);
        l.process(&mut after, 1);
        let want = tone(0.5, 24_000);
        assert!(
            after[19_200..]
                .iter()
                .zip(&want[19_200..])
                .all(|(a, b)| (a - b).abs() < 1e-3)
        );
    }

    #[test]
    fn stereo_filters_are_linked() {
        // A loud left side pulls the right side down by the same amount.
        let mut l = Limiter::new(-6, 100);
        let mut frames = vec![1.0f32, 0.1];
        l.process(&mut frames, 2);
        let g = db_to_lin(-6.0);
        assert!((frames[0] - g).abs() < 1e-6 && (frames[1] - 0.1 * g).abs() < 1e-6);
    }

    #[test]
    fn empty_chain_passes_stereo_through() {
        let mut chain = SourceChain::new(&[], false);
        assert!(chain.is_empty());
        let mut out = Vec::new();
        chain.process(&[0.4, 0.2, 0.6, -0.6], true, &mut out);
        assert_eq!(out, [0.4, 0.2, 0.6, -0.6]);
        assert_eq!(chain.delay_frames(), 0);
    }

    #[test]
    fn mono_chain_averages_and_duplicates() {
        let mut chain = SourceChain::new(&[on(FilterKind::Gain { db: -6 })], true);
        let mut out = Vec::new();
        chain.process(&[0.4, 0.2, 0.6, 0.6], true, &mut out);
        let g = db_to_lin(-6.0);
        assert_eq!(out.len(), 4);
        assert!((out[0] - 0.3 * g).abs() < 1e-6 && out[0] == out[1]);
        assert!((out[2] - 0.6 * g).abs() < 1e-6 && out[2] == out[3]);
    }

    #[test]
    fn disabled_filters_are_skipped_and_order_is_kept() {
        let off = AudioFilter {
            enabled: false,
            kind: FilterKind::Gain { db: -20 },
        };
        let mut chain = SourceChain::new(&[off], false);
        assert!(chain.is_empty());
        let mut out = Vec::new();
        chain.process(&[0.5, 0.5], true, &mut out);
        assert_eq!(out, [0.5, 0.5]);

        // Gain then limiter limits the boosted signal; limiter then gain boosts past the ceiling.
        let boost = on(FilterKind::Gain { db: 12 });
        let limit = on(FilterKind::Limiter {
            ceiling_db: -1,
            release_ms: 100,
        });
        let run = |filters: &[AudioFilter]| {
            let mut out = Vec::new();
            let input: Vec<f32> = tone(0.5, 4800).iter().flat_map(|&s| [s, s]).collect();
            SourceChain::new(filters, false).process(&input, true, &mut out);
            peak(&out)
        };
        assert!(run(&[boost.clone(), limit.clone()]) <= db_to_lin(-1.0) + 1e-6);
        assert!(run(&[limit, boost]) > 1.5);
    }

    #[test]
    fn ptt_follows_the_key_with_a_release_delay() {
        let mut p = Ptt::new(200);
        assert!(!p.update(false, 0), "never pressed: closed");
        assert!(p.update(true, 100));
        assert!(p.update(false, 150), "just released: still open");
        assert!(p.update(false, 299));
        assert!(!p.update(false, 300), "release delay passed");
        assert!(p.update(true, 400));
        assert!(!p.update(false, 700));
    }

    #[test]
    fn ptt_key_names() {
        assert_eq!(ptt_vk("").unwrap(), None);
        assert_eq!(ptt_vk("V").unwrap(), Some(b'V' as i32));
        assert_eq!(ptt_vk("Ctrl+F5").unwrap(), Some(0x74));
        assert_eq!(ptt_vk("Mouse4").unwrap(), Some(5));
        assert_eq!(ptt_vk("mouse5").unwrap(), Some(6));
        assert_eq!(ptt_vk("MouseMiddle").unwrap(), Some(4));
        assert!(ptt_vk("Banana").is_err());
    }

    #[test]
    fn ptt_with_no_delay_closes_at_once() {
        let mut p = Ptt::new(0);
        assert!(p.update(true, 10));
        assert!(!p.update(false, 11));
    }

    #[test]
    fn closed_ptt_fades_to_silence_over_10_ms() {
        let mut chain = SourceChain::new(&[], true);
        let input = vec![0.5f32; 2 * 960];
        let mut out = Vec::new();
        chain.process(&input, false, &mut out);
        assert!(out[0] > 0.0 && out[0] < 0.5, "starts fading, not a click");
        assert_eq!(out[2 * 600], 0.0, "silent after 10 ms");
        out.clear();
        chain.process(&input, true, &mut out);
        assert!((out[2 * 600] - 0.5).abs() < 1e-6, "back after the fade-in");
    }

    #[test]
    fn suppressor_keeps_length_with_a_fixed_delay() {
        for mono in [true, false] {
            let mut chain = SourceChain::new(&[on(defaults("noise_suppression"))], mono);
            assert_eq!(chain.delay_frames(), 960);
            let mut out = Vec::new();
            for n in [100usize, 480, 777, 1] {
                let input: Vec<f32> = tone(0.3, n).iter().flat_map(|&s| [s, s]).collect();
                let before = out.len();
                chain.process(&input, true, &mut out);
                assert_eq!(out.len() - before, n * 2);
            }
            // The first 480 frames are the FIFO's head start: silence.
            assert!(out[..480 * 2].iter().all(|&v| v == 0.0));
        }
    }

    fn hiss(n: usize) -> Vec<f32> {
        let mut seed = 12345u32;
        (0..n)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                ((seed >> 16) as f32 / 32768.0 - 1.0) * 0.02
            })
            .collect()
    }

    #[test]
    fn suppressor_reduces_steady_hiss_by_strength() {
        // Low-level pseudo-random hiss (the mono average already lowers it a bit): RNNoise must reduce it,
        // and less at lower strength.
        let input = hiss(48_000 * 2);
        let tail_rms = |strength| {
            let mut chain = SourceChain::new(&[on(FilterKind::NoiseSuppression { strength })], true);
            let mut out = Vec::new();
            chain.process(&input, true, &mut out);
            rms(&out[out.len() - 2 * 4800..])
        };
        let full = tail_rms(100);
        let half = tail_rms(50);
        let none = tail_rms(0);
        assert!(full < rms(&input) * 0.8, "{full} vs {}", rms(&input));
        assert!(full < half && half < none, "{full} < {half} < {none}");
    }

    #[test]
    fn suppressor_delay_matches_what_it_reports() {
        // A tone that starts at frame 4000 (not a frame boundary) must come out exactly delay_frames later,
        // at full strength and blended, or the source drifts out of sync with the video.
        for strength in [100, 40] {
            let mut chain = SourceChain::new(&[on(FilterKind::NoiseSuppression { strength })], true);
            let t = tone(0.3, 24_000);
            let input: Vec<f32> = (0..24_000)
                .map(|i| if i >= 4000 { t[i] } else { 0.0 })
                .flat_map(|s| [s, s])
                .collect();
            let mut out = Vec::new();
            for packet in input.chunks(2 * 441) {
                chain.process(packet, true, &mut out);
            }
            let first = out.iter().position(|v| v.abs() > 0.02).unwrap() / 2;
            let want = 4000 + chain.delay_frames();
            assert!(
                first.abs_diff(want) < 40,
                "strength {strength}: onset at {first}, want ~{want}"
            );
        }
    }

    #[test]
    fn flush_returns_the_held_tail_then_starts_fresh() {
        let mut chain = SourceChain::new(&[on(defaults("noise_suppression"))], false);
        let t = tone(0.3, 4800);
        let input: Vec<f32> = t.iter().flat_map(|&s| [s, s]).collect();
        let mut out = Vec::new();
        chain.process(&input, true, &mut out);
        let mut tail = Vec::new();
        chain.flush(true, &mut tail);
        assert_eq!(tail.len(), 960 * 2, "exactly the delay");
        assert!(rms(&tail) > 0.05, "the end of the tone, not silence");
        // After the flush the chain starts from silence again: no old sound comes out.
        let mut next = Vec::new();
        chain.process(&vec![0.0; 960 * 2], true, &mut next);
        assert!(peak(&next) < 1e-3, "{}", peak(&next));
    }

    #[test]
    fn zero_strength_suppression_is_skipped() {
        let chain = SourceChain::new(&[on(FilterKind::NoiseSuppression { strength: 0 })], false);
        assert!(chain.is_empty());
        assert_eq!(chain.delay_frames(), 0);
    }

    #[test]
    fn every_kind_builds() {
        for name in ["noise_suppression", "noise_gate", "compressor", "limiter", "gain"] {
            let chain = SourceChain::new(&[on(defaults(name))], false);
            assert!(!chain.is_empty(), "{name}");
        }
    }
}
