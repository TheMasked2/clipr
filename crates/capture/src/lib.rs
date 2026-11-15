//! Screen capture through Windows Graphics Capture (WGC) only: no injection, no hooks.
//!
//! [`CaptureSession::start`] captures a monitor or a window into BGRA D3D11 textures that live on the shared
//! [`clipr_gpu::Gpu`], so frames can go straight to the colour converter and encoder.
//!
//! WGC only delivers a frame when the captured content changes, so a static desktop yields few frames. The
//! encoder side is expected to repeat the last frame to keep a constant frame rate.

#![cfg(windows)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use clipr_gpu::Gpu;
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureAccess, GraphicsCaptureAccessKind,
    GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
    MONITORINFO, MONITORINFOEXW, MonitorFromWindow,
};
use windows::Win32::System::WinRT::Direct3D11::IDirect3DDxgiInterfaceAccess;
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
use windows::core::{BOOL, IInspectable, Interface, factory};

/// What to capture.
#[derive(Clone, Copy, Debug)]
pub enum Target {
    PrimaryMonitor,
    Monitor(HMONITOR),
    Window(HWND),
}

/// One display, as reported by Windows.
#[derive(Clone, Debug)]
pub struct MonitorInfo {
    pub handle: HMONITOR,
    /// Device name such as `\\.\DISPLAY1`.
    pub name: String,
    /// Desktop-space rectangle.
    pub rect: RECT,
    pub primary: bool,
}

/// All attached monitors.
pub fn list_monitors() -> Result<Vec<MonitorInfo>> {
    unsafe extern "system" fn callback(monitor: HMONITOR, _dc: HDC, _rect: *mut RECT, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the address of the `Vec` passed to EnumDisplayMonitors below, which outlives the
        // enumeration and is only accessed from this thread.
        let out = unsafe { &mut *(data.0 as *mut Vec<MonitorInfo>) };
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        // SAFETY: `info` is a valid MONITORINFOEXW with cbSize set; the pointer cast to MONITORINFO is the
        // documented way to request the extended structure.
        if unsafe { GetMonitorInfoW(monitor, (&mut info as *mut MONITORINFOEXW).cast::<MONITORINFO>()) }
            .as_bool()
        {
            let len = info
                .szDevice
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(info.szDevice.len());
            out.push(MonitorInfo {
                handle: monitor,
                name: String::from_utf16_lossy(&info.szDevice[..len]),
                rect: info.monitorInfo.rcMonitor,
                // MONITORINFOF_PRIMARY
                primary: info.monitorInfo.dwFlags & 1 != 0,
            });
        }
        BOOL(1)
    }

    let mut monitors: Vec<MonitorInfo> = Vec::new();
    // SAFETY: the callback only dereferences `data`, which points at `monitors` for the whole call.
    let ok = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(callback),
            LPARAM(&mut monitors as *mut Vec<MonitorInfo> as isize),
        )
    };
    if !ok.as_bool() {
        return Err(anyhow!("can't list the monitors"));
    }
    Ok(monitors)
}

/// The primary monitor.
pub fn primary_monitor() -> HMONITOR {
    // SAFETY: a null window with DEFAULTTOPRIMARY returns the primary monitor; no pointers involved.
    unsafe { MonitorFromWindow(HWND::default(), MONITOR_DEFAULTTOPRIMARY) }
}

/// The monitor a window is mostly on (or the nearest one).
pub fn monitor_of_window(hwnd: HWND) -> HMONITOR {
    // SAFETY: an invalid window handle just yields the nearest monitor.
    unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) }
}

/// Whether Windows Graphics Capture works on this machine.
pub fn is_supported() -> bool {
    GraphicsCaptureSession::IsSupported().unwrap_or(false)
}

