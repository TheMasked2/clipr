//! Hotkey strings like `Alt+F10` ↔ Win32 `RegisterHotKey` modifiers and virtual-key codes.
//!
//! Format: modifiers in any order (`Ctrl`, `Alt`, `Shift`, `Win`), joined with `+`, then one key: `F1`–`F24`,
//! `A`–`Z`, `0`–`9`, `Num0`–`Num9`, `Insert`, `Delete`, `Home`, `End`, `PageUp`, `PageDown`, `Pause`,
//! `ScrollLock`, `PrintScreen`, `Space`, `Tab`, `Up`, `Down`, `Left`, `Right`. Case doesn't matter.

use anyhow::{Result, bail};

pub const MOD_ALT: u32 = 0x1;
pub const MOD_CONTROL: u32 = 0x2;
pub const MOD_SHIFT: u32 = 0x4;
pub const MOD_WIN: u32 = 0x8;
/// Holding the keys down doesn't repeat the action.
pub const MOD_NOREPEAT: u32 = 0x4000;

/// A parsed hotkey: `RegisterHotKey` modifiers (without `MOD_NOREPEAT`) and the virtual-key code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Hotkey {
    pub mods: u32,
    pub vk: u32,
}

/// Parses `Ctrl+Shift+S`. `Ok(None)` for an empty string (the hotkey is off).
pub fn parse(text: &str) -> Result<Option<Hotkey>> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let parts: Vec<&str> = text.split('+').map(str::trim).collect();
    let (key, mods) = parts.split_last().expect("split gives at least one part");
    let mut bits = 0;
    for m in mods {
        bits |= match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => MOD_CONTROL,
            "alt" => MOD_ALT,
            "shift" => MOD_SHIFT,
            "win" | "windows" | "meta" => MOD_WIN,
            other => bail!("unknown modifier \"{other}\" in hotkey \"{text}\""),
        };
    }
    let Some(vk) = key_code(key) else {
        bail!("unknown key \"{key}\" in hotkey \"{text}\"");
    };
    Ok(Some(Hotkey { mods: bits, vk }))
}

fn key_code(key: &str) -> Option<u32> {
    let k = key.to_ascii_lowercase();
    let named = match k.as_str() {
        "insert" | "ins" => 0x2D,
        "delete" | "del" => 0x2E,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" | "pgup" => 0x21,
        "pagedown" | "pgdn" => 0x22,
        "pause" => 0x13,
        "scrolllock" => 0x91,
        "printscreen" | "prtsc" => 0x2C,
        "space" => 0x20,
        "tab" => 0x09,
        "up" => 0x26,
        "down" => 0x28,
        "left" => 0x25,
        "right" => 0x27,
        _ => 0,
    };
    if named != 0 {
        return Some(named);
    }
    if let Some(n) = k.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
        return (1..=24).contains(&n).then_some(0x70 + n - 1);
    }
    if let Some(n) = k.strip_prefix("num").and_then(|n| n.parse::<u32>().ok()) {
        return (n <= 9).then_some(0x60 + n);
    }
    let mut chars = k.chars();
    match (chars.next(), chars.next()) {
        (Some(c @ 'a'..='z'), None) => Some(c.to_ascii_uppercase() as u32),
        (Some(c @ '0'..='9'), None) => Some(c as u32),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_hotkeys() {
        assert_eq!(
            parse("Alt+F10").unwrap(),
            Some(Hotkey {
                mods: MOD_ALT,
                vk: 0x79
            })
        );
        assert_eq!(
            parse("ctrl + shift + s").unwrap(),
            Some(Hotkey {
                mods: MOD_CONTROL | MOD_SHIFT,
                vk: b'S' as u32
            })
        );
        assert_eq!(parse("F24").unwrap().unwrap().vk, 0x87);
        assert_eq!(
            parse("Win+Num5").unwrap().unwrap(),
            Hotkey {
                mods: MOD_WIN,
                vk: 0x65
            }
        );
        assert_eq!(parse("  ").unwrap(), None);
    }

    #[test]
    fn rejects_nonsense() {
        assert!(parse("Hyper+F1").is_err());
        assert!(parse("Alt+F25").is_err());
        assert!(parse("Alt+").is_err());
        assert!(parse("Alt+Banana").is_err());
    }
}
