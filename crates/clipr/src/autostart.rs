//! "Start with Windows": a value under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` that starts clipr
//! with `--background` (tray only, no browser tab) at login.

use anyhow::{Result, bail};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegDeleteValueW, RegOpenKeyExW,
    RegSetValueExW,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE: &str = "clipr";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn set(enabled: bool) -> Result<()> {
    let key_name = wide(RUN_KEY);
    let value = wide(VALUE);
    let mut key: HKEY = std::ptr::null_mut();
    // SAFETY: valid strings and out-pointer; the key is closed below.
    let status = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, key_name.as_ptr(), 0, KEY_SET_VALUE, &mut key) };
    if status != 0 {
        bail!("can't open the Run key (error {status})");
    }
    let status = if enabled {
        let exe = std::env::current_exe()?;
        let command = wide(&format!("\"{}\" --background", exe.display()));
        // SAFETY: valid key and data; the size is in bytes including the NUL.
        unsafe {
            RegSetValueExW(
                key,
                value.as_ptr(),
                0,
                REG_SZ,
                command.as_ptr().cast(),
                (command.len() * 2) as u32,
            )
        }
    } else {
        // SAFETY: valid key and value name. A missing value (2) is fine.
        match unsafe { RegDeleteValueW(key, value.as_ptr()) } {
            2 => 0,
            s => s,
        }
    };
    // SAFETY: we opened it.
    unsafe { RegCloseKey(key) };
    if status != 0 {
        bail!("can't change the Run key (error {status})");
    }
    tracing::info!("start with Windows: {}", if enabled { "on" } else { "off" });
    Ok(())
}
