//! The D3D11 device every other crate shares, plus GPU-side frame conversion.
//!
//! Capture (WGC), colour conversion and Media Foundation encoding all run on one [`Gpu`], so a frame goes
//! from the screen to the encoder without ever being copied to system memory.
//!
//! On PCs with two GPUs (a dGPU plus the CPU's iGPU) the device must live on the adapter that drives the
//! game's monitor; [`Gpu::for_monitor`] picks it. Capturing from a monitor on another adapter works, but
//! Windows then copies every frame across PCIe.

#![cfg(windows)]

mod convert;
mod snapshot;

pub use convert::{Nv12Converter, ScaleMode, nv12_texture};
pub use snapshot::{Snapshot, Snapshotter};

use anyhow::{Context, Result};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Win32::Foundation::{HMODULE, LUID};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_DESC1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIAdapter1, IDXGIDevice,
    IDXGIFactory1,
};
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::WinRT::Direct3D11::CreateDirect3D11DeviceFromDXGIDevice;
use windows_core::Interface;

/// GPU vendor, from the PCI vendor id. Encoder quirks differ per vendor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vendor {
    Nvidia,
    Amd,
    Intel,
    Other(u32),
}

impl Vendor {
    pub fn from_pci_id(id: u32) -> Self {
        match id {
            0x10DE => Self::Nvidia,
            0x1002 | 0x1022 => Self::Amd,
            0x8086 => Self::Intel,
            other => Self::Other(other),
        }
    }
}

/// A D3D11 device with video support, safe to use from several threads.
#[derive(Clone)]
pub struct Gpu {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub adapter_name: String,
    pub vendor: Vendor,
    /// Dedicated video memory in bytes (0 for most iGPUs).
    pub dedicated_memory: u64,
    /// The adapter's locally unique id.
    pub luid: LUID,
    /// PCI vendor and device ids; encoder registrations name their GPU by these.
    pub vendor_id: u32,
    pub device_id: u32,
}

// SAFETY: the device is created multithread-protected (ID3D11Multithread), so D3D serialises access to the
// immediate context; ID3D11Device itself is free-threaded.
unsafe impl Send for Gpu {}
// SAFETY: as above.
unsafe impl Sync for Gpu {}

impl Gpu {
    /// The device on the adapter that drives `monitor`, or [`Gpu::best`] when no adapter claims it.
    pub fn for_monitor(monitor: HMONITOR) -> Result<Self> {
        for adapter in adapters()? {
            let mut index = 0;
            // SAFETY: plain COM calls on a valid adapter; EnumOutputs errors once past the last output.
            while let Ok(output) = unsafe { adapter.EnumOutputs(index) } {
                index += 1;
                // SAFETY: valid output.
                if let Ok(desc) = unsafe { output.GetDesc() }
                    && desc.Monitor == monitor
                {
                    return Self::on_adapter(&adapter);
                }
            }
        }
        tracing::warn!("no adapter drives that monitor; using the best GPU");
        Self::best()
    }

    /// The hardware adapter with the most dedicated video memory (the dGPU on hybrid laptops).
    pub fn best() -> Result<Self> {
        let adapter = adapters()?
            .into_iter()
            .max_by_key(|a| desc(a).map(|d| d.DedicatedVideoMemory).unwrap_or(0))
            .context("no hardware GPU found")?;
        Self::on_adapter(&adapter)
    }

