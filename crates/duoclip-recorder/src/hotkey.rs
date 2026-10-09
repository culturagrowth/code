//! Hotkey strings (`"Alt+F10"`) ↔ `RegisterHotKey` modifiers + virtual-key code.
//!
//! The virtual-key codes are the documented Win32 values (winuser.h); a Windows-only unit test
//! compares them with the `windows` crate constants.

#![forbid(unsafe_code)]

use std::fmt;

/// `MOD_ALT` (RegisterHotKey).
pub const MOD_ALT: u32 = 0x0001;
/// `MOD_CONTROL`.
pub const MOD_CONTROL: u32 = 0x0002;
/// `MOD_SHIFT`.
pub const MOD_SHIFT: u32 = 0x0004;
/// `MOD_WIN`.
pub const MOD_WIN: u32 = 0x0008;
/// `MOD_NOREPEAT`: holding the keys down does not repeat `WM_HOTKEY`.
pub const MOD_NOREPEAT: u32 = 0x4000;

/// A parsed hotkey.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkey {
    /// Ctrl held.
    pub ctrl: bool,
    /// Alt held.
    pub alt: bool,
    /// Shift held.
    pub shift: bool,
    /// Windows key held.
    pub win: bool,
    /// The (non-modifier) key.
    pub key: Key,
}

/// One non-modifier key: its virtual-key code and canonical name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    /// Win32 virtual-key code.
    pub vk: u16,
    /// Canonical name, as written back by `Display`.
    pub name: &'static str,
}

/// Why a hotkey string was rejected (Portuguese message).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct HotkeyError(pub String);

impl Hotkey {
    /// Alt+F10 (the default), for when even the default string could not be parsed.
    pub const FALLBACK: Hotkey = Hotkey {
        ctrl: false,
        alt: true,
        shift: false,
        win: false,
        key: Key {
            vk: 0x79,
            name: "F10",
        },
    };

    /// Parses `"Ctrl+Shift+F9"`-style strings: modifiers (Ctrl/Control, Alt, Shift, Win/Windows)
    /// and exactly one key, separated by `+`, case-insensitive, spaces ignored. Letters, digits
    /// and Space need Ctrl, Alt or Win (otherwise the hotkey would swallow normal typing).
    pub fn parse(s: &str) -> Result<Hotkey, HotkeyError> {
        let mut h = Hotkey {
            ctrl: false,
            alt: false,
            shift: false,
            win: false,
            key: Key { vk: 0, name: "" },
        };
        let mut key: Option<Key> = None;
        let parts: Vec<&str> = s.split('+').map(str::trim).collect();
        if parts.iter().all(|p| p.is_empty()) {
            return Err(HotkeyError(
                "está vazio; use algo como \"Alt+F10\"".to_string(),
            ));
        }
        for part in parts {
            if part.is_empty() {
                return Err(HotkeyError(
                    "tem um \"+\" sobrando; use algo como \"Alt+F10\"".to_string(),
                ));
            }
            let lower = part.to_ascii_lowercase();
            let modifier = match lower.as_str() {
                "ctrl" | "control" => Some(&mut h.ctrl),
                "alt" => Some(&mut h.alt),
                "shift" => Some(&mut h.shift),
                "win" | "windows" => Some(&mut h.win),
                _ => None,
            };
            if let Some(flag) = modifier {
                if *flag {
                    return Err(HotkeyError(format!(
                        "o modificador {part} aparece duas vezes"
                    )));
                }
                *flag = true;
                continue;
            }
            let Some(k) = key_by_name(&lower) else {
                return Err(HotkeyError(format!(
                    "tecla \"{part}\" desconhecida; use F1–F24, A–Z, 0–9, Insert, Delete, Home, End, PageUp, PageDown, Pause, ScrollLock, PrintScreen, Num0–Num9, setas ou Space"
                )));
            };
            if key.is_some() {
                return Err(HotkeyError(
                    "tem mais de uma tecla; use só uma tecla mais os modificadores (Ctrl, Alt, Shift, Win)"
                        .to_string(),
                ));
            }
            key = Some(k);
        }
        let Some(k) = key else {
            return Err(HotkeyError(
                "falta a tecla; use os modificadores mais uma tecla, ex.: \"Alt+F10\"".to_string(),
            ));
        };
        h.key = k;
        if is_typing_key(k.vk) && !(h.ctrl || h.alt || h.win) {
            return Err(HotkeyError(format!(
                "a tecla {} precisa de Ctrl, Alt ou Win junto (senão atrapalha a digitação)",
                k.name
            )));
        }
        Ok(h)
    }

