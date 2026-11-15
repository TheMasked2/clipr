//! Thin wrappers around the few Win32 calls the app shell needs.

use anyhow::{Result, bail};
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows_sys::Win32::System::Threading::{CreateMutexW, GetCurrentThreadId};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MB_ICONERROR, MB_OK, MSG, MessageBoxW, PostQuitMessage,
    PostThreadMessageW, SW_SHOWNORMAL, TranslateMessage, WM_APP,
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

/// Runs the message loop until [`quit_loop`]. `on_wake` runs for every [`WM_CLIPR_WAKE`].
pub fn message_loop(mut on_wake: impl FnMut()) {
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
        // SAFETY: `msg` came from GetMessageW.
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
