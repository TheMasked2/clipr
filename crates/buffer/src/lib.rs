//! The replay buffer: the last N seconds of encoded video and audio packets, kept in RAM.
//!
//! Video is trimmed a whole keyframe interval (GOP) at a time, so the oldest packet is always a keyframe and
//! any clip cut from the buffer starts with a picture a player can decode. Audio is trimmed to the video's
//! start. Packets share their bytes (`Arc`), so cutting a clip copies nothing.
//!
//! Timestamps are on [`clipr_core::clock`]'s clock (100 ns units), the same for video and audio.

use std::collections::VecDeque;
use std::sync::Arc;

/// 100 ns units per second.
pub const SECOND: i64 = 10_000_000;

/// One encoded packet (a video frame or an audio frame).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    pub pts: i64,
    pub duration: i64,
    /// Video only: a picture that decodes on its own (IDR).
    pub keyframe: bool,
    pub data: Arc<[u8]>,
}

impl Packet {
    pub fn end(&self) -> i64 {
        self.pts + self.duration
    }
}

/// The packets of one clip, in order. `video[0]` is a keyframe.
#[derive(Clone, Debug, Default)]
pub struct Cut {
    pub video: Vec<Packet>,
    pub audio: Vec<Packet>,
}

impl Cut {
    /// The time of the first video packet; the clip's zero point.
    pub fn start(&self) -> Option<i64> {
        self.video.first().map(|p| p.pts)
    }

    pub fn duration_secs(&self) -> f64 {
        match (self.video.first(), self.video.last()) {
            (Some(a), Some(b)) => (b.end() - a.pts) as f64 / SECOND as f64,
            _ => 0.0,
        }
    }
}

pub struct ReplayBuffer {
    video: VecDeque<Packet>,
    audio: VecDeque<Packet>,
    keep: i64,
    bytes: usize,
}

impl ReplayBuffer {
    /// A buffer that keeps at least `keep_secs` seconds (up to one keyframe interval more).
    pub fn new(keep_secs: f64) -> Self {
        Self {
            video: VecDeque::new(),
            audio: VecDeque::new(),
            keep: (keep_secs * SECOND as f64) as i64,
            bytes: 0,
        }
    }

    pub fn set_keep(&mut self, keep_secs: f64) {
        self.keep = (keep_secs * SECOND as f64) as i64;
        self.trim();
    }

    pub fn push_video(&mut self, packet: Packet) {
        // A buffer must start with a keyframe; anything before the first one can't be decoded.
        if self.video.is_empty() && !packet.keyframe {
            return;
        }
        self.bytes += packet.data.len();
        self.video.push_back(packet);
        self.trim();
    }

    pub fn push_audio(&mut self, packet: Packet) {
        self.bytes += packet.data.len();
        self.audio.push_back(packet);
        if self.video.is_empty() {
            // No video yet: keep only the window, so audio can't pile up.
            let newest = self.audio.back().map_or(0, Packet::end);
            while self.audio.front().is_some_and(|p| p.end() < newest - self.keep) {
                self.pop_audio();
            }
        }
    }

    pub fn clear(&mut self) {
        self.video.clear();
        self.audio.clear();
        self.bytes = 0;
    }

    /// Memory held by packet data.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Seconds of video held.
    pub fn video_secs(&self) -> f64 {
        match (self.video.front(), self.video.back()) {
            (Some(a), Some(b)) => (b.end() - a.pts) as f64 / SECOND as f64,
            _ => 0.0,
        }
    }

    /// The last `secs` seconds: video from the keyframe at or before `newest - secs` (or the oldest one, if
    /// the buffer holds less), and the audio that overlaps it.
    pub fn cut(&self, secs: f64) -> Cut {
        let Some(newest) = self.video.back().map(Packet::end) else {
            return Cut::default();
        };
        let from = newest - (secs * SECOND as f64) as i64;
        // The last keyframe at or before `from`; the first packet when there's none.
        let start = self
            .video
            .iter()
            .enumerate()
            .filter(|(_, p)| p.keyframe && p.pts <= from)
            .map(|(i, _)| i)
            .next_back()
            .unwrap_or(0);
        let video: Vec<Packet> = self.video.range(start..).cloned().collect();
        let t0 = video[0].pts;
        let audio = self
            .audio
            .iter()
            .filter(|p| p.end() > t0 && p.pts < newest)
            .cloned()
            .collect();
        Cut { video, audio }
    }

