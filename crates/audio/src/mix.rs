//! The pure parts of the audio pipeline, with no Windows API in sight so they can be unit tested.
//!
//! - Sample format conversion to interleaved stereo `f32`.
//! - A linear-interpolation resampler for devices that don't run at 48 kHz.
//! - [`Timeline`]: a continuous 48 kHz timeline that sources place their samples on by QPC time. Gaps
//!   become silence, drift is absorbed, and mixed output is cut as 16-bit PCM for the AAC encoder.

/// The output sample rate, in Hz.
pub const RATE: u32 = 48_000;

/// 100 ns units per second (the unit of `clipr_core::clock::now_100ns`).
pub const HNS_PER_SEC: i64 = 10_000_000;

/// A source whose data lands more than this many frames (20 ms) from where its QPC says it should be is
/// moved to the QPC position instead of being appended seamlessly.
const RESYNC_FRAMES: i64 = (RATE as i64) / 50;

/// The timeline frame (rounded) that a time on the shared clock falls on, given the timeline start.
pub fn frame_at(start_100ns: i64, time_100ns: i64) -> i64 {
    let hns = time_100ns as i128 - start_100ns as i128;
    ((hns * RATE as i128 + HNS_PER_SEC as i128 / 2).div_euclid(HNS_PER_SEC as i128)) as i64
}

/// The time on the shared clock (rounded down) of a timeline frame.
pub fn time_at(start_100ns: i64, frame: i64) -> i64 {
    start_100ns + (frame as i128 * HNS_PER_SEC as i128).div_euclid(RATE as i128) as i64
}

/// How the samples of a device buffer are laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleKind {
    F32,
    I16,
}

impl SampleKind {
    /// Bytes per sample (one channel of one frame).
    pub fn width(self) -> usize {
        match self {
            Self::F32 => 4,
            Self::I16 => 2,
        }
    }
}

/// Converts interleaved device frames to interleaved stereo `f32`, appended to `out`: mono goes to both
/// channels, more than two channels keep the front left and right (the first two).
pub fn to_stereo(data: &[u8], kind: SampleKind, channels: usize, out: &mut Vec<f32>) {
    let channels = channels.max(1);
    let width = kind.width();
    let sample = |bytes: &[u8]| -> f32 {
        match kind {
            SampleKind::F32 => f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            SampleKind::I16 => i16::from_le_bytes([bytes[0], bytes[1]]) as f32 / 32768.0,
        }
    };
    out.reserve(data.len() / (channels * width) * 2);
    for frame in data.chunks_exact(channels * width) {
        let left = sample(&frame[..width]);
        let right = if channels > 1 {
            sample(&frame[width..2 * width])
        } else {
            left
        };
        out.push(left);
        out.push(right);
    }
}

/// Linear-interpolation resampler for interleaved stereo, keeping its position across calls so packet
/// boundaries don't click.
pub struct Resampler {
    /// Input frames consumed per output frame.
    step: f64,
    /// Position of the next output frame, relative to `prev` (index 0) and the next input (index 1..).
    pos: f64,
    prev: Option<[f32; 2]>,
}

impl Resampler {
    /// Forgets the previous input, after a gap in it.
    pub fn reset(&mut self) {
        self.pos = 0.0;
        self.prev = None;
    }

    pub fn new(from_rate: u32, to_rate: u32) -> Self {
        Self {
            step: from_rate as f64 / to_rate as f64,
            pos: 0.0,
            prev: None,
        }
    }

    /// Resamples `input` (interleaved stereo), appending to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let frames = input.len() / 2;
        if frames == 0 {
            return;
        }
        let prev = *self.prev.get_or_insert([input[0], input[1]]);
        // Frame i of the extended stream: 0 is `prev`, 1..=frames are the input.
        let at = |i: usize, ch: usize| -> f32 { if i == 0 { prev[ch] } else { input[(i - 1) * 2 + ch] } };
        while self.pos < frames as f64 {
            let i = self.pos as usize;
            let frac = (self.pos - i as f64) as f32;
            for ch in 0..2 {
                let a = at(i, ch);
                let b = at(i + 1, ch);
                out.push(a + (b - a) * frac);
            }
            self.pos += self.step;
        }
        self.pos -= frames as f64;
        self.prev = Some([input[frames * 2 - 2], input[frames * 2 - 1]]);
    }
}

