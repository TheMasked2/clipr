//! The in-game overlay: clipr's own pages (the Gallery, Settings) in a window over the game, opened and closed
//! with a hotkey, so a clip can be watched, trimmed or shared without alt-tabbing out.
//!
//! It is a plain topmost Win32 window holding a WebView2 view of the local web UI (`?overlay`), not a hook
//! into the game: nothing is injected into the game's process. Games that run borderless or
//! windowed (most games on Windows 10/11, which turn "fullscreen" into borderless) show it on top; a game in
//! true exclusive fullscreen is minimised by Windows while it's open.
//!
//! It never ends up in a clip: window capture only takes the game's window, and the overlay is excluded from
//! screen capture (`WDA_EXCLUDEFROMCAPTURE`, like the "Clip saved" toast, Windows 10 2004 or newer), so monitor
//! capture, OBS and Discord screen sharing don't see it either.
//!
//! The web view is made the first time the overlay opens (WebView2 costs ~100 MB while it exists), then kept,
//! so it opens instantly after that and comes back on the page it was left on.

use std::cell::{Cell, RefCell};
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
use windows_sys::Win32::System::Threading::GetCurrentProcessId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetForegroundWindow, GetMessageW,
    GetWindowThreadProcessId, HWND_TOPMOST, IsWindow, IsWindowVisible, MSG, PostMessageW, RegisterClassW,
    SW_HIDE, SWP_SHOWWINDOW, SetForegroundWindow, SetWindowDisplayAffinity, SetWindowPos, ShowWindow,
    TranslateMessage, WA_INACTIVE, WDA_EXCLUDEFROMCAPTURE, WM_ACTIVATE, WM_APP, WM_CLOSE, WNDCLASSW,
    WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
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
    /// Declared first so it's dropped last: the view runs in the WebView2 environment it set up.
    webview: Option<WebView>,
    context: Option<WebContext>,
    /// The window that had the focus before the overlay opened (the game), to give it back.
    previous: isize,
    /// WebView2 couldn't start (not installed?): the hotkey opens the browser instead.
    failed: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    /// WebView2 is starting. Its start-up pumps this thread's messages, so the hotkey, Alt+F4 or the game taking
    /// the focus back can reach the window procedure in the middle of `show`: they're noted, not handled.
    static BUILDING: Cell<bool> = const { Cell::new(false) };
    /// Something asked to close the overlay while WebView2 was starting: close it once it's up.
    static HIDE_AFTER_BUILD: Cell<bool> = const { Cell::new(false) };
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
            webview: None,
            context: None,
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
    // SAFETY: our own window.
    if !capturable && unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) } == 0 {
        tracing::warn!("this Windows can't keep the in-game overlay out of screen captures (needs 10 2004+)");
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
    if BUILDING.get() && matches!(msg, WM_TOGGLE | WM_HIDE | WM_CLOSE) {
        HIDE_AFTER_BUILD.set(true);
        return 0;
    }
    match msg {
        WM_TOGGLE => {
            // SAFETY: our own window.
            if unsafe { IsWindowVisible(hwnd) } != 0 {
                hide(hwnd, true);
            } else {
                show(hwnd);
            }
            0
        }
        WM_HIDE | WM_CLOSE => {
            hide(hwnd, true);
            0
        }
        // Another window took over (alt-tab, a click on another screen): get out of its way, and leave the
        // focus where the user put it. A click into the web view doesn't come here, since its window is a child
        // of this one, and clipr's own windows (the folder picker) don't count.
        WM_ACTIVATE if (wparam & 0xFFFF) as u32 == WA_INACTIVE => {
            let mut pid = 0;
            // SAFETY: a window query that fails harmlessly on a null or stale handle.
            unsafe { GetWindowThreadProcessId(lparam as HWND, &mut pid) };
            // SAFETY: our own window.
            if unsafe { IsWindowVisible(hwnd) } == 0 || pid == unsafe { GetCurrentProcessId() } {
                return 0;
            }
            tracing::debug!("in-game overlay: another window took the focus ({lparam:#x})");
            if BUILDING.get() {
                HIDE_AFTER_BUILD.set(true);
            } else {
                hide(hwnd, false);
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
    // Short borrows only: making the web view pumps messages, which can come back here (see `BUILDING`).
    enum Next {
        Gone,
        Failed(String),
        Build(String, PathBuf),
        Shown,
    }
    let next = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let Some(s) = s.as_mut() else { return Next::Gone };
        if front != hwnd {
            s.previous = front as isize;
        }
        if s.failed {
            Next::Failed(s.url.clone())
        } else if s.webview.is_none() {
            Next::Build(s.url.clone(), s.data_dir.clone())
        } else {
            Next::Shown
        }
    });
    let opened = match next {
        Next::Gone => return,
        Next::Failed(url) => Err(url),
        Next::Shown => Ok(()),
        Next::Build(url, data_dir) => {
            BUILDING.set(true);
            HIDE_AFTER_BUILD.set(false);
            let built = build(hwnd, &url, &data_dir);
            BUILDING.set(false);
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                let Some(s) = s.as_mut() else { return Err(url) };
                match built {
                    Ok((view, context)) => {
                        s.webview = Some(view);
                        s.context = Some(context);
                        Ok(())
                    }
                    Err(e) => {
                        tracing::warn!(
                            "the in-game overlay can't show clipr (is WebView2 installed?): {e:#}"
                        );
                        s.failed = true;
                        Err(url)
                    }
                }
            })
        }
    };
    tracing::info!("in-game overlay opened (shows clipr: {})", opened.is_ok());
    if let Err(url) = opened {
        // No web view: hide again, give the game its focus back and open the Gallery in the browser instead.
        hide(hwnd, true);
        crate::win::open_url(&format!("{url}#/gallery"));
        return;
    }
    if HIDE_AFTER_BUILD.replace(false) {
        hide(hwnd, true);
        return;
    }
    STATE.with(|s| {
        if let Some(view) = s.borrow().as_ref().and_then(|s| s.webview.as_ref()) {
            let _ = view.set_visible(true);
            // The page refreshes what may have changed while it was hidden (new clips) and focuses itself.
            let _ = view.evaluate_script("window.dispatchEvent(new Event('clipr-overlay-open'))");
            let _ = view.focus();
        }
    });
    // SAFETY: our own window. The hotkey that opened it was this process's input, so Windows lets it take the
    // focus.
    unsafe { SetForegroundWindow(hwnd) };
}

