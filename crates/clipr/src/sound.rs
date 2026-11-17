//! clipr's own short sounds (clip saved, saving failed, mic muted or back on), made here instead of Windows'
//! system beeps, at the user's volume (`replay.sound_volume`).
//!
//! They play through the default output device, so with system audio on they can be heard in a clip saved
//! right after.

/// Which sound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cue {
    /// A clip or recording was saved: two bright rising notes.
    Saved,
    /// Saving failed: two low falling notes.
    Failed,
    /// The microphone was muted: a short falling blip.
    MicMuted,
    /// The microphone is back on: a short rising blip.
    MicOn,
}

const RATE: u32 = 44_100;

impl Cue {
    /// The notes: frequency in Hz and length in ms, played one after the other.
    fn notes(self) -> &'static [(f32, u32)] {
        match self {
            Cue::Saved => &[(1046.5, 70), (1568.0, 160)],
            Cue::Failed => &[(392.0, 110), (261.6, 200)],
            Cue::MicMuted => &[(740.0, 55), (494.0, 90)],
            Cue::MicOn => &[(494.0, 55), (740.0, 90)],
        }
    }
}

/// Plays `cue` at `volume` % (0 = nothing) without waiting for it. A newer sound cuts an older one off.
pub fn play(cue: Cue, volume: u32) {
    if volume == 0 {
        return;
    }
    let wav = wav(&samples(cue, volume));
    let spawned = std::thread::Builder::new()
        .name("clipr-sound".into())
        .spawn(move || play_wav(&wav));
    if let Err(e) = spawned {
        tracing::warn!("can't play a sound: {e}");
    }
}

#[cfg(windows)]
fn play_wav(wav: &[u8]) {
    use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_MEMORY, SND_NODEFAULT, SND_SYNC};
    // SAFETY: with SND_MEMORY the first argument points at a whole WAV file in memory; SND_SYNC returns only
    // after it played (or was cut off), so `wav` outlives the call.
    unsafe {
        PlaySoundW(
            wav.as_ptr().cast(),
            std::ptr::null_mut(),
            SND_MEMORY | SND_SYNC | SND_NODEFAULT,
        )
    };
}

#[cfg(not(windows))]
fn play_wav(_wav: &[u8]) {}

/// The cue as 16-bit mono samples: each note a sine with a soft octave on top, a 4 ms fade-in so it doesn't
/// click, and a quick decay like a struck bell.
fn samples(cue: Cue, volume: u32) -> Vec<i16> {
    // Loudness follows the square of the slider, which sounds more even than a straight line.
    let v = volume.min(100) as f32 / 100.0;
    let peak = 0.5 * v * v * f32::from(i16::MAX);
    let mut out = Vec::new();
    for &(freq, ms) in cue.notes() {
        let n = (RATE * ms / 1000) as usize;
        let attack = (RATE as f32 * 0.004) as usize;
        let tau = n as f32 * 0.35;
        for i in 0..n {
            let t = i as f32 / RATE as f32;
            let w = std::f32::consts::TAU * freq * t;
            let tone = w.sin() * 0.8 + (2.0 * w).sin() * 0.2;
            let env = (i as f32 / attack as f32).min(1.0) * (-(i as f32) / tau).exp();
            out.push((tone * env * peak) as i16);
        }
    }
    out
}

/// A complete WAV file (PCM, 16-bit, mono) holding `samples`.
fn wav(samples: &[i16]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut w = Vec::with_capacity(44 + data_len as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&1u16.to_le_bytes()); // mono
    w.extend_from_slice(&RATE.to_le_bytes());
    w.extend_from_slice(&(RATE * 2).to_le_bytes()); // bytes per second
    w.extend_from_slice(&2u16.to_le_bytes()); // block align
    w.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        w.extend_from_slice(&s.to_le_bytes());
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peak(cue: Cue, volume: u32) -> i32 {
        samples(cue, volume)
            .iter()
            .map(|s| i32::from(*s).abs())
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn the_wav_header_matches_the_samples() {
        let s = samples(Cue::Saved, 60);
        assert_eq!(s.len(), (RATE * 230 / 1000) as usize);
        let w = wav(&s);
        assert_eq!(&w[..4], b"RIFF");
        assert_eq!(&w[8..16], b"WAVEfmt ");
        assert_eq!(
            u32::from_le_bytes(w[4..8].try_into().unwrap()) as usize,
            w.len() - 8
        );
        assert_eq!(
            u32::from_le_bytes(w[40..44].try_into().unwrap()) as usize,
            s.len() * 2
        );
    }

    #[test]
    fn volume_scales_and_never_clips() {
        assert_eq!(peak(Cue::Saved, 0), 0);
        assert!(peak(Cue::Saved, 30) < peak(Cue::Saved, 60));
        assert!(peak(Cue::Saved, 100) <= i32::from(i16::MAX) / 2 + 1);
        // Above 100 % is treated as 100 %.
        assert_eq!(peak(Cue::Failed, 250), peak(Cue::Failed, 100));
    }

    #[test]
    fn notes_start_without_a_click() {
        for cue in [Cue::Saved, Cue::Failed, Cue::MicMuted, Cue::MicOn] {
            assert_eq!(samples(cue, 100)[0], 0);
        }
    }
}