/// Capture settings.
#[derive(Clone, Debug)]
pub struct CaptureOptions {
    /// Include the mouse cursor.
    pub cursor: bool,
    /// Try to hide the yellow capture border (Windows 11 only; silently keeps it if unavailable).
    pub borderless: bool,
    /// Frame pool size.
    pub buffers: u32,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            cursor: true,
            borderless: true,
            buffers: 2,
        }
    }
}

/// One captured frame. Dropping it returns the surface to the capture pool.
pub struct CapturedFrame {
    /// BGRA (`DXGI_FORMAT_B8G8R8A8_UNORM`) texture on the capture `Gpu`'s device. Valid while this frame lives.
    pub texture: ID3D11Texture2D,
    /// Content size of this frame (the texture itself may be larger).
    pub width: u32,
    pub height: u32,
    /// `SystemRelativeTime`: QPC based, 100 ns units.
    pub time_100ns: i64,
    frame: Direct3D11CaptureFrame,
}

impl Drop for CapturedFrame {
    fn drop(&mut self) {
        let _ = self.frame.Close();
    }
}

// SAFETY: the frame and its texture are free-threaded WinRT / D3D11 objects (the pool is created with
// CreateFreeThreaded and the device is multithread-protected), so the frame may move to an encoder thread.
unsafe impl Send for CapturedFrame {}

type FrameCallback = Box<dyn FnMut(CapturedFrame) + Send>;

/// A running capture. Stops on [`CaptureSession::stop`] or drop.
pub struct CaptureSession {
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    item: GraphicsCaptureItem,
    frame_arrived_token: i64,
    closed_token: Option<i64>,
    size: Arc<AtomicU64>,
    stopped: bool,
}

// SAFETY: all held WinRT objects are agile / free-threaded (free-threaded frame pool).
unsafe impl Send for CaptureSession {}

fn pack(w: u32, h: u32) -> u64 {
    (u64::from(w) << 32) | u64::from(h)
}

fn unpack(v: u64) -> (u32, u32) {
    ((v >> 32) as u32, v as u32)
}

impl CaptureSession {
    /// Starts capturing. `on_frame` runs on a capture worker thread for every new frame.
    pub fn start(
        gpu: &Gpu,
        target: Target,
        options: CaptureOptions,
        on_frame: impl FnMut(CapturedFrame) + Send + 'static,
    ) -> Result<Self> {
        // WinRT needs an initialised apartment on this thread. Already-initialised or different-mode results
        // are fine, so the result is ignored.
        // SAFETY: plain COM/WinRT initialisation.
        let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };

        if !is_supported() {
            return Err(anyhow!("this version of Windows doesn't support screen capture"));
        }

