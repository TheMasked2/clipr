//! Media Foundation hardware encoding.
//!
//! [`HwEncoder`] drives the GPU vendor's encoder MFT (AMD AMF, NVIDIA NVENC, Intel Quick Sync) directly:
//! - It finds an encoder that actually works with *our* GPU. PCs with an iGPU and a dGPU from the same
//!   vendor register one encoder per GPU, the adapter filter (`MFT_ENUM_ADAPTER_LUID`) isn't honoured by
//!   every driver, and an encoder fed textures from the other GPU silently stalls forever. So each
//!   candidate gets a one-frame self-test; the first that answers is used and remembered.
//! - It feeds NV12 textures straight from the GPU and hands back encoded packets, which the replay buffer
//!   (Phase 2) keeps in memory.
//! - Every wait has a deadline, so a misbehaving driver becomes an error instead of a hang.
//!
//! [`MfFileEncoder`] wraps it and writes the packets into an MP4 with a passthrough Sink Writer (no
//! re-encode). That's the Phase 1 spike path.

use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Media::MediaFoundation::{
    CODECAPI_AVEncCommonMeanBitRate, CODECAPI_AVEncCommonRateControlMode,
    CODECAPI_AVEncMPVDefaultBPictureCount, CODECAPI_AVEncMPVGOPSize, ICodecAPI, IMF2DBuffer, IMFActivate,
    IMFAttributes, IMFDXGIDeviceManager, IMFMediaEventGenerator, IMFMediaType, IMFSample, IMFSinkWriter,
    IMFTransform, MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS, METransformDrainComplete, METransformHaveOutput,
    METransformNeedInput, MF_ATTRIBUTE_STRING, MF_E_NO_EVENTS_AVAILABLE, MF_E_TRANSFORM_NEED_MORE_INPUT,
    MF_E_TRANSFORM_STREAM_CHANGE, MF_EVENT_FLAG_NO_WAIT, MF_LOW_LATENCY, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE,
    MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO,
    MF_MT_SUBTYPE, MF_MT_TRANSFER_FUNCTION, MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_VIDEO_PRIMARIES,
    MF_MT_YUV_MATRIX, MF_TRANSFORM_ASYNC_UNLOCK, MF_VERSION, MFCreateAttributes, MFCreateDXGIDeviceManager,
    MFCreateDXGISurfaceBuffer, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
    MFCreateSinkWriterFromURL, MFMediaType_Video, MFNominalRange_16_235, MFSTARTUP_FULL, MFShutdown,
    MFStartup, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_ADAPTER_LUID, MFT_ENUM_FLAG_HARDWARE,
    MFT_ENUM_FLAG_SORTANDFILTER, MFT_FRIENDLY_NAME_Attribute, MFT_MESSAGE_COMMAND_DRAIN,
    MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
    MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
    MFT_REGISTER_TYPE_INFO, MFTEnum2, MFVideoFormat_AV1, MFVideoFormat_H264, MFVideoFormat_HEVC,
    MFVideoFormat_NV12, MFVideoInterlace_Progressive, MFVideoPrimaries_BT709, MFVideoTransFunc_709,
    MFVideoTransferMatrix_BT709, eAVEncCommonRateControlMode_CBR, eAVEncH264VProfile_High,
    eAVEncH265VProfile_Main_420_8,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Variant::{VARENUM, VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_UI4};
use windows::core::{GUID, HSTRING, PWSTR};
use windows_core::Interface;

use crate::{Codec, EncodeSettings, VideoEncoder};
use clipr_gpu::{Gpu, nv12_texture};

/// How long the encoder may take to ask for the next frame, or to finish draining, before we give up.
const ENCODER_TIMEOUT: Duration = Duration::from_secs(3);

/// How long a candidate encoder gets to answer the one-frame self-test. Working encoders answer in
/// milliseconds.
const SELF_TEST_TIMEOUT: Duration = Duration::from_millis(500);

/// An adapter LUID (low, high), a codec, and the index of the candidate that passed the self-test.
type Choice = ((u32, i32), Codec, usize);

/// Which candidate passed the self-test, per adapter and codec, so later encoders skip the test.
static CHOSEN: Mutex<Vec<Choice>> = Mutex::new(Vec::new());

/// Keeps Media Foundation started while something uses it (MFStartup/MFShutdown are reference counted).
struct MfRuntime;

impl MfRuntime {
    fn start() -> Result<Self> {
        // SAFETY: plain API call; balanced by MFShutdown in Drop.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.context("can't start Media Foundation")?;
        Ok(Self)
    }
}

impl Drop for MfRuntime {
    fn drop(&mut self) {
        // SAFETY: balances the MFStartup in `start`.
        let _ = unsafe { MFShutdown() };
    }
}

/// One encoded packet: a sample with a system-memory buffer and its presentation time.
pub type Packet = IMFSample;

/// A hardware encoder MFT on our GPU, fed with NV12 textures, producing encoded packets.
pub struct HwEncoder {
    transform: IMFTransform,
    events: IMFMediaEventGenerator,
    /// NeedInput events received but not yet answered with a frame.
    need_input: u32,
    drained: bool,
    frames_in: u64,
    packets_out: u64,
    frame_duration: i64,
    name: String,
    _manager: IMFDXGIDeviceManager,
    // Declared last so Media Foundation shuts down after everything above is released.
    _mf: MfRuntime,
}

// SAFETY: hardware encoder MFTs are free-threaded (MF_TRANSFORM_ASYNC); the D3D device is multithread-protected.
unsafe impl Send for HwEncoder {}

impl HwEncoder {
    /// A working hardware encoder for `settings.codec` on `gpu`.
    pub fn new(gpu: &Gpu, settings: &EncodeSettings) -> Result<Self> {
        if settings.width % 2 != 0 || settings.height % 2 != 0 || settings.fps == 0 {
            bail!("invalid encode settings {settings:?}");
        }
        let _mf = MfRuntime::start()?;
        let candidates = candidates(gpu, settings.codec)?;
        let key = (gpu.luid.LowPart, gpu.luid.HighPart);
        let known = CHOSEN
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|(k, c, _)| *k == key && *c == settings.codec)
            .map(|(_, _, i)| *i);
        if let Some((activate, name)) = known.and_then(|i| candidates.get(i)) {
            return Self::open(activate, name, gpu, settings);
        }

        let mut last_err = None;
        for (i, (activate, name)) in candidates.iter().enumerate() {
            match Self::self_test(activate, name, gpu, settings) {
                Ok(()) => {
                    CHOSEN
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push((key, settings.codec, i));
                    tracing::info!("encoder: {name} (candidate {i} of {})", candidates.len());
                    return Self::open(activate, name, gpu, settings);
                }
                Err(e) => {
                    tracing::info!("skipping {name} (candidate {i}): {e:#}");
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("no candidates"))).with_context(|| {
            format!(
                "no {:?} hardware encoder works on {}",
                settings.codec, gpu.adapter_name
            )
        })
    }

    /// Encodes one blank frame on a throwaway instance and checks the encoder reacts.
    fn self_test(activate: &IMFActivate, name: &str, gpu: &Gpu, settings: &EncodeSettings) -> Result<()> {
        let result = (|| {
            let mut encoder = Self::open(activate, name, gpu, settings)?;
            let texture = nv12_texture(gpu, (settings.width, settings.height))?;
            encoder.feed(&texture, 0, SELF_TEST_TIMEOUT, &mut |_, _| Ok(()))?;
            let deadline = Instant::now() + SELF_TEST_TIMEOUT;
            while encoder.packets_out == 0 && encoder.need_input == 0 {
                if !encoder.next_event(&mut |_, _| Ok(()))? {
                    if Instant::now() > deadline {
                        bail!("no response to a test frame");
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            Ok(())
        })();
        // Shut the test instance down properly (a stalled one otherwise keeps driver resources and breaks
        // the next candidate) so the next ActivateObject creates a fresh one.
        // SAFETY: valid activate; our references to the instance were dropped above.
        let _ = unsafe { activate.ShutdownObject() };
        result
    }

    fn open(activate: &IMFActivate, name: &str, gpu: &Gpu, settings: &EncodeSettings) -> Result<Self> {
        let mf = MfRuntime::start()?;
        // SAFETY: valid activation object.
        let transform: IMFTransform = unsafe { activate.ActivateObject() }
            .with_context(|| format!("can't start the {name} encoder"))?;

        // SAFETY: valid transform; hardware MFTs are async and must be unlocked before use.
        unsafe {
            let attrs = transform.GetAttributes()?;
            attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
            attrs.SetUINT32(&MF_LOW_LATENCY, 1)?;
        }

        let mut token = 0u32;
        let mut manager = None;
        // SAFETY: valid out-pointers.
        unsafe { MFCreateDXGIDeviceManager(&mut token, &mut manager) }.context("no DXGI device manager")?;
        let manager: IMFDXGIDeviceManager = manager.context("no DXGI device manager")?;
        // SAFETY: valid device and the token from the call above; the MFT AddRefs the manager it's given.
        unsafe {
            manager
                .ResetDevice(&gpu.device, token)
                .context("can't hand the GPU to Media Foundation")?;
            transform
                .ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)
                .with_context(|| format!("{name} won't take frames from this GPU"))?;
        }

        // Rate control and GOP go in before the media types: AMD's H.264 encoder sizes frames for 30 fps
        // when they're changed after (and Microsoft's docs ask for this order too).
        let bitrate_bps = settings.bitrate_kbps.saturating_mul(1000);
        configure(&transform, settings, bitrate_bps);

        // Encoders want the output type first.
        let output = media_type(settings, codec_subtype(settings.codec))?;
        // SAFETY: valid media types and transform.
        unsafe {
            output.SetUINT32(&MF_MT_AVG_BITRATE, bitrate_bps)?;
            match settings.codec {
                Codec::H264 => output.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?,
                Codec::Hevc => {
                    output.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH265VProfile_Main_420_8.0 as u32)?
                }
                Codec::Av1 => {}
            }
            transform
                .SetOutputType(0, &output, 0)
                .with_context(|| format!("{name} doesn't support this output format"))?;
            let input = media_type(settings, &MFVideoFormat_NV12)?;
            transform.SetInputType(0, &input, 0).with_context(|| {
                format!(
                    "{name} doesn't accept NV12 at {}x{}",
                    settings.width, settings.height
                )
            })?;
        }

        // SAFETY: valid transform, fully configured.
        unsafe {
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        let events: IMFMediaEventGenerator = transform.cast().context("encoder isn't asynchronous")?;

        Ok(Self {
            transform,
            events,
            need_input: 0,
            drained: false,
            frames_in: 0,
            packets_out: 0,
            frame_duration: 10_000_000 / settings.fps as i64,
            name: name.to_owned(),
            _manager: manager,
            _mf: mf,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The encoder's current output type, including the codec headers once the first packet is out.
    pub fn output_type(&self) -> Result<IMFMediaType> {
        // SAFETY: valid transform, stream 0.
        Ok(unsafe { self.transform.GetOutputCurrentType(0) }?)
    }

    /// Encodes one NV12 texture. Finished packets go to `out` (possibly several, possibly none yet).
    pub fn encode(
        &mut self,
        nv12: &ID3D11Texture2D,
        time_100ns: i64,
        out: &mut dyn FnMut(&Self, Packet) -> Result<()>,
    ) -> Result<()> {
        self.feed(nv12, time_100ns, ENCODER_TIMEOUT, out)
    }

    fn feed(
        &mut self,
        nv12: &ID3D11Texture2D,
        time_100ns: i64,
        timeout: Duration,
        out: &mut dyn FnMut(&Self, Packet) -> Result<()>,
    ) -> Result<()> {
        let sample = texture_sample(nv12, time_100ns, self.frame_duration)?;
        let deadline = Instant::now() + timeout;
        while self.need_input == 0 {
            if !self.next_event(out)? {
                if Instant::now() > deadline {
                    bail!(
                        "{} stopped asking for frames ({} in, {} out)",
                        self.name,
                        self.frames_in,
                        self.packets_out
                    );
                }
                std::thread::sleep(Duration::from_micros(200));
            }
        }
        // SAFETY: valid transform and sample; the MFT asked for input (NeedInput).
        unsafe { self.transform.ProcessInput(0, &sample, 0) }
            .with_context(|| format!("{} rejected a frame", self.name))?;
        self.need_input -= 1;
        self.frames_in += 1;
        // Collect whatever is ready without waiting.
        while self.next_event(out)? {}
        Ok(())
    }

    /// Flushes the encoder: every remaining packet goes to `out`.
    pub fn drain(&mut self, out: &mut dyn FnMut(&Self, Packet) -> Result<()>) -> Result<()> {
        // SAFETY: valid transform.
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)?;
            self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)?;
        }
        let deadline = Instant::now() + ENCODER_TIMEOUT;
        while !self.drained {
            if !self.next_event(out)? {
                if Instant::now() > deadline {
                    bail!("{} didn't finish within {ENCODER_TIMEOUT:?}", self.name);
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        Ok(())
    }

    /// Handles one pending encoder event. `Ok(false)` when none was waiting.
    fn next_event(&mut self, out: &mut dyn FnMut(&Self, Packet) -> Result<()>) -> Result<bool> {
        let no_wait = MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(MF_EVENT_FLAG_NO_WAIT.0);
        // SAFETY: valid event generator; NO_WAIT returns MF_E_NO_EVENTS_AVAILABLE instead of blocking.
        let event = match unsafe { self.events.GetEvent(no_wait) } {
            Ok(event) => event,
            Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(false),
            Err(e) => return Err(e).with_context(|| format!("{} failed", self.name)),
        };
        // SAFETY: valid event.
        let (kind, status) = unsafe { (event.GetType()?, event.GetStatus()?) };
        status
            .ok()
            .with_context(|| format!("{} reported an error", self.name))?;
        match kind as i32 {
            k if k == METransformNeedInput.0 => self.need_input += 1,
            k if k == METransformHaveOutput.0 => self.take_output(out)?,
            k if k == METransformDrainComplete.0 => self.drained = true,
            _ => {}
        }
        Ok(true)
    }

    fn take_output(&mut self, out: &mut dyn FnMut(&Self, Packet) -> Result<()>) -> Result<()> {
        // SAFETY: valid transform, stream 0.
        let info = unsafe { self.transform.GetOutputStreamInfo(0) }?;
        let sample = if info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0 {
            None
        } else {
            // SAFETY: plain allocations.
            unsafe {
                let sample = MFCreateSample()?;
                sample.AddBuffer(&MFCreateMemoryBuffer(info.cbSize.max(1 << 20))?)?;
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
            Ok(()) => match sample {
                Some(sample) => {
                    self.packets_out += 1;
                    out(self, sample)
                }
                None => Ok(()),
            },
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                // The encoder changed its output format (e.g. now knows the codec headers): accept it.
                // SAFETY: valid transform, stream 0.
                unsafe {
                    let t = self.transform.GetOutputAvailableType(0, 0)?;
                    self.transform.SetOutputType(0, &t, 0)?;
                }
                Ok(())
            }
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(()),
            Err(e) => Err(e).with_context(|| format!("{} failed to produce a packet", self.name)),
        }
    }
}

/// Encodes into an MP4 file: [`HwEncoder`] plus a passthrough Sink Writer, created at the first packet
/// when the encoder's output type carries the codec headers the MP4 needs.
pub struct MfFileEncoder {
    encoder: HwEncoder,
    file: Mp4Writer,
    last_time: Option<i64>,
}

impl MfFileEncoder {
    pub fn create(gpu: &Gpu, path: &Path, settings: &EncodeSettings) -> Result<Self> {
        Ok(Self {
            encoder: HwEncoder::new(gpu, settings)?,
            file: Mp4Writer {
                path: path.to_owned(),
                writer: None,
            },
            last_time: None,
        })
    }

    pub fn encoder_name(&self) -> &str {
        self.encoder.name()
    }
}

impl VideoEncoder for MfFileEncoder {
    fn push(&mut self, nv12: &ID3D11Texture2D, time_100ns: i64) -> Result<()> {
        if self.last_time.is_some_and(|last| time_100ns <= last) {
            bail!(
                "timestamps must increase ({time_100ns} after {:?})",
                self.last_time
            );
        }
        self.last_time = Some(time_100ns);
        let file = &mut self.file;
        self.encoder.encode(nv12, time_100ns, &mut |encoder, packet| {
            file.write(encoder, &packet)
        })
    }

    fn finish(self: Box<Self>) -> Result<()> {
        let Self {
            mut encoder,
            mut file,
            ..
        } = *self;
        encoder.drain(&mut |encoder, packet| file.write(encoder, &packet))?;
        file.finish()
    }
}

/// An MP4 file written from encoded packets without re-encoding.
struct Mp4Writer {
    path: PathBuf,
    writer: Option<(IMFSinkWriter, u32)>,
}

impl Mp4Writer {
    fn write(&mut self, encoder: &HwEncoder, packet: &Packet) -> Result<()> {
        if self.writer.is_none() {
            let output = encoder.output_type()?;
            let url = HSTRING::from(self.path.as_os_str());
            // SAFETY: valid URL; the same type in and out makes a passthrough stream (no encoder inserted).
            let opened = unsafe {
                let w = MFCreateSinkWriterFromURL(&url, None, None::<&IMFAttributes>)
                    .with_context(|| format!("can't create {}", self.path.display()))?;
                let stream = w
                    .AddStream(&output)
                    .context("the MP4 writer doesn't accept this codec")?;
                w.SetInputMediaType(stream, &output, None)?;
                w.BeginWriting()?;
                (w, stream)
            };
            self.writer = Some(opened);
        }
        let (w, stream) = self.writer.as_ref().expect("set above");
        // SAFETY: valid writer, stream and sample.
        unsafe { w.WriteSample(*stream, packet) }.context("can't write to the video file")
    }

    fn finish(self) -> Result<()> {
        match self.writer {
            // SAFETY: valid writer; writes the MP4 index.
            Some((w, _)) => unsafe { w.Finalize() }.context("can't finish the video file"),
            None => bail!("the encoder produced no video"),
        }
    }
}

/// Hardware encoders for `codec` that can belong to `gpu`, best match first.
///
/// Drivers register one encoder per GPU, and the registrations name their device with a PCI path like
/// `PCI\VEN_1002&DEV_7550&...` (AMD does; the attribute key isn't documented, so we look for the path).
/// Ones naming our device come first, ones naming none next. Ones naming another GPU are left out: they
/// don't just fail with our textures, starting one can break the driver's other encoders for the rest of
/// the process.
fn candidates(gpu: &Gpu, codec: Codec) -> Result<Vec<(IMFActivate, String)>> {
    let mut luid = [0u8; 8];
    luid[..4].copy_from_slice(&gpu.luid.LowPart.to_le_bytes());
    luid[4..].copy_from_slice(&gpu.luid.HighPart.to_le_bytes());
    let mut list = enumerate(codec, Some(&luid))?;
    if list.is_empty() {
        list = enumerate(codec, None)?;
    }
    let ours = format!("VEN_{:04X}&DEV_{:04X}", gpu.vendor_id, gpu.device_id);
    let mut ranked: Vec<(u8, IMFActivate, String)> = list
        .into_iter()
        .filter_map(|a| {
            let rank = match device_path(&a) {
                Some(url) if url.to_ascii_uppercase().contains(&ours) => 0,
                Some(url) if url.to_ascii_uppercase().contains("DEV_") => {
                    tracing::debug!("skipping an encoder for another GPU ({url})");
                    return None;
                }
                _ => 1,
            };
            let name = string_attr(&a, &MFT_FRIENDLY_NAME_Attribute)
                .unwrap_or_else(|| format!("{codec:?} hardware encoder"));
            Some((rank, a, name))
        })
        .collect();
    if ranked.is_empty() {
        bail!("{} has no hardware {codec:?} encoder", gpu.adapter_name);
    }
    ranked.sort_by_key(|(rank, ..)| *rank);
    Ok(ranked.into_iter().map(|(_, a, name)| (a, name)).collect())
}

fn enumerate(codec: Codec, luid: Option<&[u8; 8]>) -> Result<Vec<IMFActivate>> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: *codec_subtype(codec),
    };
    let mut attrs: Option<IMFAttributes> = None;
    // SAFETY: valid out-pointer and blob.
    unsafe {
        MFCreateAttributes(&mut attrs, 1)?;
        if let Some(luid) = luid {
            attrs
                .as_ref()
                .context("no attributes")?
                .SetBlob(&MFT_ENUM_ADAPTER_LUID, luid)?;
        }
    }
    let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: valid type infos, attributes and out-pointers.
    unsafe {
        MFTEnum2(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input),
            Some(&output),
            attrs.as_ref(),
            &mut list,
            &mut count,
        )
    }
    .context("can't list hardware encoders")?;
    if list.is_null() {
        return Ok(Vec::new());
    }
    // SAFETY: MFTEnum2 returned `count` owned activation objects in a CoTaskMem array; read each one out
    // exactly once (so Rust releases them) and free the array.
    Ok(unsafe {
        let items = (0..count as usize)
            .filter_map(|i| std::ptr::read(list.add(i)))
            .collect();
        CoTaskMemFree(Some(list.cast()));
        items
    })
}

/// The PCI device path of the GPU an encoder registration belongs to, if it names one.
fn device_path(activate: &IMFActivate) -> Option<String> {
    // SAFETY: valid activate; index within GetCount; no value is fetched by GetItemByIndex.
    unsafe {
        (0..activate.GetCount().ok()?).find_map(|i| {
            let mut key = GUID::zeroed();
            activate.GetItemByIndex(i, &mut key, None).ok()?;
            if activate.GetItemType(&key).ok()? != MF_ATTRIBUTE_STRING {
                return None;
            }
            string_attr(activate, &key).filter(|s| s.to_ascii_uppercase().starts_with("PCI\\"))
        })
    }
}

fn string_attr(activate: &IMFActivate, key: &GUID) -> Option<String> {
    let mut text = PWSTR::null();
    let mut len = 0u32;
    // SAFETY: valid activate and out-pointers; the string is CoTaskMem-allocated and freed below.
    unsafe {
        activate.GetAllocatedString(key, &mut text, &mut len).ok()?;
        let name = text.to_string().ok();
        CoTaskMemFree(Some(text.0.cast()));
        name
    }
}

/// Wraps an NV12 texture in a sample without copying it.
fn texture_sample(nv12: &ID3D11Texture2D, time_100ns: i64, duration: i64) -> Result<IMFSample> {
    // SAFETY: the texture is a valid D3D11 resource on the device the encoder holds.
    let buffer = unsafe { MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, nv12, 0, false) }
        .context("can't wrap the frame for the encoder")?;
    // SAFETY: plain calls on valid objects. DXGI buffers report length 0 until told, and encoders reject
    // empty buffers.
    unsafe {
        let length = buffer.cast::<IMF2DBuffer>()?.GetContiguousLength()?;
        buffer.SetCurrentLength(length)?;
        let sample = MFCreateSample()?;
        sample.AddBuffer(&buffer)?;
        sample.SetSampleTime(time_100ns)?;
        sample.SetSampleDuration(duration)?;
        Ok(sample)
    }
}

fn codec_subtype(codec: Codec) -> &'static GUID {
    match codec {
        Codec::H264 => &MFVideoFormat_H264,
        Codec::Hevc => &MFVideoFormat_HEVC,
        Codec::Av1 => &MFVideoFormat_AV1,
    }
}

/// A progressive video type with size, rate and BT.709 limited-range colour tags.
fn media_type(settings: &EncodeSettings, subtype: &GUID) -> Result<IMFMediaType> {
    // SAFETY: plain creation and setters with valid GUID constants.
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack(settings.width, settings.height))?;
        t.SetUINT64(&MF_MT_FRAME_RATE, pack(settings.fps, 1))?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        t.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32)?;
        t.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32)?;
        t.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
        t.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
        Ok(t)
    }
}