/// One source's samples waiting to be mixed.
struct Lane {
    gain: f32,
    /// Interleaved stereo for timeline frames `emitted..`.
    buf: Vec<f32>,
    /// Where the next pushed sample belongs if the source stays continuous.
    expected: Option<i64>,
}

/// The shared 48 kHz timeline: sources push samples with their QPC time, [`Timeline::pop_until`] hands out
/// the mix.
pub struct Timeline {
    start_100ns: i64,
    /// Frames already handed out.
    emitted: i64,
    lanes: Vec<Lane>,
    /// For each output track, the lanes mixed into it.
    tracks: Vec<Vec<usize>>,
}

impl Timeline {
    /// A timeline starting at `start_100ns` with one lane per entry of `gains`, and output tracks that
    /// each mix the lanes listed.
    pub fn new(start_100ns: i64, gains: &[f32], tracks: Vec<Vec<usize>>) -> Self {
        Self {
            start_100ns,
            emitted: 0,
            lanes: gains
                .iter()
                .map(|&gain| Lane {
                    gain,
                    buf: Vec::new(),
                    expected: None,
                })
                .collect(),
            tracks,
        }
    }

    /// A timeline with a single track mixing every lane.
    #[cfg(test)]
    fn single(start_100ns: i64, gains: &[f32]) -> Self {
        Self::new(start_100ns, gains, vec![(0..gains.len()).collect()])
    }

    /// Frames handed out so far.
    pub fn emitted_frames(&self) -> i64 {
        self.emitted
    }

    /// Adds interleaved stereo `samples` whose first frame was captured at `qpc_100ns`.
    ///
    /// The samples go right after the previous ones unless the source's own clock has drifted more than
    /// 20 ms from QPC or `discontinuity` is set, then they go where QPC says. Frames that would land
    /// before what was already handed out are dropped; gaps become silence.
    pub fn push(&mut self, lane: usize, qpc_100ns: i64, samples: &[f32], discontinuity: bool) {
        let frames = (samples.len() / 2) as i64;
        if frames == 0 {
            return;
        }
        let target = frame_at(self.start_100ns, qpc_100ns);
        let emitted = self.emitted;
        let lane = &mut self.lanes[lane];
        let pos = match lane.expected {
            Some(e) if !discontinuity && (target - e).abs() <= RESYNC_FRAMES => e,
            _ => target,
        };
        lane.expected = Some(pos + frames);

        let end = emitted + (lane.buf.len() / 2) as i64;
        if pos > end {
            lane.buf.resize(lane.buf.len() + (pos - end) as usize * 2, 0.0);
        }
        let skip = (end - pos).clamp(0, frames) as usize;
        lane.buf.extend_from_slice(&samples[skip * 2..]);
    }