    /// `RegisterHotKey` modifiers (without `MOD_NOREPEAT`).
    pub fn modifiers(&self) -> u32 {
        let mut m = 0;
        if self.alt {
            m |= MOD_ALT;
        }
        if self.ctrl {
            m |= MOD_CONTROL;
        }
        if self.shift {
            m |= MOD_SHIFT;
        }
        if self.win {
            m |= MOD_WIN;
        }
        m
    }

    /// Virtual-key code for `RegisterHotKey`.
    pub fn vk(&self) -> u32 {
        u32::from(self.key.vk)
    }
}

impl fmt::Display for Hotkey {
    /// Canonical form: `Ctrl+Alt+Shift+Win+Key`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (on, name) in [
            (self.ctrl, "Ctrl"),
            (self.alt, "Alt"),
            (self.shift, "Shift"),
            (self.win, "Win"),
        ] {
            if on {
                write!(f, "{name}+")?;
            }
        }
        f.write_str(self.key.name)
    }
}

/// Letters, digits and Space.
fn is_typing_key(vk: u16) -> bool {
    vk == 0x20 || (0x30..=0x39).contains(&vk) || (0x41..=0x5A).contains(&vk)
}

/// Named keys (lowercase aliases → key). Letters, digits and F-keys are computed.
const NAMED: &[(&str, Key)] = &[
    ("insert", key(0x2D, "Insert")),
    ("ins", key(0x2D, "Insert")),
    ("delete", key(0x2E, "Delete")),
    ("del", key(0x2E, "Delete")),
    ("home", key(0x24, "Home")),
    ("end", key(0x23, "End")),
    ("pageup", key(0x21, "PageUp")),
    ("pgup", key(0x21, "PageUp")),
    ("pagedown", key(0x22, "PageDown")),
    ("pgdn", key(0x22, "PageDown")),
    ("pause", key(0x13, "Pause")),
    ("scrolllock", key(0x91, "ScrollLock")),
    ("printscreen", key(0x2C, "PrintScreen")),
    ("prtsc", key(0x2C, "PrintScreen")),
    ("space", key(0x20, "Space")),
    ("up", key(0x26, "Up")),
    ("down", key(0x28, "Down")),
    ("left", key(0x25, "Left")),
    ("right", key(0x27, "Right")),
];

const fn key(vk: u16, name: &'static str) -> Key {
    Key { vk, name }
}

const F_NAMES: [&str; 24] = [
    "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12", "F13", "F14", "F15",
    "F16", "F17", "F18", "F19", "F20", "F21", "F22", "F23", "F24",
];
const LETTERS: [&str; 26] = [
    "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R", "S",
    "T", "U", "V", "W", "X", "Y", "Z",
];
const DIGITS: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
const NUMPAD: [&str; 10] = [
    "Num0", "Num1", "Num2", "Num3", "Num4", "Num5", "Num6", "Num7", "Num8", "Num9",
];

