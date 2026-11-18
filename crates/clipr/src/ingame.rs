//! The in-game overlay: clipr's own pages (the Gallery, Settings) in a window over the game, opened and closed
//! with a hotkey, so a clip can be watched, trimmed or shared without alt-tabbing out.
//!
//! It is a plain topmost Win32 window holding a WebView2 view of the local web UI (`?overlay`), not a hook
//! into the game: nothing is injected, so anti-cheat has nothing to object to. Games that run borderless or
//! windowed (most games on Windows 10/11, which turn "fullscreen" into borderless) show it on top; a game in
//! true exclusive fullscreen is minimised by Windows while it's open.
//!
//! It never ends up in a clip: window capture only takes the game's window, and the overlay is excluded from
//! screen capture (`WDA_EXCLUDEFROMCAPTURE`, like the "Clip saved" toast), so monitor capture, OBS and
//! Discord screen sharing don't see it either.
//!
//! The web view is made the first time the overlay opens (WebView2 costs ~100 MB while it exists), then kept,
//! so it opens instantly after that and comes back on the page it was left on.

use std::cell::RefCell;
use std::num::NonZeroIsize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicIsize, Ordering};

use raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromWindow,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetForegroundWindow, GetMessageW, HWND_TOPMOST,
    IsWindow, IsWindowVisible, MSG, PostMessageW, RegisterClassW, SW_HIDE, SWP_SHOWWINDOW,
    SetForegroundWindow, SetWindowDisplayAffinity, SetWindowPos, ShowWindow, TranslateMessage, WA_INACTIVE,
    WDA_EXCLUDEFROMCAPTURE, WM_ACTIVATE, WM_APP, WM_CLOSE, WNDCLASSW, WS_EX_NOREDIRECTIONBITMAP,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use wry::{NewWindowResponse, WebContext, WebView, WebViewBuilder};

/// Opens the overlay, or closes it when it's open (the hotkey).
const WM_TOGGLE: u32 = WM_APP + 1;
/// Closes it (the page's ✕, Esc).
const WM_HIDE: u32 = WM_APP + 2;

/// The overlay window, once its thread made it (0 until then).
static WINDOW: AtomicIsize = AtomicIsize::new(0);

struct State {
    /// The page to show: the web UI's address.
    url: String,
    /// Where WebView2 keeps its profile (cache, local storage).
    data_dir: PathBuf,
    /// Dropped after the view (it holds the WebView2 environment the view runs in).
    context: Option<WebContext>,
    webview: Option<WebView>,
    /// The window that had the focus before the overlay opened (the game), to give it back.
    previous: isize,
    /// WebView2 couldn't start (not installed?): the hotkey opens the browser instead.
    failed: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Starts the overlay's thread. `url` is the web UI's address, `data_dir` clipr's data folder.
pub fn start(url: String, data_dir: PathBuf) {
    let spawned = std::thread::Builder::new()
        .name("clipr-ingame".into())
        .spawn(move || run(url, data_dir));
    if let Err(e) = spawned {
        tracing::warn!("can't start the in-game overlay: {e}");
    }
}

/// Opens the overlay over whatever has the focus (the game), or closes it.
pub fn toggle() {
    post(WM_TOGGLE);
}

fn post(msg: u32) {
    let hwnd = WINDOW.load(Ordering::Acquire);
    if hwnd != 0 {
        // SAFETY: posting to a window this process made; a stale handle only makes the call fail.
        unsafe { PostMessageW(hwnd as HWND, msg, 0, 0) };
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn run(url: String, data_dir: PathBuf) {
    // SAFETY: WebView2 needs a single-threaded apartment; this thread is ours alone.
    if let Err(e) = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok() {
        tracing::warn!("the in-game overlay can't start COM: {e}");
        return;
    }
    STATE.with(|s| {
        *s.borrow_mut() = Some(State {
            url,
            data_dir,
            context: None,
            webview: None,
            previous: 0,
            failed: false,
        })
    });
    let class = wide("clipr-ingame");
    let title = wide("clipr");
    // SAFETY: standard window class registration and creation; the strings outlive the calls.
    let hwnd = unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc_),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassW(&wc);
        // No redirection bitmap: the web view's see-through background shows the game behind it.
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOREDIRECTIONBITMAP,
            class.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            0,
            0,
            800,
            600,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        tracing::warn!("can't make the in-game overlay window");
        return;
    }
    // Never in clips, screenshots or screen shares. Dev builds can leave it visible to screenshots
    // (`CLIPR_OVERLAY_CAPTURABLE`), to check how it looks.
    let capturable = cfg!(debug_assertions) && std::env::var_os("CLIPR_OVERLAY_CAPTURABLE").is_some();
    if !capturable {
        // SAFETY: our own window.
        unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) };
    }
    WINDOW.store(hwnd as isize, Ordering::Release);

    // SAFETY: a standard message loop on the thread that owns the window (and, later, the web view).
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TOGGLE => {
            // SAFETY: our own window.
            if unsafe { IsWindowVisible(hwnd) } != 0 {
                hide(hwnd);
            } else {
                show(hwnd);
            }
            0
        }
        WM_HIDE | WM_CLOSE => {
            hide(hwnd);
            0
        }
        // Another window took over (alt-tab, a click on another screen): get out of its way. A click into the
        // web view doesn't come here, since its window is a child of this one.
        WM_ACTIVATE if (wparam & 0xFFFF) as u32 == WA_INACTIVE => {
            // SAFETY: our own window.
            if unsafe { IsWindowVisible(hwnd) } != 0 {
                tracing::debug!("in-game overlay: another window took the focus ({lparam:#x})");
                hide(hwnd);
            }
            0
        }
        // SAFETY: default handling for everything else.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Covers the monitor of the window in front (the game) and gives the overlay the focus.
