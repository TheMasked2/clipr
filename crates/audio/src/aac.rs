//! The Media Foundation AAC encoder MFT: 16-bit 48 kHz stereo PCM in, raw AAC-LC frames out.

use std::mem::ManuallyDrop;

use anyhow::{Context, Result};
use windows::Win32::Media::MediaFoundation::{
    AACMFTEncoder, IMFMediaType, IMFSample, IMFTransform, MF_E_TRANSFORM_NEED_MORE_INPUT,
    MF_E_TRANSFORM_STREAM_CHANGE, MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, MF_MT_AAC_PAYLOAD_TYPE,
    MF_MT_AUDIO_AVG_BYTES_PER_SECOND, MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_BLOCK_ALIGNMENT,
    MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE,
    MF_MT_USER_DATA, MFAudioFormat_AAC, MFAudioFormat_PCM, MFCreateMediaType, MFCreateMemoryBuffer,
    MFCreateSample, MFGetAttributesAsBlob, MFGetAttributesAsBlobSize, MFInitAttributesFromBlob,
    MFMediaType_Audio, MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
    MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};

use crate::AudioPacket;
use crate::mix::{HNS_PER_SEC, RATE, time_at};

/// PCM samples per AAC frame.
const FRAME_SAMPLES: i64 = 1024;

/// The AAC encoder's allowed average data rates, in bytes per second (96/128/160/192 kbps).
const ALLOWED_BYTES_PER_SEC: [u32; 4] = [12_000, 16_000, 20_000, 24_000];

/// The encoder's data rate closest to `kbps`.
pub fn bytes_per_sec(kbps: u32) -> u32 {
    let wanted = kbps.saturating_mul(125);
    *ALLOWED_BYTES_PER_SEC
        .iter()
        .min_by_key(|b| b.abs_diff(wanted))
        .expect("non-empty")
}

pub struct AacEncoder {
    transform: IMFTransform,
    track: usize,
    start_100ns: i64,
    packets: i64,
}

// SAFETY: the encoder is created and used on the audio thread only; the MFT is a plain in-process object.
unsafe impl Send for AacEncoder {}

impl AacEncoder {
    /// A running encoder whose first packet is stamped `start_100ns`.
    pub fn new(track: usize, bitrate_kbps: u32, start_100ns: i64) -> Result<Self> {
        // SAFETY: plain COM creation of the in-box encoder.
        let transform: IMFTransform = unsafe { CoCreateInstance(&AACMFTEncoder, None, CLSCTX_INPROC_SERVER) }
            .context("this PC has no AAC encoder (Media Foundation is missing or disabled)")?;
        let avg = bytes_per_sec(bitrate_kbps);

        // SAFETY: valid media types and transform; the output type goes first.
        unsafe {
            let output = audio_type(&MFAudioFormat_AAC)?;
            output.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, avg)?;
            output.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0)?;
            output.SetUINT32(&MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, 0x29)?;
            transform
                .SetOutputType(0, &output, 0)
                .context("the AAC encoder doesn't accept 48 kHz stereo")?;