/// Looks a (lowercase) key name up.
fn key_by_name(lower: &str) -> Option<Key> {
    if let Some((_, k)) = NAMED.iter().find(|(n, _)| *n == lower) {
        return Some(*k);
    }
    // F1..F24 → VK_F1 (0x70) + n - 1.
    if let Some(n) = lower
        .strip_prefix('f')
        .and_then(|d| d.parse::<usize>().ok())
    {
        if (1..=24).contains(&n) && !lower.starts_with("f0") {
            return Some(key(0x70 + (n as u16 - 1), F_NAMES[n - 1]));
        }
    }
    // Num0..Num9 / Numpad0..Numpad9 → VK_NUMPAD0 (0x60) + n.
    if let Some(d) = lower
        .strip_prefix("numpad")
        .or_else(|| lower.strip_prefix("num"))
    {
        if let [c @ b'0'..=b'9'] = d.as_bytes() {
            let n = usize::from(c - b'0');
            return Some(key(0x60 + n as u16, NUMPAD[n]));
        }
    }
    match lower.as_bytes() {
        // A..Z → 0x41.., 0..9 → 0x30..
        [c @ b'a'..=b'z'] => {
            let n = usize::from(c - b'a');
            Some(key(0x41 + n as u16, LETTERS[n]))
        }
        [c @ b'0'..=b'9'] => {
            let n = usize::from(c - b'0');
            Some(key(0x30 + n as u16, DIGITS[n]))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_and_round_trip() {
        let h = Hotkey::parse("Alt+F10").unwrap();
        assert_eq!(h, Hotkey::FALLBACK);
        assert_eq!(h.modifiers(), MOD_ALT);
        assert_eq!(h.vk(), 0x79);
        for s in [
            "Alt+F10",
            "Ctrl+Shift+F9",
            "Ctrl+Alt+Shift+Win+PageUp",
            "F12",
            "Shift+F1",
            "Alt+A",
            "Ctrl+0",
            "Win+Num5",
            "Ctrl+Space",
            "Insert",
            "Alt+F24",
        ] {
            let h = Hotkey::parse(s).unwrap();
            assert_eq!(h.to_string(), s);
            assert_eq!(Hotkey::parse(&h.to_string()).unwrap(), h);
        }
    }

    #[test]
    fn case_spaces_aliases_and_order() {
        let h = Hotkey::parse("  shift + CONTROL+f9 ").unwrap();
        assert_eq!(h.to_string(), "Ctrl+Shift+F9");
        assert_eq!(h.modifiers(), MOD_CONTROL | MOD_SHIFT);
        assert_eq!(
            Hotkey::parse("Windows+Del").unwrap().to_string(),
            "Win+Delete"
        );
        assert_eq!(Hotkey::parse("alt+pgdn").unwrap().key.vk, 0x22);
        assert_eq!(Hotkey::parse("alt+numpad9").unwrap().key.vk, 0x69);
        assert_eq!(Hotkey::parse("ctrl+prtsc").unwrap().key.vk, 0x2C);
    }

    #[test]
    fn key_codes() {
        let vk = |s: &str| Hotkey::parse(s).unwrap().key.vk;
        assert_eq!(vk("F1"), 0x70);
        assert_eq!(vk("F24"), 0x87);
        assert_eq!(vk("Alt+A"), 0x41);
        assert_eq!(vk("Alt+Z"), 0x5A);
        assert_eq!(vk("Alt+0"), 0x30);
        assert_eq!(vk("Alt+9"), 0x39);
        assert_eq!(vk("Num0"), 0x60);
        assert_eq!(vk("Pause"), 0x13);
        assert_eq!(vk("Home"), 0x24);
        assert_eq!(vk("ScrollLock"), 0x91);
        assert_eq!(vk("Alt+Left"), 0x25);
    }

    #[test]
    fn errors_are_portuguese_and_specific() {
        let err = |s: &str| Hotkey::parse(s).unwrap_err().0;
        assert!(err("").contains("vazio"));
        assert!(err("  ").contains("vazio"));
        assert!(err("Alt+").contains("sobrando"));
        assert!(err("+F10").contains("sobrando"));
        assert!(err("Alt+Alt+F10").contains("duas vezes"));
        assert!(err("Alt+F10+F11").contains("mais de uma tecla"));
        assert!(err("Alt+Ctrl").contains("falta a tecla"));
        assert!(err("Alt+F25").contains("desconhecida"));
        assert!(err("Alt+F0").contains("desconhecida"));
        assert!(err("Alt+F01").contains("desconhecida"));
        assert!(err("Alt+Enter").contains("desconhecida"));
        assert!(err("Alt+ç").contains("desconhecida"));
        assert!(err("Alt+Num10").contains("desconhecida"));
        assert!(err("A").contains("Ctrl, Alt ou Win"));
        assert!(err("Shift+1").contains("Ctrl, Alt ou Win"));
        assert!(err("Space").contains("Ctrl, Alt ou Win"));
        // Long / odd input never panics.
        let _ = Hotkey::parse(&"+".repeat(10_000));
        let _ = Hotkey::parse("F99999999999999999999999");
        let _ = Hotkey::parse("\u{0}+\u{1F600}");
    }
}
