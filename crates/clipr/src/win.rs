//! Thin wrappers around the few Win32 calls the app shell needs.

use anyhow::{Result, bail};
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows_sys::Win32::System::Threading::{CreateMutexW, GetCurrentThreadId};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MB_ICONERROR, MB_OK, MSG, MessageBoxW, PostQuitMessage,
    PostThreadMessageW, SW_SHOWNORMAL, TranslateMessage, WM_APP, WM_HOTKEY, WM_QUIT,
};

/// Posted to the main thread when it should look at its queues (menu clicks, status changes).
pub const WM_CLIPR_WAKE: u32 = WM_APP + 1;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A named mutex held for the whole run, so only one clipr runs per Windows session.
pub struct SingleInstance(HANDLE);

impl SingleInstance {
    /// `Ok(None)` when another clipr already holds it.
    pub fn acquire() -> Result<Option<Self>> {
        let name = wide("Local\\clipr-single-instance");
        // SAFETY: `name` is a valid NUL-terminated wide string that outlives the call.
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        // SAFETY: reads this thread's last error, set by the call above.
        let err = unsafe { GetLastError() };
        if handle.is_null() {
            bail!("can't create the single-instance lock (error {err})");
        }
        if err == ERROR_ALREADY_EXISTS {
            // SAFETY: `handle` is a valid handle we own.
            unsafe { CloseHandle(handle) };
            return Ok(None);
        }
        Ok(Some(Self(handle)))
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        // SAFETY: we own the handle and close it once.
        unsafe { CloseHandle(self.0) };
    }
}

/// Opens a URL in the default browser.
pub fn open_url(url: &str) {
    let (op, file) = (wide("open"), wide(url));
    // SAFETY: all strings are valid NUL-terminated wide strings; null pointers are allowed for the rest.
    unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            op.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
}

pub fn error_box(text: &str) {
    let (text, title) = (wide(text), wide("clipr"));
    // SAFETY: valid NUL-terminated wide strings; no owner window.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        )
    };
}

pub fn current_thread_id() -> u32 {
    // SAFETY: no preconditions.
    unsafe { GetCurrentThreadId() }
}

/// Wakes the main thread's message loop from any thread.
pub fn wake(thread_id: u32) {
    // SAFETY: posting a plain thread message has no memory-safety preconditions; failure is harmless.
    unsafe { PostThreadMessageW(thread_id, WM_CLIPR_WAKE, 0, 0) };
}

pub fn quit_loop() {
    // SAFETY: no preconditions; ends this thread's message loop.
    unsafe { PostQuitMessage(0) };
}

/// Ends another thread's message loop (like the tray's Quit, from any thread).
pub fn quit_thread(thread_id: u32) {
    // SAFETY: posting a plain thread message has no memory-safety preconditions.
    unsafe { PostThreadMessageW(thread_id, WM_QUIT, 0, 0) };
}

/// Registers a system-wide hotkey for this thread; presses arrive in [`message_loop`] as `on_hotkey(id)`.
/// Fails when another program holds the same combination.
pub fn register_hotkey(id: i32, key: clipr_core::hotkey::Hotkey) -> Result<()> {
    let mods = key.mods | clipr_core::hotkey::MOD_NOREPEAT;
    // SAFETY: null HWND = post WM_HOTKEY to this thread's queue.
    if unsafe { RegisterHotKey(std::ptr::null_mut(), id, mods, key.vk) } == 0 {
        // SAFETY: reads this thread's last error.
        bail!("it's taken by another program (error {})", unsafe {
            GetLastError()
        });
    }
    Ok(())
}

pub fn unregister_hotkey(id: i32) {
    // SAFETY: unregistering an id that isn't registered is harmless.
    unsafe { UnregisterHotKey(std::ptr::null_mut(), id) };
}

/// Shows a file selected in Explorer, or opens a folder.
pub fn reveal(path: &std::path::Path) {
    let (op, file, params);
    if path.is_dir() {
        (op, file, params) = (wide("open"), wide(&path.to_string_lossy()), wide(""));
    } else {
        (op, file, params) = (
            wide("open"),
            wide("explorer.exe"),
            wide(&format!("/select,\"{}\"", path.display())),
        );
    }
    // SAFETY: valid NUL-terminated wide strings.
    unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            op.as_ptr(),
            file.as_ptr(),
            params.as_ptr(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
}

/// Runs the message loop until [`quit_loop`]. `on_wake` runs for every [`WM_CLIPR_WAKE`], `on_hotkey` for
/// every registered hotkey press.
pub fn message_loop(mut on_wake: impl FnMut(), mut on_hotkey: impl FnMut(i32)) {
    // SAFETY: MSG is plain data; all-zero is a valid initial value.
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: `msg` is a valid out-pointer; null HWND means "any message for this thread".
        let got = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        if got <= 0 {
            return; // WM_QUIT (0) or error (-1)
        }
        if msg.hwnd.is_null() && msg.message == WM_CLIPR_WAKE {
            on_wake();
            continue;
        }
        if msg.message == WM_HOTKEY {
            on_hotkey(msg.wParam as i32);
            continue;
        }
        // SAFETY: `msg` came from GetMessageW.
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Bytes free for this user on the drive holding `path`; `None` if Windows won't say. The folder must
/// exist.
pub fn free_bytes(path: &std::path::Path) -> Option<u64> {
    let dir = wide(&path.to_string_lossy());
    let mut free = 0u64;
    // SAFETY: `dir` is a NUL-terminated wide string and `free` a valid out pointer, both alive for the call;
    // the other two out pointers may be null.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            dir.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(free)
}
