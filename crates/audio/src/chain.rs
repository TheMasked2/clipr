//! The microphone chain: noise suppression (RNNoise) → gate → compressor → push-to-talk → volume.
//!
//! All pure sample processing at 48 kHz, so it is unit tested without a device. The chain works on mono
//! (the microphone's channels are averaged) and hands back stereo with both channels equal.

use std::collections::VecDeque;

use crate::mix::RATE;

/// RNNoise's frame size: 10 ms at 48 kHz.
const NS_FRAME: usize = nnnoiseless::DenoiseState::FRAME_SIZE;

const SR: f32 = RATE as f32;

fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// What the chain does, from the user's settings.
#[derive(Clone, Debug)]
pub struct ChainSettings {
    pub suppress: bool,
    /// Gate threshold in dBFS, or `None` for no gate.
    pub gate_db: Option<f32>,
    pub compressor: bool,
    /// Linear gain applied last.
    pub volume: f32,
}

/// RNNoise behind a FIFO: it works on frames of 480 samples, so the output lags by exactly one frame
/// (10 ms), and always has as many samples as the input.
struct Suppressor {
    state: Box<nnnoiseless::DenoiseState<'static>>,
    input: Vec<f32>,
    output: VecDeque<f32>,
}

impl Suppressor {
    fn new() -> Self {
        Self {
            state: nnnoiseless::DenoiseState::new(),
            input: Vec::new(),
            // The constant one-frame delay: the output never runs dry.
            output: std::iter::repeat_n(0.0, NS_FRAME).collect(),
        }
    }

    fn process(&mut self, samples: &mut [f32]) {
        // RNNoise expects 16-bit scale.
        self.input.extend(samples.iter().map(|s| s * 32768.0));
        let mut frame = [0.0f32; NS_FRAME];
        while self.input.len() >= NS_FRAME {
            self.state.process_frame(&mut frame, &self.input[..NS_FRAME]);
            self.output.extend(frame.iter().map(|s| s / 32768.0));
            self.input.drain(..NS_FRAME);
        }
        for s in samples {
            *s = self.output.pop_front().unwrap_or(0.0);
        }
    }
}

/// A noise gate: opens above the threshold (10 ms attack), stays open 150 ms after the signal drops
/// below it, then closes over 100 ms.
pub struct Gate {
    threshold: f32,
    /// Fast-attack, slow-release peak follower.
    env: f32,
    hold_left: u32,
    gain: f32,
}

impl Gate {
    const HOLD: u32 = RATE * 150 / 1000;
    const ATTACK_STEP: f32 = 1.0 / (SR * 0.010);
    const RELEASE_STEP: f32 = 1.0 / (SR * 0.100);
    /// Per-sample decay of the follower (~40 ms).
    const DECAY: f32 = 0.9995;

    pub fn new(threshold_db: f32) -> Self {
        Self {
            threshold: db_to_lin(threshold_db),
            env: 0.0,
            hold_left: 0,
            gain: 0.0,
        }
    }

    pub fn process(&mut self, samples: &mut [f32]) {
        for s in samples {
            self.env = s.abs().max(self.env * Self::DECAY);
            if self.env >= self.threshold {
                self.hold_left = Self::HOLD;
            } else {
                self.hold_left = self.hold_left.saturating_sub(1);
            }
            self.gain = if self.hold_left > 0 {
                (self.gain + Self::ATTACK_STEP).min(1.0)
            } else {
                (self.gain - Self::RELEASE_STEP).max(0.0)
            };
            *s *= self.gain;
        }
    }
}

/// A feed-forward compressor: -20 dBFS threshold, 3:1 ratio, +4 dB makeup gain.
pub struct Compressor {
    env_db: f32,
    attack: f32,
    release: f32,
}

impl Compressor {
    const THRESHOLD_DB: f32 = -20.0;
    const RATIO: f32 = 3.0;
    const MAKEUP_DB: f32 = 4.0;

    pub fn new() -> Self {
        let coef = |seconds: f32| (-1.0 / (seconds * SR)).exp();
        Self {
            env_db: -120.0,
            attack: coef(0.005),
            release: coef(0.080),
        }
    }

    pub fn process(&mut self, samples: &mut [f32]) {
        for s in samples {
            let level = 20.0 * (s.abs() + 1e-9).log10();
            let coef = if level > self.env_db {
                self.attack
            } else {
                self.release
            };
            self.env_db = coef * self.env_db + (1.0 - coef) * level;
            let over = self.env_db - Self::THRESHOLD_DB;
            let reduction = if over > 0.0 {
                -over * (1.0 - 1.0 / Self::RATIO)
            } else {
                0.0
            };
            *s *= db_to_lin(reduction + Self::MAKEUP_DB);
        }
    }
}

