//! FLV tag bodies, which RTMP carries as its audio and video messages: H.264 in AVCC form (length-prefixed NAL
//! units, the parameter sets in a sequence header) and raw AAC frames.

/// NAL unit types the stream handles specially.
const NAL_SPS: u8 = 7;
const NAL_PPS: u8 = 8;
const NAL_AUD: u8 = 9;

/// The NAL units of an Annex B byte stream (split at `00 00 01` / `00 00 00 01` start codes), without start
/// codes and trailing zero bytes. Data without a start code is taken as one NAL unit.
pub fn nal_units(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    if starts.is_empty() {
        return if data.is_empty() { Vec::new() } else { vec![data] };
    }
    let mut nals = Vec::new();
    for (n, &start) in starts.iter().enumerate() {
        let end = starts.get(n + 1).map_or(data.len(), |&next| next - 3);
        let mut nal = &data[start..end.max(start)];
        while let [rest @ .., 0] = nal {
            nal = rest;
        }
        if !nal.is_empty() {
            nals.push(nal);
        }
    }
    nals
}

fn nal_type(nal: &[u8]) -> u8 {
    nal.first().map_or(0, |b| b & 0x1F)
}

/// The H.264 parameter sets a decoder needs before the first picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AvcConfig {
    pub sps: Vec<u8>,
    pub pps: Vec<u8>,
}

impl AvcConfig {
    /// The SPS and PPS among `nals`, if both are there.
    pub fn find(nals: &[&[u8]]) -> Option<Self> {
        let sps = nals.iter().find(|n| nal_type(n) == NAL_SPS && n.len() >= 4)?;
        let pps = nals.iter().find(|n| nal_type(n) == NAL_PPS)?;
        Some(Self {
            sps: sps.to_vec(),
            pps: pps.to_vec(),
        })
    }

    /// The video tag that carries them (an AVCDecoderConfigurationRecord).
    pub fn sequence_header(&self) -> Vec<u8> {
        let mut out = vec![0x17, 0, 0, 0, 0];
        // Version, then profile, compatibility and level straight from the SPS.
        out.extend([1, self.sps[1], self.sps[2], self.sps[3]]);
        // Four-byte NAL lengths; one SPS; one PPS.
        out.extend([0xFF, 0xE1]);
        out.extend((self.sps.len() as u16).to_be_bytes());
        out.extend(&self.sps);
        out.push(1);
        out.extend((self.pps.len() as u16).to_be_bytes());
        out.extend(&self.pps);
        out
    }
}

/// The video tag of one encoded picture. Parameter sets and access unit delimiters stay out (the sequence
/// header has them). No B-frames, so the composition time is 0.
pub fn avc_frame(keyframe: bool, nals: &[&[u8]]) -> Vec<u8> {
    let mut out = vec![if keyframe { 0x17 } else { 0x27 }, 1, 0, 0, 0];
    for nal in nals {
        if matches!(nal_type(nal), NAL_SPS | NAL_PPS | NAL_AUD) {
            continue;
        }
        out.extend((nal.len() as u32).to_be_bytes());
        out.extend(*nal);
    }
    out
}

/// AAC, "44 kHz", 16-bit, stereo: the fixed flags byte FLV uses for every AAC stream (the real rate is in
/// the AudioSpecificConfig).
const AAC_FLAGS: u8 = 0xAF;

/// The AudioSpecificConfig of AAC-LC at `rate` Hz with `channels` channels.
pub fn aac_lc_config(rate: u32, channels: u8) -> [u8; 2] {
    const RATES: [u32; 13] = [
        96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
    ];
    let index = RATES.iter().position(|&r| r == rate).unwrap_or(3) as u16;
    let bits = (2u16 << 11) | (index << 7) | (u16::from(channels & 0xF) << 3);
    bits.to_be_bytes()
}

pub fn aac_sequence_header(config: &[u8]) -> Vec<u8> {
    let mut out = vec![AAC_FLAGS, 0];
    out.extend(config);
    out
}

pub fn aac_frame(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 2);
    out.extend([AAC_FLAGS, 1]);
    out.extend(data);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPS: [u8; 6] = [0x67, 0x64, 0x00, 0x2A, 0xAC, 0x2B];
    const PPS: [u8; 4] = [0x68, 0xEE, 0x3C, 0x80];

    fn annex_b() -> Vec<u8> {
        let mut v = vec![0, 0, 0, 1, 0x09, 0xF0, 0, 0, 0, 1];
        v.extend(SPS);
        v.extend([0, 0, 1]);
        v.extend(PPS);
        v.extend([0, 0, 0, 1, 0x65, 0x88, 0x84, 0x00]);
        v
    }

    #[test]
    fn annex_b_splits_into_nal_units() {
        let data = annex_b();
        let nals = nal_units(&data);
        assert_eq!(nals.len(), 4);
        assert_eq!(nals[0], [0x09, 0xF0]);
        assert_eq!(nals[1], SPS);
        assert_eq!(nals[2], PPS);
        // A trailing zero belongs to the start code, not the picture.
        assert_eq!(nals[3], [0x65, 0x88, 0x84]);
        assert_eq!(nal_units(&[0x41, 0x9A]), vec![&[0x41, 0x9A][..]]);
    }

    #[test]
    fn keyframes_carry_a_sequence_header() {
        let data = annex_b();
        let nals = nal_units(&data);
        let config = AvcConfig::find(&nals).unwrap();
        let header = config.sequence_header();
        assert_eq!(&header[..9], &[0x17, 0, 0, 0, 0, 1, 0x64, 0x00, 0x2A]);
        assert_eq!(&header[9..13], &[0xFF, 0xE1, 0, 6]);
        let frame = avc_frame(true, &nals);
        // Only the picture: its length, then its bytes.
        assert_eq!(frame, [0x17, 1, 0, 0, 0, 0, 0, 0, 3, 0x65, 0x88, 0x84]);
        assert_eq!(avc_frame(false, &[&[0x41]])[0], 0x27);
    }

    #[test]
    fn aac_config_for_48k_stereo() {
        assert_eq!(aac_lc_config(48000, 2), [0x11, 0x90]);
        assert_eq!(aac_lc_config(44100, 2), [0x12, 0x10]);
        assert_eq!(aac_sequence_header(&[0x11, 0x90]), [0xAF, 0, 0x11, 0x90]);
        assert_eq!(aac_frame(&[7]), [0xAF, 1, 7]);
    }
}