/// MF packs size and ratio attributes as (high << 32) | low.
fn pack(high: u32, low: u32) -> u64 {
    ((high as u64) << 32) | low as u64
}

/// Best-effort tuning: CBR at the target bitrate, a short keyframe interval (so clips can start almost
/// anywhere) and no B-frames (no reordering). Drivers may ignore any of these; we log it.
///
/// Low latency comes from the `MF_LOW_LATENCY` attribute instead of `CODECAPI_AVLowLatencyMode`: set this
/// early, the codec API one makes AMD's H.264 encoder write a corrupt stream.
fn configure(transform: &IMFTransform, settings: &EncodeSettings, bitrate_bps: u32) {
    let Ok(codec) = transform.cast::<ICodecAPI>() else {
        tracing::warn!("encoder has no codec API, using its defaults");
        return;
    };
    let gop = ((settings.fps as f32 * settings.keyframe_interval_secs).round() as u32).max(1);
    let values: [(&GUID, VARIANT, &str); 4] = [
        (
            &CODECAPI_AVEncCommonRateControlMode,
            variant_u32(eAVEncCommonRateControlMode_CBR.0 as u32),
            "CBR",
        ),
        (
            &CODECAPI_AVEncCommonMeanBitRate,
            variant_u32(bitrate_bps),
            "bitrate",
        ),
        (&CODECAPI_AVEncMPVGOPSize, variant_u32(gop), "keyframe interval"),
        (
            &CODECAPI_AVEncMPVDefaultBPictureCount,
            variant_u32(0),
            "no B-frames",
        ),
    ];
    for (api, value, what) in values {
        // SAFETY: valid codec API, GUID and a fully initialised VARIANT.
        if let Err(e) = unsafe { codec.SetValue(api, &value) } {
            tracing::debug!("encoder ignored {what}: {e}");
        }
    }
}

/// A `VT_UI4` VARIANT. Plain data, so leaving it to `ManuallyDrop` leaks nothing.
fn variant_u32(value: u32) -> VARIANT {
    variant(VT_UI4, VARIANT_0_0_0 { ulVal: value })
}

fn variant(vt: VARENUM, data: VARIANT_0_0_0) -> VARIANT {
    VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                vt,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: data,
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_puts_first_value_high() {
        assert_eq!(pack(1920, 1080), (1920u64 << 32) | 1080);
        assert_eq!(pack(60, 1) >> 32, 60);
    }
}