fn show(hwnd: HWND) {
    // SAFETY: plain window and monitor queries and our own window's position.
    let front = unsafe {
        let front = GetForegroundWindow();
        let monitor = MonitorFromWindow(
            if front.is_null() { hwnd } else { front },
            MONITOR_DEFAULTTOPRIMARY,
        );
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..std::mem::zeroed()
        };
        let r = if GetMonitorInfoW(monitor, &mut info) != 0 {
            info.rcMonitor
        } else {
            RECT {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080,
            }
        };
        SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            SWP_SHOWWINDOW,
        );
        front
    };
    let opened = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let Some(s) = s.as_mut() else { return false };
        if front != hwnd {
            s.previous = front as isize;
        }
        if s.failed {
            return false;
        }
        if s.webview.is_none() {
            match build(hwnd, s) {
                Ok(()) => {}
                Err(e) => {
                    tracing::warn!("the in-game overlay can't show clipr (is WebView2 installed?): {e:#}");
                    s.failed = true;
                    return false;
                }
            }
        } else if let Some(view) = &s.webview {
            // The page refreshes what may have changed while it was hidden (new clips).
            let _ = view.evaluate_script("window.dispatchEvent(new Event('clipr-overlay-open'))");
        }
        if let Some(view) = &s.webview {
            let _ = view.focus();
        }
        true
    });
    tracing::info!("in-game overlay opened (shows clipr: {opened})");
    if opened {
        // SAFETY: our own window. The hotkey that opened it was this process's input, so Windows lets it
        // take the focus.
        unsafe { SetForegroundWindow(hwnd) };
    } else {
        // No web view: hide again and open the Gallery in the browser instead.
        // SAFETY: our own window.
        unsafe { ShowWindow(hwnd, SW_HIDE) };
        let url = STATE.with(|s| s.borrow().as_ref().map(|s| s.url.clone()));
        if let Some(url) = url {
            crate::win::open_url(&format!("{url}#/gallery"));
        }
    }
}

/// Hides the overlay, stops anything playing in it and gives the focus back to the game.
fn hide(hwnd: HWND) {
    tracing::info!("in-game overlay closed");
    let previous = STATE.with(|s| {
        let s = s.borrow();
        let s = s.as_ref()?;
        if let Some(view) = &s.webview {
            let _ = view.evaluate_script(
                "document.querySelectorAll('video,audio').forEach((m) => m.pause());\
                 window.dispatchEvent(new Event('clipr-overlay-close'))",
            );
        }
        Some(s.previous)
    });
    // SAFETY: our own window, then a window handle that is checked first.
    unsafe {
        ShowWindow(hwnd, SW_HIDE);
        if let Some(previous) = previous.filter(|&p| p != 0)
            && IsWindow(previous as HWND) != 0
        {
            SetForegroundWindow(previous as HWND);
        }
    }
}

/// The overlay window, for wry.
struct Parent(isize);

impl HasWindowHandle for Parent {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let handle = Win32WindowHandle::new(NonZeroIsize::new(self.0).ok_or(HandleError::Unavailable)?);
        // SAFETY: the overlay window lives as long as its thread, which owns the web view too.
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(handle)) })
    }
}

fn build(hwnd: HWND, s: &mut State) -> anyhow::Result<()> {
    let context = s
        .context
        .insert(WebContext::new(Some(s.data_dir.join("webview"))));
    let url = format!("{}?overlay#/gallery", s.url);
    let view = WebViewBuilder::new_with_web_context(context)
        .with_url(url)
        .with_transparent(true)
        .with_focused(true)
        // The page's ✕ and Esc.
        .with_ipc_handler(|req| {
            if req.body() == "close" {
                post(WM_HIDE);
            }
        })
        // Links that open a new window (a shared clip's page, Help) go to the browser, not over the game.
        .with_new_window_req_handler(|url, _| {
            crate::win::open_url(&url);
            NewWindowResponse::Deny
        })
        .build(&Parent(hwnd as isize))?;
    s.webview = Some(view);
    Ok(())
}
