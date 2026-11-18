//! The "Clip saved" toast: a small, click-through, topmost window in the top-right corner of the monitor
//! under the mouse, shown for a few seconds.
//!
//! It is a plain Win32 window, not a game hook, so games in exclusive fullscreen hide it. It is excluded from
//! screen capture (`WDA_EXCLUDEFROMCAPTURE`), so it never ends up in a clip.

use std::sync::mpsc;
use std::time::Duration;

use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateRoundRectRgn, CreateSolidBrush, DT_CALCRECT, DT_CENTER, DT_END_ELLIPSIS,
    DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW, EndPaint, FW_SEMIBOLD, FillRect, GetDC,
    GetMonitorInfoW, HDC, HFONT, InvalidateRect, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
    PAINTSTRUCT, ReleaseDC, SelectObject, SetBkMode, SetTextColor, SetWindowRgn, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetCursorPos, KillTimer, LWA_ALPHA, MSG, PM_REMOVE,
    PeekMessageW, RegisterClassW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SetLayeredWindowAttributes,
    SetTimer, SetWindowDisplayAffinity, SetWindowPos, ShowWindow, TranslateMessage, WDA_EXCLUDEFROMCAPTURE,
    WM_ERASEBKGND, WM_NCPAINT, WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

// Small on purpose: a glance in the corner, not a banner over the game (beta.3 feedback).
/// Widest note; most fit in about 170 px.
const MAX_WIDTH: i32 = 340;
/// The width of the note showing now (it follows the text).
static WIDTH: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(168);

fn width() -> i32 {
    WIDTH.load(std::sync::atomic::Ordering::Relaxed)
}
const HEIGHT: i32 = 32;
/// Space around the text: left (after the bar) plus right.
const PADDING: i32 = 24;
const MARGIN: i32 = 16;
const BAR: i32 = 4;
const FONT_PX: i32 = 15;
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
            MAX_WIDTH,
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
    }
    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok((text, ok)) => {
                let text = wide(&text);
                let w = (measure(hwnd, &text) + BAR + PADDING).clamp(120, MAX_WIDTH);
                WIDTH.store(w, std::sync::atomic::Ordering::Relaxed);
                CURRENT.with(|c| *c.borrow_mut() = (text, ok));
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

/// The note's font. Delete it after use.
fn font() -> HFONT {
    let face = wide("Segoe UI");
    // SAFETY: plain GDI call; `face` outlives it.
    unsafe {
        CreateFontW(
            FONT_PX,
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
        )
    }
}

/// Draws (or with `DT_CALCRECT` measures) NUL-terminated `text` in the note's font. Returns the text's width.
///
/// # Safety
/// `dc` must be a valid device context.
unsafe fn draw_text(dc: HDC, text: &[u16], rect: &mut RECT, flags: u32) -> i32 {
    let len = text.iter().position(|&c| c == 0).unwrap_or(text.len());
    if len == 0 {
        return 0;
    }
    let mut text = text.to_vec();
    // SAFETY: valid DC (caller); the font is selected for the call and deleted after.
    unsafe {
        let font = font();
        let old = SelectObject(dc, font);
        DrawTextW(
            dc,
            text.as_mut_ptr(),
            len as i32,
            rect,
            flags | DT_SINGLELINE | DT_NOPREFIX,
        );
        SelectObject(dc, old);
        DeleteObject(font);
    }
    rect.right - rect.left
}

/// How wide `text` is in the note's font, in pixels.
fn measure(hwnd: HWND, text: &[u16]) -> i32 {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: MAX_WIDTH,
        bottom: HEIGHT,
    };
    // SAFETY: the window's own DC, released right after.
    unsafe {
        let dc = GetDC(hwnd);
        let w = draw_text(dc, text, &mut rect, DT_CALCRECT);
        ReleaseDC(hwnd, dc);
        w
    }
}

fn show(hwnd: HWND) {
    let w = width();
    // SAFETY: valid window; plain geometry calls. The window owns the region once set.
    unsafe {
        SetWindowRgn(hwnd, CreateRoundRectRgn(0, 0, w + 1, HEIGHT + 1, 10, 10), 1);
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
            work.right - w - MARGIN,
            work.top + MARGIN,
            w,
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
                    right: width(),
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
                        right: BAR,
                        bottom: HEIGHT,
                    },
                    bar,
                );
                DeleteObject(bar);
                SetBkMode(dc, TRANSPARENT as i32);
                SetTextColor(dc, rgb(0xE5, 0xE5, 0xEA));
                let mut text_rect = RECT {
                    left: BAR + PADDING / 2,
                    right: rect.right - PADDING / 2,
                    ..rect
                };
                // Windows may paint before any text was set: that draws nothing. Text too long for the widest note
                // ends in "…".
                draw_text(
                    dc,
                    &text,
                    &mut text_rect,
                    DT_CENTER | DT_VCENTER | DT_END_ELLIPSIS,
                );
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