/// Hides the overlay and stops anything playing in it. `refocus`: give the focus back to the game (the user
/// closed it), not when another window took the focus.
fn hide(hwnd: HWND, refocus: bool) {
    tracing::info!("in-game overlay closed");
    let previous = STATE.with(|s| {
        let s = s.try_borrow().ok()?;
        let s = s.as_ref()?;
        if let Some(view) = &s.webview {
            let _ = view.evaluate_script(
                "document.querySelectorAll('video,audio').forEach((m) => m.pause());\
                 window.dispatchEvent(new Event('clipr-overlay-close'))",
            );
            // Tells WebView2 the page is hidden: its meters and timers stop while the game runs.
            let _ = view.set_visible(false);
        }
        Some(s.previous)
    });
    // SAFETY: our own window, then a window handle that is checked first.
    unsafe {
        ShowWindow(hwnd, SW_HIDE);
        if let Some(previous) = previous.filter(|&p| refocus && p != 0)
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

fn build(hwnd: HWND, url: &str, data_dir: &std::path::Path) -> anyhow::Result<(WebView, WebContext)> {
    let mut context = WebContext::new(Some(data_dir.join("webview")));
    let url = format!("{url}?overlay#/gallery");
    let view = WebViewBuilder::new_with_web_context(&mut context)
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
            if url.starts_with("https://") || url.starts_with("http://") {
                crate::win::open_url(&url);
            }
            NewWindowResponse::Deny
        })
        .build(&Parent(hwnd as isize))?;
    Ok((view, context))
}