    fn on_adapter(adapter: &IDXGIAdapter1) -> Result<Self> {
        let desc = desc(adapter)?;
        let levels: [D3D_FEATURE_LEVEL; 2] = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
        let mut device = None;
        let mut context = None;
        // SAFETY: out-pointers are valid; with an explicit adapter the driver type must be UNKNOWN.
        unsafe {
            D3D11CreateDevice(
                adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                Some(&levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }
        .context("can't create the D3D11 device")?;
        let device: ID3D11Device = device.context("D3D11 returned no device")?;
        let context = context.context("D3D11 returned no context")?;

        // Capture callbacks, the converter and Media Foundation all touch the context from their own threads.
        let mt: ID3D11Multithread = device.cast()?;
        // SAFETY: valid interface; returns the previous setting.
        let _ = unsafe { mt.SetMultithreadProtected(true) };

        let name_len = desc
            .Description
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(desc.Description.len());
        let gpu = Self {
            device,
            context,
            adapter_name: String::from_utf16_lossy(&desc.Description[..name_len]),
            vendor: Vendor::from_pci_id(desc.VendorId),
            dedicated_memory: desc.DedicatedVideoMemory as u64,
            luid: desc.AdapterLuid,
            vendor_id: desc.VendorId,
            device_id: desc.DeviceId,
        };
        tracing::info!("GPU: {} ({:?})", gpu.adapter_name, gpu.vendor);
        Ok(gpu)
    }

    /// The same device as a WinRT `IDirect3DDevice`, which Windows Graphics Capture needs.
    pub fn winrt_device(&self) -> Result<IDirect3DDevice> {
        let dxgi: IDXGIDevice = self.device.cast()?;
        // SAFETY: valid DXGI device.
        let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }
            .context("can't wrap the D3D11 device for WinRT")?;
        Ok(inspectable.cast()?)
    }
}

/// Hardware adapters (no WARP / Basic Render Driver).
fn adapters() -> Result<Vec<IDXGIAdapter1>> {
    // SAFETY: plain factory creation.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.context("can't create a DXGI factory")?;
    let mut out = Vec::new();
    let mut index = 0;
    // SAFETY: EnumAdapters1 errors (DXGI_ERROR_NOT_FOUND) once past the last adapter.
    while let Ok(adapter) = unsafe { factory.EnumAdapters1(index) } {
        index += 1;
        if desc(&adapter).is_ok_and(|d| d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 == 0) {
            out.push(adapter);
        }
    }
    Ok(out)
}

fn desc(adapter: &IDXGIAdapter1) -> Result<DXGI_ADAPTER_DESC1> {
    // SAFETY: valid adapter.
    Ok(unsafe { adapter.GetDesc1() }?)
}

/// Gives clipr's GPU work a little priority over the game's, so a game running at uncapped FPS can't starve
/// the capture (stutter and dropped frames in clips). Opt-in: beta.3 used the HIGH class and the top GPU
/// thread priority, which cost a game frames on an RTX 4070 Ti, so this stays at ABOVE_NORMAL. Best effort.
pub fn raise_gpu_priority(_gpu: &Gpu) {
    const ABOVE_NORMAL: i32 = 3;
    // SAFETY: the current-process pseudo handle; a valid D3DKMT_SCHEDULINGPRIORITYCLASS.
    let status = unsafe {
        D3DKMTSetProcessSchedulingPriorityClass(
            windows::Win32::System::Threading::GetCurrentProcess(),
            ABOVE_NORMAL,
        )
    };
    if status >= 0 {
        tracing::info!("GPU scheduling priority: above normal");
    } else {
        tracing::info!("GPU scheduling priority left at normal (Windows refused)");
    }
}

/// Registers the calling thread with the Windows multimedia scheduler (MMCSS, task "Capture"), so it keeps
/// getting CPU time when a game loads every core. Lasts until the thread ends.
pub fn raise_thread_priority() {
    let task: Vec<u16> = "Capture".encode_utf16().chain([0]).collect();
    let mut index = 0u32;
    // SAFETY: valid NUL-terminated string and out-pointer.
    let handle = unsafe { AvSetMmThreadCharacteristicsW(task.as_ptr(), &mut index) };
    if handle == 0 {
        tracing::debug!("MMCSS registration failed; using a raised thread priority instead");
        // SAFETY: the current-thread pseudo handle.
        let _ = unsafe {
            windows::Win32::System::Threading::SetThreadPriority(
                windows::Win32::System::Threading::GetCurrentThread(),
                windows::Win32::System::Threading::THREAD_PRIORITY_ABOVE_NORMAL,
            )
        };
    }
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn D3DKMTSetProcessSchedulingPriorityClass(
        process: windows::Win32::Foundation::HANDLE,
        class: i32,
    ) -> i32;
}

#[link(name = "avrt")]
unsafe extern "system" {
    fn AvSetMmThreadCharacteristicsW(task: *const u16, index: *mut u32) -> isize;
}