impl Default for Compressor {
    fn default() -> Self {
        Self::new()
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

pub struct MicChain {
    suppressor: Option<Suppressor>,
    gate: Option<Gate>,
    compressor: Option<Compressor>,
    volume: f32,
    /// Push-to-talk fade (0 = muted, 1 = open), moved 10 ms per full swing.
    ptt_gain: f32,
    mono: Vec<f32>,
}

impl MicChain {
    pub fn new(settings: &ChainSettings) -> Self {
        Self {
            suppressor: settings.suppress.then(Suppressor::new),
            gate: settings.gate_db.map(Gate::new),
            compressor: settings.compressor.then(Compressor::new),
            volume: settings.volume,
            ptt_gain: 1.0,
            mono: Vec::new(),
        }
    }

    /// How many frames later than its input the output is (noise suppression works in 10 ms frames).
    pub fn delay_frames(&self) -> usize {
        if self.suppressor.is_some() { NS_FRAME } else { 0 }
    }

    /// Runs interleaved stereo `input` through the chain, appending the same number of stereo frames to
    /// `out`. `open` is the push-to-talk state (true when there is no push-to-talk).
    pub fn process(&mut self, input: &[f32], open: bool, out: &mut Vec<f32>) {
        self.mono.clear();
        self.mono
            .extend(input.chunks_exact(2).map(|f| (f[0] + f[1]) * 0.5));
        if let Some(s) = &mut self.suppressor {
            s.process(&mut self.mono);
        }
        if let Some(g) = &mut self.gate {
            g.process(&mut self.mono);
        }
        if let Some(c) = &mut self.compressor {
            c.process(&mut self.mono);
        }
        let target = if open { 1.0 } else { 0.0 };
        let step = 1.0 / (SR * 0.010);
        out.reserve(self.mono.len() * 2);
        for &s in &self.mono {
            self.ptt_gain = if self.ptt_gain < target {
                (self.ptt_gain + step).min(target)
            } else {
                (self.ptt_gain - step).max(target)
            };
            let v = s * self.ptt_gain * self.volume;
            out.push(v);
            out.push(v);
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

    #[test]
    fn gate_opens_for_loud_and_closes_after_hold_and_release() {
        let mut g = Gate::new(-45.0);
        let mut quiet = vec![0.001f32; 4800]; // -60 dBFS
        g.process(&mut quiet);
        assert!(quiet.iter().all(|&v| v == 0.0), "closed at the start");

        let mut loud = vec![0.5f32; 2400];
        g.process(&mut loud);
        assert!(loud[0] < 0.1, "attack ramps");
        assert!(loud[1000] > 0.49, "open after the 10 ms attack");

        // 50 ms later the follower and hold keep it open.
        let mut tail = vec![0.001f32; 2400];
        g.process(&mut tail);
        assert!(tail[2399] > 0.0009);
        // Well past follower decay + 150 ms hold + 100 ms release: closed.
        let mut later = vec![0.001f32; 48_000];
        g.process(&mut later);
        assert!(later[47_999] == 0.0);
    }

    #[test]
    fn compressor_squeezes_loud_and_lifts_quiet() {
        let mut c = Compressor::new();
        let mut loud = vec![0.5f32; 24_000]; // -6 dBFS
        c.process(&mut loud);
        // 14 dB over: -9.33 dB reduction + 4 dB makeup = -5.33 dB.
        let want = 0.5 * db_to_lin(-14.0 * 2.0 / 3.0 + 4.0);
        assert!((loud[23_999] - want).abs() < 0.01, "{} vs {want}", loud[23_999]);

        let mut c = Compressor::new();
        let mut quiet = vec![0.01f32; 24_000]; // -40 dBFS: below threshold, makeup only
        c.process(&mut quiet);
        assert!((quiet[23_999] - 0.01 * db_to_lin(4.0)).abs() < 1e-4);
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
    fn plain_chain_applies_volume_and_goes_stereo() {
        let mut chain = MicChain::new(&ChainSettings {
            suppress: false,
            gate_db: None,
            compressor: false,
            volume: 0.5,
        });
        let mut out = Vec::new();
        chain.process(&[0.4, 0.2, 0.6, 0.6], true, &mut out);
        // Mono average, then volume; both channels equal.
        assert_eq!(out.len(), 4);
        assert!((out[0] - 0.15).abs() < 1e-6 && out[0] == out[1]);
        assert!((out[2] - 0.3).abs() < 1e-6 && out[2] == out[3]);
        assert_eq!(chain.delay_frames(), 0);
    }

    #[test]
    fn closed_ptt_fades_to_silence_over_10_ms() {
        let mut chain = MicChain::new(&ChainSettings {
            suppress: false,
            gate_db: None,
            compressor: false,
            volume: 1.0,
        });
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
        let mut chain = MicChain::new(&ChainSettings {
            suppress: true,
            gate_db: None,
            compressor: false,
            volume: 1.0,
        });
        assert_eq!(chain.delay_frames(), 480);
        let mut out = Vec::new();
        for n in [100usize, 480, 777, 1] {
            let input: Vec<f32> = tone(0.3, n).iter().flat_map(|&s| [s, s]).collect();
            let before = out.len();
            chain.process(&input, true, &mut out);
            assert_eq!(out.len() - before, n * 2);
        }
        // The first 480 frames are the delay line: silence.
        assert!(out[..480 * 2].iter().all(|&v| v == 0.0));
    }

    #[test]
    fn suppressor_reduces_steady_hiss() {
        let mut chain = MicChain::new(&ChainSettings {
            suppress: true,
            gate_db: None,
            compressor: false,
            volume: 1.0,
        });
        // Low-level pseudo-random hiss (the mono average already lowers it a bit): RNNoise must reduce it.
        let mut seed = 12345u32;
        let hiss: Vec<f32> = (0..48_000 * 2)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                ((seed >> 16) as f32 / 32768.0 - 1.0) * 0.02
            })
            .collect();
        let mut out = Vec::new();
        chain.process(&hiss, true, &mut out);
        let tail = &out[out.len() - 2 * 4800..];
        assert!(rms(tail) < rms(&hiss) * 0.8, "{} vs {}", rms(tail), rms(&hiss));
    }
}