    /// Drops whole keyframe intervals from the front while the rest still covers `keep`.
    fn trim(&mut self) {
        let Some(newest) = self.video.back().map(Packet::end) else {
            return;
        };
        let cutoff = newest - self.keep;
        // Find the second keyframe; dropping everything before it leaves the buffer starting at a keyframe.
        while let Some(next) = self.video.iter().skip(1).position(|p| p.keyframe).map(|i| i + 1) {
            if self.video[next].pts > cutoff {
                break;
            }
            for _ in 0..next {
                if let Some(p) = self.video.pop_front() {
                    self.bytes -= p.data.len();
                }
            }
        }
        let first = self.video.front().map_or(i64::MAX, |p| p.pts);
        while self.audio.front().is_some_and(|p| p.end() <= first) {
            self.pop_audio();
        }
    }

    fn pop_audio(&mut self) {
        if let Some(p) = self.audio.pop_front() {
            self.bytes -= p.data.len();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: i64 = SECOND / 60;

    fn video(i: i64) -> Packet {
        Packet {
            pts: i * FRAME,
            duration: FRAME,
            keyframe: i % 60 == 0,
            data: vec![0u8; 100].into(),
        }
    }

    fn audio(i: i64) -> Packet {
        let d = SECOND * 1024 / 48_000;
        Packet {
            pts: i * d,
            duration: d,
            keyframe: true,
            data: vec![0u8; 10].into(),
        }
    }

    fn fill(buffer: &mut ReplayBuffer, secs: i64) {
        let frames = secs * 60;
        let mut a = 0;
        for i in 0..frames {
            buffer.push_video(video(i));
            while audio(a).pts < (i + 1) * FRAME {
                buffer.push_audio(audio(a));
                a += 1;
            }
        }
    }

    #[test]
    fn keeps_the_window_and_starts_on_a_keyframe() {
        let mut b = ReplayBuffer::new(10.0);
        fill(&mut b, 30);
        assert!(
            b.video_secs() >= 10.0 && b.video_secs() <= 11.0,
            "{}",
            b.video_secs()
        );
        assert!(b.video[0].keyframe);
        assert!(b.audio[0].end() > b.video[0].pts);
        let expected = b.video.len() * 100 + b.audio.len() * 10;
        assert_eq!(b.bytes(), expected);
    }

    #[test]
    fn cut_starts_at_keyframe_before_the_request() {
        let mut b = ReplayBuffer::new(20.0);
        fill(&mut b, 30);
        let cut = b.cut(5.5);
        assert!(cut.video[0].keyframe);
        // 30 s total, asked for 5.5 s: the keyframe at 24 s.
        assert_eq!(cut.start(), Some(video(24 * 60).pts));
        assert!((cut.duration_secs() - 6.0).abs() < 0.02);
        assert!(cut.audio.first().unwrap().pts <= video(24 * 60).pts);
        assert!(cut.audio.last().unwrap().end() >= 30 * SECOND - 3 * FRAME);
    }

    #[test]
    fn cut_longer_than_buffer_gives_everything() {
        let mut b = ReplayBuffer::new(60.0);
        fill(&mut b, 5);
        let cut = b.cut(30.0);
        assert_eq!(cut.video.len(), 300);
        assert_eq!(cut.start(), Some(0));
    }

    #[test]
    fn packets_before_the_first_keyframe_are_dropped() {
        let mut b = ReplayBuffer::new(10.0);
        b.push_video(video(1));
        b.push_video(video(2));
        assert_eq!(b.video_secs(), 0.0);
        b.push_video(video(60));
        assert_eq!(b.video.len(), 1);
    }

    #[test]
    fn audio_alone_stays_bounded() {
        let mut b = ReplayBuffer::new(2.0);
        for i in 0..1000 {
            b.push_audio(audio(i));
        }
        assert!(b.audio.len() < 100, "{}", b.audio.len());
    }

    #[test]
    fn empty_cut_and_clear() {
        let mut b = ReplayBuffer::new(5.0);
        assert!(b.cut(5.0).video.is_empty());
        fill(&mut b, 3);
        b.clear();
        assert_eq!(b.bytes(), 0);
        assert!(b.cut(5.0).video.is_empty());
    }
}
