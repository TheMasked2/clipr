//! The Windows folder picker, for the settings page's "Browse" buttons.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{ERROR_CANCELLED, HWND};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, CoUninitialize,
};
use windows::Win32::UI::Shell::{
    FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, IShellItem,
    SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
};
use windows::core::HSTRING;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN, SetForegroundWindow,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

/// Shows the folder picker, opening at `start` (or the nearest folder above it that exists), and waits until
/// the user picks a folder (`Some`) or cancels (`None`). The dialog needs a single-threaded apartment, so it
/// gets a thread of its own.
pub fn pick_folder(title: &str, start: &Path) -> Result<Option<PathBuf>, String> {
    let (title, start) = (title.to_owned(), start.to_owned());
    std::thread::spawn(move || {
        // SAFETY: COM is set up and torn down on this thread only; the owner window lives until it returns.
        unsafe { pick(&title, &start) }
    })
    .join()
    .map_err(|_| "The folder picker crashed.".to_string())?
    .map_err(|e| format!("Couldn't show the folder picker: {}", e.message()))
}

unsafe fn pick(title: &str, start: &Path) -> windows::core::Result<Option<PathBuf>> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).ok()?;
        struct Com;
        impl Drop for Com {
            fn drop(&mut self) {
                // SAFETY: balances the successful CoInitializeEx above, on the same thread.
                unsafe { CoUninitialize() }
            }
        }
        let _com = Com;
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        let options = dialog.GetOptions()?;
        dialog.SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST)?;
        dialog.SetTitle(&HSTRING::from(title))?;
        if let Some(dir) = start
            .ancestors()
            .find(|d| !d.as_os_str().is_empty() && d.is_dir())
        {
            if let Ok(item) =
                SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(dir.as_os_str()), None)
            {
                let _ = dialog.SetFolder(&item);
            }
        }
        // The browser has the focus, and Windows won't let clipr take it: an invisible always-on-top owner in
        // the middle of the screen keeps the dialog above the browser instead of behind it.
        let owner = Owner::new();
        match dialog.Show(Some(owner.hwnd())) {
            Err(e) if e.code() == ERROR_CANCELLED.to_hresult() => return Ok(None),
            r => r?,
        }
        let name = dialog.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?;
        let path = name.to_string();
        CoTaskMemFree(Some(name.0 as _));
        Ok(Some(PathBuf::from(path?)))
    }
}

struct Owner(windows_sys::Win32::Foundation::HWND);

impl Owner {
    fn new() -> Self {
        let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        // SAFETY: a plain popup of a system class with no parent; destroyed in Drop on this thread.
        unsafe {
            let (w, h) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                class.as_ptr(),
                std::ptr::null(),
                WS_POPUP,
                w / 2,
                h / 3,
                0,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            );
            if !hwnd.is_null() {
                SetForegroundWindow(hwnd);
            }
            Self(hwnd)
        }
    }

    fn hwnd(&self) -> HWND {
        HWND(self.0)
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: our own window, created on this thread.
            unsafe { DestroyWindow(self.0) };
        }
    }
}
