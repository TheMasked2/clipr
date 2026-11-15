//! Hardware probe: the GPUs in the machine and the video encoders Media Foundation offers for each codec.
//!
//! Used by the auto preset and for diagnostics. Nothing here needs a live capture session.

use std::ffi::c_void;
use std::ptr;

use anyhow::{Context, Result};
use clipr_gpu::Vendor;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1};
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, MF_VERSION, MFMediaType_Video, MFSTARTUP_FULL, MFShutdown, MFStartup,
    MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG, MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER,
    MFT_ENUM_FLAG_SYNCMFT, MFT_ENUM_HARDWARE_VENDOR_ID_Attribute, MFT_FRIENDLY_NAME_Attribute,
    MFT_REGISTER_TYPE_INFO, MFTEnumEx, MFVideoFormat_AV1, MFVideoFormat_H264, MFVideoFormat_HEVC,
    MFVideoFormat_NV12,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows_core::{GUID, PWSTR};

use crate::Codec;

/// One DXGI adapter, hardware or software (WARP).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterInfo {
    pub name: String,
    pub vendor: Vendor,
    /// PCI vendor id, e.g. `0x10DE` for NVIDIA.
    pub vendor_id: u32,
    /// Dedicated video memory in bytes.
    pub dedicated_memory: u64,
    pub software: bool,
}

/// Every DXGI adapter in the machine, in enumeration order.
pub fn list_adapters() -> Result<Vec<AdapterInfo>> {
    // SAFETY: plain factory creation, no pointer arguments.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.context("can't create a DXGI factory")?;
    let mut out = Vec::new();
    let mut index = 0;
    // SAFETY: EnumAdapters1 fails (DXGI_ERROR_NOT_FOUND) once past the last adapter, which ends the loop.
    while let Ok(adapter) = unsafe { factory.EnumAdapters1(index) } {
        index += 1;
        // SAFETY: valid adapter interface; GetDesc1 only writes to the returned struct.
        let desc = unsafe { adapter.GetDesc1() }
            .with_context(|| format!("can't read the description of adapter {index}"))?;
        let name_len = desc
            .Description
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(desc.Description.len());
        out.push(AdapterInfo {
            name: String::from_utf16_lossy(&desc.Description[..name_len]),
            vendor: Vendor::from_pci_id(desc.VendorId),
            vendor_id: desc.VendorId,
            dedicated_memory: desc.DedicatedVideoMemory as u64,
            software: (desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0,
        });
    }
    Ok(out)
}

/// One video encoder MFT that accepts NV12 input and produces `codec`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncoderInfo {
    pub codec: Codec,
    pub name: String,
    pub hardware: bool,
    /// The hardware vendor id attribute, e.g. `"VEN_1002"`. `None` for software encoders, or when the
    /// attribute is absent.
    pub vendor_id: Option<String>,
}

const CODECS: [Codec; 3] = [Codec::H264, Codec::Hevc, Codec::Av1];

fn output_subtype(codec: Codec) -> GUID {
    match codec {
        Codec::H264 => MFVideoFormat_H264,
        Codec::Hevc => MFVideoFormat_HEVC,
        Codec::Av1 => MFVideoFormat_AV1,
    }
}

/// Every video encoder MFT for each codec: hardware first, then software, grouped by codec.
pub fn list_encoders() -> Result<Vec<EncoderInfo>> {
    // SAFETY: MFStartup takes no pointers. It is balanced by the MFShutdown below, which runs only if this
    // call succeeds.
    unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.context("MFStartup failed")?;
    let result = enumerate_encoders();
    // SAFETY: balances the successful MFStartup above.
    if let Err(e) = unsafe { MFShutdown() } {
        tracing::warn!("MFShutdown failed: {e}");
    }
    result
}

fn enumerate_encoders() -> Result<Vec<EncoderInfo>> {
    let passes = [
        (MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER, true),
        (MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER, false),
    ];
    let mut out = Vec::new();
    for codec in CODECS {
        for (flags, hardware) in passes {
            for activate in
                enum_mfts(codec, flags).with_context(|| format!("can't enumerate {codec:?} encoders"))?
            {
                let name = get_string(&activate, &MFT_FRIENDLY_NAME_Attribute)
                    .with_context(|| format!("can't read the name of a {codec:?} encoder"))?;
                let vendor_id = if hardware {
                    get_string(&activate, &MFT_ENUM_HARDWARE_VENDOR_ID_Attribute).ok()
                } else {
                    None
                };
                out.push(EncoderInfo {
                    codec,
                    name,
                    hardware,
                    vendor_id,
                });
            }
        }
    }
    Ok(out)
}

/// The activation objects for MFTs matching `codec` with NV12 input, filtered by `flags`.
fn enum_mfts(codec: Codec, flags: MFT_ENUM_FLAG) -> Result<Vec<IMFActivate>> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: output_subtype(codec),
    };
    let mut array: *mut Option<IMFActivate> = ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: both type infos outlive the call. On success MF allocates `array` with `count` entries, which
    // are taken over below.
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            flags,
            Some(&input as *const _),
            Some(&output as *const _),
            &mut array,
            &mut count,
        )
    }
    .context("MFTEnumEx failed")?;

    let mut found = Vec::with_capacity(count as usize);
    if !array.is_null() {
        for i in 0..count as usize {
            // SAFETY: MF initialised `count` elements at `array`. Each element is read exactly once and the
            // value moves into `found`, so Drop releases it. Nothing reads the slot afterwards.
            let item = unsafe { ptr::read(array.add(i)) };
            if let Some(activate) = item {
                found.push(activate);
            }
        }
        // SAFETY: the array came from CoTaskMemAlloc inside MFTEnumEx, and its elements were moved out above.
        unsafe { CoTaskMemFree(Some(array as *const c_void)) };
    }
    Ok(found)
}

/// Reads a string attribute of an activation object. The buffer is allocated by Media Foundation and freed here.
fn get_string(activate: &IMFActivate, key: &GUID) -> Result<String> {
    let mut value = PWSTR(ptr::null_mut());
    let mut len = 0u32;
    // SAFETY: `key` is a valid GUID reference. On success `value` points to `len` UTF-16 units allocated with
    // CoTaskMemAlloc.
    unsafe { activate.GetAllocatedString(key, &mut value, &mut len) }.context("GetAllocatedString failed")?;
    // SAFETY: on success `value` points to `len` valid UTF-16 code units, and it is not freed yet.
    let text = unsafe { String::from_utf16_lossy(std::slice::from_raw_parts(value.0, len as usize)) };
    // SAFETY: `value` was allocated by GetAllocatedString with CoTaskMemAlloc and is not used afterwards.
    unsafe { CoTaskMemFree(Some(value.0 as *const c_void)) };
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_codec_maps_to_a_distinct_output_subtype() {
        let subtypes = CODECS.map(output_subtype);
        assert_ne!(subtypes[0], subtypes[1]);
        assert_ne!(subtypes[0], subtypes[2]);
        assert_ne!(subtypes[1], subtypes[2]);
    }

    #[test]
    fn adapter_enumeration_does_not_error() {
        list_adapters().expect("list_adapters failed");
    }

    #[test]
    fn encoder_enumeration_does_not_error() {
        list_encoders().expect("list_encoders failed");
    }
}
