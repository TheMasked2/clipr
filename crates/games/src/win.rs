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
    /// The full path of the process image, as Windows gives it (to recognise game library folders).
    pub path: String,
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
    let path = process_path(pid)?;
    Some(Foreground {
        hwnd: hwnd.0 as isize,
        pid,
        exe: file_name(&path),
        path,
        title: window_title(hwnd),
        fullscreen: is_fullscreen(hwnd),
    })
}

/// The lower-case image file name of a running process, or `None` if it is gone or inaccessible.
pub fn process_exe(pid: u32) -> Option<String> {
    process_path(pid).map(|p| file_name(&p))
}

fn file_name(path: &str) -> String {
    path.rsplit(['\\', '/'])
        .next()
        .unwrap_or(path)
        .to_ascii_lowercase()
}

/// The full image path of a running process, or `None` if it is gone or inaccessible.
fn process_path(pid: u32) -> Option<String> {
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
    Some(String::from_utf16_lossy(&buf[..len as usize]))
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

/// The game's main window: the largest visible, not minimised, unowned top-level window of `pid`. Dialogs,
/// splash screens and launchers are smaller or owned, so window capture doesn't latch onto them.
pub fn main_window(pid: u32) -> Option<(isize, i64)> {
    use windows::Win32::Foundation::{LPARAM, TRUE};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GW_OWNER, GetWindow, IsIconic, IsWindowVisible,
    };
    use windows::core::BOOL;

    struct Search {
        pid: u32,
        best: Option<(isize, i64)>,
    }
    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: `lparam` is the `Search` passed below, alive for the whole EnumWindows call.
        let search = unsafe { &mut *(lparam.0 as *mut Search) };
        let mut pid = 0u32;
        // SAFETY: plain queries on a window handle Windows gave us; a stale handle only makes them fail.
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid != search.pid
                || !IsWindowVisible(hwnd).as_bool()
                || IsIconic(hwnd).as_bool()
                || GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.0.is_null())
            {
                return TRUE;
            }
            let mut r = RECT::default();
            if GetWindowRect(hwnd, &mut r).is_ok() {
                let area = i64::from(r.right - r.left) * i64::from(r.bottom - r.top);
                if area > 0 && search.best.is_none_or(|(_, a)| area > a) {
                    search.best = Some((hwnd.0 as isize, area));
                }
            }
        }
        TRUE
    }
    let mut search = Search { pid, best: None };
    // SAFETY: the callback only touches `search` through the LPARAM, which outlives the call.
    let _ = unsafe { EnumWindows(Some(visit), LPARAM(&raw mut search as isize)) };
    search.best
}

/// Whether `hwnd` still is a visible window of `pid`, and its area.
pub fn window_of(hwnd: isize, pid: u32) -> Option<i64> {
    use windows::Win32::UI::WindowsAndMessaging::{IsIconic, IsWindow, IsWindowVisible};
    let h = HWND(hwnd as *mut _);
    let mut owner = 0u32;
    let mut r = RECT::default();
    // SAFETY: plain queries; a stale handle only makes them fail.
    unsafe {
        if !IsWindow(Some(h)).as_bool() || !IsWindowVisible(h).as_bool() {
            return None;
        }
        GetWindowThreadProcessId(h, Some(&mut owner));
        if owner != pid {
            return None;
        }
        if IsIconic(h).as_bool() {
            // Minimised: still the game's window; keep it (capture repeats the last picture).
            return Some(i64::MAX);
        }
        GetWindowRect(h, &mut r).ok()?;
    }
    Some(i64::from(r.right - r.left) * i64::from(r.bottom - r.top))
}