    /// Mixes and removes everything up to `until_100ns`, appending each track's 16-bit interleaved stereo to
    /// the matching entry of `out`. Returns the number of frames added to each (0 when `until_100ns` isn't
    /// past the last output).
    pub fn pop_until(&mut self, until_100ns: i64, out: &mut [Vec<i16>]) -> usize {
        let frames = frame_at(self.start_100ns, until_100ns) - self.emitted;
        if frames <= 0 {
            return 0;
        }
        let frames = frames as usize;
        for (track, out) in self.tracks.iter().zip(out.iter_mut()) {
            let mut mix = vec![0.0f32; frames * 2];
            for &lane in track {
                let lane = &self.lanes[lane];
                let take = (lane.buf.len() / 2).min(frames);
                for (m, s) in mix.iter_mut().zip(&lane.buf[..take * 2]) {
                    *m += s * lane.gain;
                }
            }
            out.extend(mix.iter().map(|&m| (m.clamp(-1.0, 1.0) * 32767.0).round() as i16));
        }
        for lane in &mut self.lanes {
            let take = (lane.buf.len() / 2).min(frames);
            lane.buf.drain(..take * 2);
        }
        self.emitted += frames as i64;
        frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 100 ns per frame at 48 kHz is 208.33; use whole milliseconds in tests.
    const MS: i64 = 10_000;

    fn pop(t: &mut Timeline, until: i64, out: &mut Vec<i16>) -> usize {
        let mut tracks = vec![Vec::new()];
        let n = t.pop_until(until, &mut tracks);
        out.extend(&tracks[0]);
        n
    }

    fn stereo(v: f32, frames: usize) -> Vec<f32> {
        vec![v; frames * 2]
    }

    #[test]
    fn frame_and_time_round_trip() {
        assert_eq!(frame_at(1_000, 1_000 + HNS_PER_SEC), 48_000);
        assert_eq!(frame_at(1_000, 1_000 - 10 * MS), -480);
        assert_eq!(time_at(1_000, 48_000), 1_000 + HNS_PER_SEC);
        assert_eq!(time_at(0, 1024), 213_333);
    }

    #[test]
    fn converts_float_stereo() {
        let mut bytes = Vec::new();
        for v in [0.5f32, -0.25, 1.0, 0.0] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        let mut out = Vec::new();
        to_stereo(&bytes, SampleKind::F32, 2, &mut out);
        assert_eq!(out, [0.5, -0.25, 1.0, 0.0]);
    }

    #[test]
    fn converts_i16_mono_to_both_channels() {
        let mut bytes = Vec::new();
        for v in [16384i16, -32768] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        let mut out = Vec::new();
        to_stereo(&bytes, SampleKind::I16, 1, &mut out);
        assert_eq!(out, [0.5, 0.5, -1.0, -1.0]);
    }

    #[test]
    fn surround_keeps_front_left_and_right() {
        let mut bytes = Vec::new();
        for v in [0.1f32, 0.2, 0.9, 0.9, 0.9, 0.9] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        let mut out = Vec::new();
        to_stereo(&bytes, SampleKind::F32, 6, &mut out);
        assert_eq!(out, [0.1, 0.2]);
    }

    #[test]
    fn ignores_a_trailing_partial_frame() {
        let mut out = Vec::new();
        to_stereo(&[0, 0, 0], SampleKind::I16, 2, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn resampling_44100_to_48000_scales_the_length() {
        let mut r = Resampler::new(44_100, 48_000);
        let input: Vec<f32> = (0..44_100).flat_map(|i| [i as f32, -(i as f32)]).collect();
        let mut out = Vec::new();
        // Feed in odd-sized chunks; the result must not depend on the chunking.
        for chunk in input.chunks(2 * 441) {
            r.process(chunk, &mut out);
        }
        let frames = out.len() / 2;
        assert!((47_990..=48_001).contains(&frames), "{frames}");
        // A ramp stays a ramp: monotonic, with the right slope (44100/48000 input frames per output frame).
        for w in out.chunks(2).collect::<Vec<_>>().windows(2) {
            assert!(w[1][0] >= w[0][0] && w[1][1] <= w[0][1]);
        }
        let slope = out[2 * 1000] - out[2 * 999];
        assert!((slope - 0.91875).abs() < 1e-3, "{slope}");
    }

    #[test]
    fn resampling_keeps_a_constant_constant() {
        let mut r = Resampler::new(96_000, 48_000);
        let mut out = Vec::new();
        r.process(&stereo(0.3, 960), &mut out);
        r.process(&stereo(0.3, 960), &mut out);
        assert_eq!(out.len() / 2, 960);
        assert!(out.iter().all(|&v| (v - 0.3).abs() < 1e-6));
    }

    #[test]
    fn silence_fills_the_gap_between_packets() {
        let mut t = Timeline::single(0, &[1.0]);
        t.push(0, 0, &stereo(0.5, 480), false); // 0..10 ms
        t.push(0, 40 * MS, &stereo(0.25, 480), false); // 40..50 ms
        let mut out = Vec::new();
        assert_eq!(pop(&mut t, 50 * MS, &mut out), 2400);
        assert_eq!(out[0], 16384);
        assert_eq!(out[479 * 2], 16384);
        assert_eq!(out[480 * 2], 0);
        assert_eq!(out[1919 * 2], 0);
        assert_eq!(out[1920 * 2], 8192);
        assert_eq!(out.len(), 2400 * 2);
    }

    #[test]
    fn empty_sources_still_produce_a_continuous_timeline() {
        let mut t = Timeline::single(5_000, &[1.0, 1.0]);
        let mut out = Vec::new();
        assert_eq!(pop(&mut t, 5_000 + 100 * MS, &mut out), 4800);
        assert_eq!(pop(&mut t, 5_000 + 100 * MS, &mut out), 0);
        assert_eq!(pop(&mut t, 5_000 + 110 * MS, &mut out), 480);
        assert_eq!(t.emitted_frames(), 5280);
        assert!(out.iter().all(|&s| s == 0));
    }

    #[test]
    fn mixes_sources_with_gain_and_clamps() {
        let mut t = Timeline::single(0, &[1.0, 2.0]);
        t.push(0, 0, &stereo(0.25, 480), false);
        t.push(1, 0, &stereo(0.125, 480), false);
        let mut out = Vec::new();
        pop(&mut t, 10 * MS, &mut out);
        assert_eq!(out[0], (0.5f32 * 32767.0).round() as i16);

        t.push(0, 10 * MS, &stereo(0.9, 480), false);
        t.push(1, 10 * MS, &stereo(0.9, 480), false);
        pop(&mut t, 20 * MS, &mut out);
        assert_eq!(out[480 * 2], 32767);
    }

    #[test]
    fn small_drift_is_absorbed_and_big_drift_resyncs() {
        let mut t = Timeline::single(0, &[1.0]);
        t.push(0, 0, &stereo(0.5, 480), false);
        // 5 ms late by QPC: still appended seamlessly after the first packet.
        t.push(0, 15 * MS, &stereo(0.5, 480), false);
        let mut out = Vec::new();
        pop(&mut t, 20 * MS, &mut out);
        assert!(out.iter().all(|&s| s == 16384));
        // 100 ms late by QPC: moved, with silence in between.
        t.push(0, 120 * MS, &stereo(0.5, 480), false);
        pop(&mut t, 130 * MS, &mut out);
        assert_eq!(out[960 * 2], 0);
        assert_eq!(*out.last().unwrap(), 16384);
        assert_eq!(out[(120 * 48) * 2], 16384);
        assert_eq!(out[(120 * 48 - 1) * 2], 0);
    }

    #[test]
    fn a_discontinuity_resyncs_even_when_close() {
        let mut t = Timeline::single(0, &[1.0]);
        t.push(0, 0, &stereo(0.5, 480), false);
        t.push(0, 15 * MS, &stereo(0.5, 480), true);
        let mut out = Vec::new();
        pop(&mut t, 25 * MS, &mut out);
        assert_eq!(out[(14 * 48) * 2], 0); // the 10..15 ms gap is silent
        assert_eq!(out[(15 * 48) * 2], 16384);
    }

    #[test]
    fn data_from_before_the_start_or_already_emitted_is_dropped() {
        let mut t = Timeline::single(100 * MS, &[1.0]);
        // Captured 5 ms before the timeline began: the first 240 frames fall off.
        t.push(0, 95 * MS, &stereo(0.5, 480), false);
        let mut out = Vec::new();
        pop(&mut t, 110 * MS, &mut out);
        assert_eq!(out.len(), 480 * 2);
        assert_eq!(out[239 * 2], 16384);
        assert_eq!(out[240 * 2], 0);

        // Late data overlapping what was already handed out is trimmed, not duplicated.
        t.push(0, 105 * MS, &stereo(0.5, 480), true);
        pop(&mut t, 120 * MS, &mut out);
        assert_eq!(out.len(), 960 * 2);
        assert_eq!(out[480 * 2], 16384); // frames 480..: the part of the packet past 110 ms
        assert_eq!(out[(480 + 239) * 2], 16384);
        assert_eq!(out[(480 + 240) * 2], 0);
    }

    #[test]
    fn tracks_mix_their_own_lanes() {
        // Lane 0 = system, lane 1 = mic, lane 2 = an app. Track 0 mixes system + mic, track 1 is the app.
        let mut t = Timeline::new(0, &[1.0, 1.0, 1.0], vec![vec![0, 1], vec![2], vec![1]]);
        t.push(0, 0, &stereo(0.25, 480), false);
        t.push(1, 0, &stereo(0.125, 480), false);
        t.push(2, 0, &stereo(0.5, 480), false);
        let mut out = vec![Vec::new(), Vec::new(), Vec::new()];
        assert_eq!(t.pop_until(10 * MS, &mut out), 480);
        assert_eq!(out[0][0], (0.375f32 * 32767.0).round() as i16);
        assert_eq!(out[1][0], 16384);
        assert_eq!(out[2][0], (0.125f32 * 32767.0).round() as i16);
        assert!(out.iter().all(|o| o.len() == 960));
        // Lanes were consumed once, not once per track.
        assert_eq!(t.pop_until(20 * MS, &mut out), 480);
        assert_eq!(out[0][960], 0);
    }
}