        let item = create_item(target)?;
        let item_size = item.Size().context("can't read the size of the capture target")?;
        let device = gpu.winrt_device()?;
        let buffers = options.buffers.clamp(1, 16) as i32;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            buffers,
            item_size,
        )
        .context("can't create the capture frame pool")?;

        let size = Arc::new(AtomicU64::new(pack(
            item_size.Width.max(0) as u32,
            item_size.Height.max(0) as u32,
        )));
        let callback: Arc<Mutex<FrameCallback>> = Arc::new(Mutex::new(Box::new(on_frame)));

        let handler = {
            let size = size.clone();
            let gpu = gpu.clone();
            TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(move |sender, _| {
                let Some(pool) = sender.as_ref() else {
                    return Ok(());
                };
                // Drain the pool: with several frames queued we deliver all of them in order.
                while let Ok(frame) = pool.TryGetNextFrame() {
                    let content = frame.ContentSize()?;
                    let (w, h) = (content.Width.max(0) as u32, content.Height.max(0) as u32);
                    if pack(w, h) != size.load(Ordering::Relaxed) {
                        size.store(pack(w, h), Ordering::Relaxed);
                        // Resize the pool for later frames; this frame is still valid at the old size.
                        match gpu.winrt_device() {
                            Ok(dev) => {
                                let _ = pool.Recreate(
                                    &dev,
                                    DirectXPixelFormat::B8G8R8A8UIntNormalized,
                                    buffers,
                                    SizeInt32 {
                                        Width: content.Width,
                                        Height: content.Height,
                                    },
                                );
                            }
                            Err(e) => tracing::warn!("can't resize the capture pool: {e}"),
                        }
                    }
                    let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
                    // SAFETY: the surface is a D3D11 texture on our device; GetInterface checks the IID.
                    let texture: ID3D11Texture2D = unsafe { access.GetInterface()? };
                    let captured = CapturedFrame {
                        texture,
                        width: w,
                        height: h,
                        time_100ns: frame.SystemRelativeTime()?.Duration,
                        frame,
                    };
                    if let Ok(mut cb) = callback.lock() {
                        (*cb)(captured);
                    }
                }
                Ok(())
            })
        };
        let frame_arrived_token = pool
            .FrameArrived(&handler)
            .context("can't subscribe to capture frames")?;

        let session = pool
            .CreateCaptureSession(&item)
            .context("can't create the capture session")?;
        session.SetIsCursorCaptureEnabled(options.cursor).ok();
        if options.borderless {
            hide_border(&session);
        }

        // If the target (a window) closes, WGC stops delivering frames; log it so the caller can see why.
        let closed_token = item
            .Closed(&TypedEventHandler::<GraphicsCaptureItem, IInspectable>::new(
                |_, _| {
                    tracing::info!("the captured target was closed");
                    Ok(())
                },
            ))
            .ok();

        if let Err(e) = session.StartCapture() {
            let _ = pool.RemoveFrameArrived(frame_arrived_token);
            let _ = pool.Close();
            return Err(anyhow::Error::new(e).context("can't start capturing"));
        }
        tracing::info!("capture started: {}x{}", item_size.Width, item_size.Height);

        Ok(Self {
            pool,
            session,
            item,
            frame_arrived_token,
            closed_token,
            size,
            stopped: false,
        })
    }

    /// Latest known content size.
    pub fn content_size(&self) -> (u32, u32) {
        unpack(self.size.load(Ordering::Relaxed))
    }

    /// Stops capturing (same as dropping).
    pub fn stop(self) {}

    fn shutdown(&mut self) {
        if std::mem::replace(&mut self.stopped, true) {
            return;
        }
        let _ = self.pool.RemoveFrameArrived(self.frame_arrived_token);
        if let Some(token) = self.closed_token {
            let _ = self.item.RemoveClosed(token);
        }
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

impl Drop for CaptureSession {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn create_item(target: Target) -> Result<GraphicsCaptureItem> {
    let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
        .context("screen capture isn't available on this system")?;
    match target {
        Target::PrimaryMonitor => create_for_monitor(&interop, primary_monitor()),
        Target::Monitor(m) => create_for_monitor(&interop, m),
        // SAFETY: plain COM call; an invalid HWND results in an error, not UB.
        Target::Window(hwnd) => unsafe { interop.CreateForWindow(hwnd) }
            .context("can't capture that window (it may be closed, minimised, or protected from capture)"),
    }
}

fn create_for_monitor(
    interop: &IGraphicsCaptureItemInterop,
    monitor: HMONITOR,
) -> Result<GraphicsCaptureItem> {
    // SAFETY: plain COM call; an invalid HMONITOR results in an error, not UB.
    unsafe { interop.CreateForMonitor(monitor) }.context("can't capture that monitor")
}

/// Best-effort removal of the yellow border (Windows 11). Never fails the session.
fn hide_border(session: &GraphicsCaptureSession) {
    let access = match GraphicsCaptureAccess::RequestAccessAsync(GraphicsCaptureAccessKind::Borderless)
        .and_then(|op| op.get())
    {
        Ok(status) => status,
        Err(e) => {
            tracing::debug!("borderless capture access unavailable: {e}");
            return;
        }
    };
    if let Err(e) = session.SetIsBorderRequired(false) {
        tracing::debug!("can't hide the capture border (access status {access:?}): {e}");
    }
}