            let input = audio_type(&MFAudioFormat_PCM)?;
            input.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 4)?;
            input.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, RATE * 4)?;
            transform
                .SetInputType(0, &input, 0)
                .context("the AAC encoder doesn't accept 16-bit PCM")?;

            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        Ok(Self {
            transform,
            track,
            start_100ns,
            packets: 0,
        })
    }

    /// The output type serialised with `MFGetAttributesAsBlob` (it carries `MF_MT_USER_DATA`).
    pub fn media_type_blob(&self) -> Result<Vec<u8>> {
        // SAFETY: valid transform; the blob buffer is sized by the matching Size call.
        unsafe {
            let t = self.transform.GetOutputCurrentType(0)?;
            if t.GetBlobSize(&MF_MT_USER_DATA).unwrap_or(0) == 0 {
                // HEAACWAVEINFO (raw payload, AAC-LC profile/level 0x29) + AudioSpecificConfig for
                // AAC-LC, 48 kHz, stereo.
                let user_data = [
                    0u8, 0, 0x29, 0, 0, 0, 0, 0, 0, 0, 0, 0, // HEAACWAVEINFO tail
                    0x11, 0x90, // AudioSpecificConfig
                ];
                t.SetBlob(&MF_MT_USER_DATA, &user_data)?;
            }
            let size = MFGetAttributesAsBlobSize(&t)?;
            let mut blob = vec![0u8; size as usize];
            MFGetAttributesAsBlob(&t, &mut blob)?;
            Ok(blob)
        }
    }

    /// Encodes `pcm` (interleaved 16-bit stereo) that starts at timeline time `time_100ns`.
    pub fn encode(&mut self, pcm: &[i16], time_100ns: i64, out: &mut dyn FnMut(AudioPacket)) -> Result<()> {
        let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        let frames = (pcm.len() / 2) as i64;
        // SAFETY: plain allocations; the buffer is locked only while copying into it.
        let sample = unsafe {
            let buffer = MFCreateMemoryBuffer(bytes.len() as u32)?;
            let mut data = std::ptr::null_mut();
            buffer.Lock(&mut data, None, None)?;
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(bytes.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(time_100ns)?;
            sample.SetSampleDuration(frames * HNS_PER_SEC / RATE as i64)?;
            sample
        };
        // SAFETY: valid transform and sample.
        unsafe { self.transform.ProcessInput(0, &sample, 0) }.context("the AAC encoder rejected audio")?;
        self.collect(out)
    }

    /// Flushes the encoder: every remaining packet goes to `out`.
    pub fn drain(&mut self, out: &mut dyn FnMut(AudioPacket)) -> Result<()> {
        // SAFETY: valid transform.
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)?;
            self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)?;
        }
        self.collect(out)
    }

    /// Takes every finished packet out of the encoder.
    fn collect(&mut self, out: &mut dyn FnMut(AudioPacket)) -> Result<()> {
        loop {
            // SAFETY: valid transform, stream 0.
            let info = unsafe { self.transform.GetOutputStreamInfo(0) }?;
            let sample = if info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0 {
                None
            } else {
                // SAFETY: plain allocations.
                unsafe {
                    let sample = MFCreateSample()?;
                    sample.AddBuffer(&MFCreateMemoryBuffer(info.cbSize.max(8192))?)?;
                    Some(sample)
                }
            };
            let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(sample),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            // SAFETY: valid transform; `buffers` follows the MFT_OUTPUT_DATA_BUFFER contract.
            let result = unsafe { self.transform.ProcessOutput(0, &mut buffers, &mut status) };
            // SAFETY: take ownership of whatever the MFT left in the ManuallyDrop fields, exactly once.
            let (sample, _events) = unsafe {
                (
                    ManuallyDrop::take(&mut buffers[0].pSample),
                    ManuallyDrop::take(&mut buffers[0].pEvents),
                )
            };
            match result {
                Ok(()) => {
                    if let Some(sample) = sample {
                        out(self.packet(&sample)?);
                    }
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    // SAFETY: valid transform, stream 0.
                    unsafe {
                        let t = self.transform.GetOutputAvailableType(0, 0)?;
                        self.transform.SetOutputType(0, &t, 0)?;
                    }
                }
                Err(e) => return Err(e).context("the AAC encoder failed"),
            }
        }
    }

    /// Copies a finished sample out. Packets are stamped from their position in the stream (1024 samples
    /// each) so they are exactly contiguous on the shared clock.
    fn packet(&mut self, sample: &IMFSample) -> Result<AudioPacket> {
        // SAFETY: valid sample; the buffer is locked only while copying out of it.
        let data = unsafe {
            let buffer = sample.ConvertToContiguousBuffer()?;
            let (mut ptr, mut len) = (std::ptr::null_mut(), 0u32);
            buffer.Lock(&mut ptr, None, Some(&mut len))?;
            let data = std::slice::from_raw_parts(ptr, len as usize).to_vec();
            buffer.Unlock()?;
            data
        };
        let pts = time_at(self.start_100ns, self.packets * FRAME_SAMPLES);
        self.packets += 1;
        let next = time_at(self.start_100ns, self.packets * FRAME_SAMPLES);
        Ok(AudioPacket {
            track: self.track,
            pts_100ns: pts,
            duration_100ns: next - pts,
            data,
        })
    }
}

/// Rebuilds a media type from [`AacEncoder::media_type_blob`].
pub fn type_from_blob(blob: &[u8]) -> Result<IMFMediaType> {
    // SAFETY: plain creation; the blob came from MFGetAttributesAsBlob.
    unsafe {
        let t = MFCreateMediaType()?;
        MFInitAttributesFromBlob(&t, blob)?;
        Ok(t)
    }
}

/// An audio type at 48 kHz, 16 bits, stereo.
fn audio_type(subtype: &windows::core::GUID) -> Result<IMFMediaType> {
    // SAFETY: plain creation and setters with valid GUID constants.
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, RATE)?;
        t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
        Ok(t)
    }
}

#[cfg(test)]
mod tests {
    use super::bytes_per_sec;

    #[test]
    fn bitrate_snaps_to_an_allowed_rate() {
        assert_eq!(bytes_per_sec(96), 12_000);
        assert_eq!(bytes_per_sec(128), 16_000);
        assert_eq!(bytes_per_sec(192), 24_000);
        assert_eq!(bytes_per_sec(150), 20_000);
        assert_eq!(bytes_per_sec(64), 12_000);
        assert_eq!(bytes_per_sec(320), 24_000);
    }
}
