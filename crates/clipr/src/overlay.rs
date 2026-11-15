//! The "Clip saved" toast: a small, click-through, topmost window in the top-right corner of the monitor
//! under the mouse, shown for a few seconds.
//!
//! It is a plain Win32 window, not a game hook, so games in exclusive fullscreen hide it. It is excluded from
//! screen capture (`WDA_EXCLUDEFROMCAPTURE`), so it never ends up in a clip.

use std::sync::mpsc;
use std::time::Duration;

use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateRoundRectRgn, CreateSolidBrush, DT_CENTER, DT_SINGLELINE, DT_VCENTER,
    DeleteObject, DrawTextW, EndPaint, FW_SEMIBOLD, FillRect, GetMonitorInfoW, InvalidateRect,
    MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint, PAINTSTRUCT, SelectObject, SetBkMode,
    SetTextColor, SetWindowRgn, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetCursorPos, KillTimer, LWA_ALPHA, MSG, PM_REMOVE,
    PeekMessageW, RegisterClassW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SetLayeredWindowAttributes,
    SetTimer, SetWindowDisplayAffinity, SetWindowPos, ShowWindow, TranslateMessage, WDA_EXCLUDEFROMCAPTURE,
    WM_ERASEBKGND, WM_NCPAINT, WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

const WIDTH: i32 = 280;
const HEIGHT: i32 = 56;
const MARGIN: i32 = 24;
const SHOW_FOR: Duration = Duration::from_millis(2500);
const HIDE_TIMER: usize = 1;

/// Sends texts to the toast thread.
#[derive(Clone)]
pub struct Toast(mpsc::Sender<(String, bool)>);

impl Toast {
    /// Shows `text` (green accent when `ok`, red otherwise).
    pub fn show(&self, text: &str, ok: bool) {
        let _ = self.0.send((text.to_string(), ok));
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

thread_local! {
    static CURRENT: std::cell::RefCell<(Vec<u16>, bool)> = const { std::cell::RefCell::new((Vec::new(), true)) };
}

/// Starts the toast thread.
pub fn start() -> Toast {
    let (tx, rx) = mpsc::channel::<(String, bool)>();
    std::thread::Builder::new()
        .name("clipr-toast".into())
        .spawn(move || run(rx))
        .expect("can't start a thread");
    Toast(tx)
}

fn run(rx: mpsc::Receiver<(String, bool)>) {
    let class = wide("clipr-toast");
    // SAFETY: standard window class registration and creation; all strings outlive the calls.
    let hwnd = unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc_),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassW(&wc);
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class.as_ptr(),
            class.as_ptr(),
            WS_POPUP,
            0,
            0,
            WIDTH,
            HEIGHT,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        tracing::warn!("couldn't create the toast window");
        return;
    }
    // SAFETY: valid window we own.
    unsafe {
        SetLayeredWindowAttributes(hwnd, 0, 235, LWA_ALPHA);
        SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
        SetWindowRgn(hwnd, CreateRoundRectRgn(0, 0, WIDTH + 1, HEIGHT + 1, 16, 16), 1);
    }
    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok((text, ok)) => {
                CURRENT.with(|c| *c.borrow_mut() = (wide(&text), ok));
                show(hwnd);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        // SAFETY: plain message pump for this thread's window.
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

fn show(hwnd: HWND) {
    // SAFETY: valid window; plain geometry calls.
    unsafe {
        let mut cursor = POINT { x: 0, y: 0 };
        GetCursorPos(&mut cursor);
        let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTOPRIMARY);
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(monitor, &mut info);
        let work = info.rcWork;
        SetWindowPos(
            hwnd,
            -1isize as HWND, // HWND_TOPMOST
            work.right - WIDTH - MARGIN,
            work.top + MARGIN,
            WIDTH,
            HEIGHT,
            SWP_NOACTIVATE,
        );
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        InvalidateRect(hwnd, std::ptr::null(), 1);
        SetTimer(hwnd, HIDE_TIMER, SHOW_FOR.as_millis() as u32, None);
    }
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            // SAFETY: standard paint cycle on our own window; GDI objects are deleted after use.
            unsafe {
                let mut ps: PAINTSTRUCT = std::mem::zeroed();
                let dc = BeginPaint(hwnd, &mut ps);
                let rect = RECT {
                    left: 0,
                    top: 0,
                    right: WIDTH,
                    bottom: HEIGHT,
                };
                let bg = CreateSolidBrush(rgb(0x16, 0x16, 0x1d));
                FillRect(dc, &rect, bg);
                DeleteObject(bg);
                let (text, ok) = CURRENT.with(|c| c.borrow().clone());
                let bar = CreateSolidBrush(if ok {
                    rgb(0x8B, 0x5C, 0xF6)
                } else {
                    rgb(0xEF, 0x44, 0x44)
                });
                FillRect(
                    dc,
                    &RECT {
                        left: 0,
                        top: 0,
                        right: 6,
                        bottom: HEIGHT,
                    },
                    bar,
                );
                DeleteObject(bar);
                let face = wide("Segoe UI");
                let font = CreateFontW(
                    20,
                    0,
                    0,
                    0,
                    FW_SEMIBOLD as i32,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    5,
                    0,
                    face.as_ptr(),
                );
                let old = SelectObject(dc, font);
                SetBkMode(dc, TRANSPARENT as i32);
                SetTextColor(dc, rgb(0xE5, 0xE5, 0xEA));
                let mut text_rect = RECT { left: 10, ..rect };
                // Windows may paint before any text was set: draw nothing then. The length excludes the NUL.
                let mut text = text;
                let len = text.iter().position(|&c| c == 0).unwrap_or(text.len());
                if len > 0 {
                    DrawTextW(
                        dc,
                        text.as_mut_ptr(),
                        len as i32,
                        &mut text_rect,
                        DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                    );
                }
                SelectObject(dc, old);
                DeleteObject(font);
                EndPaint(hwnd, &ps);
            }
            0
        }
        // No frame to draw, and the paint handler fills the whole window.
        WM_NCPAINT => 0,
        WM_ERASEBKGND => 1,
        WM_TIMER if wparam == HIDE_TIMER => {
            // SAFETY: our own window and timer.
            unsafe {
                KillTimer(hwnd, HIDE_TIMER);
                ShowWindow(hwnd, SW_HIDE);
            }
            0
        }
        // SAFETY: default handling for everything else.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16)
}
