//! The few Win32 calls game detection needs: foreground window, process image name, monitor.

use windows::Win32::Foundation::{CloseHandle, HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetDesktopWindow, GetForegroundWindow, GetShellWindow, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId,
};
use windows::core::PWSTR;

/// The foreground window and the process that owns it.
pub struct Foreground {
    pub hwnd: isize,
    pub pid: u32,
    /// Lower-case file name of the process image, e.g. `valorant-win64-shipping.exe`.
    pub exe: String,
    pub title: String,
    /// Covers its whole monitor, and is not the desktop or shell window.
    pub fullscreen: bool,
}

pub fn foreground() -> Option<Foreground> {
    // SAFETY: plain query with no arguments; returns a null handle when there is no foreground window.
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() {
        return None;
    }
    let mut pid = 0u32;
    // SAFETY: `hwnd` is a window handle from the call above and `pid` is a valid out pointer.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 {
        return None;
    }
    let exe = process_exe(pid)?;
    Some(Foreground {
        hwnd: hwnd.0 as isize,
        pid,
        exe,
        title: window_title(hwnd),
        fullscreen: is_fullscreen(hwnd),
    })
}

/// The lower-case image file name of a running process, or `None` if it is gone or inaccessible.
pub fn process_exe(pid: u32) -> Option<String> {
    // SAFETY: OpenProcess has no memory preconditions; the handle is closed below.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: `buf` is valid for `len` UTF-16 units and `len` is updated to the number written.
    let res =
        unsafe { QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) };
    // SAFETY: `handle` was opened above and is not used afterwards.
    let _ = unsafe { CloseHandle(handle) };
    res.ok()?;
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    let name = path.rsplit(['\\', '/']).next().unwrap_or(&path);
    Some(name.to_ascii_lowercase())
}

fn window_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: `buf` is a valid writable slice for the duration of the call.
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// The monitor a window is mostly on, falling back to the primary monitor.
pub fn monitor_of(hwnd: isize) -> isize {
    // SAFETY: MonitorFromWindow tolerates stale handles; with DEFAULTTOPRIMARY it still returns a monitor.
    unsafe { MonitorFromWindow(HWND(hwnd as *mut _), MONITOR_DEFAULTTOPRIMARY) }.0 as isize
}

fn is_fullscreen(hwnd: HWND) -> bool {
    // SAFETY: plain queries with no arguments.
    let (shell, desktop) = unsafe { (GetShellWindow(), GetDesktopWindow()) };
    if hwnd == shell || hwnd == desktop {
        return false;
    }
    let mut rect = RECT::default();
    // SAFETY: `rect` is a valid out pointer.
    if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
        return false;
    }
    // SAFETY: as in `monitor_of`.
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY) };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `info.cbSize` is set and `info` is a valid out pointer.
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        return false;
    }
    let m = info.rcMonitor;
    rect.left <= m.left && rect.top <= m.top && rect.right >= m.right && rect.bottom >= m.bottom
}
