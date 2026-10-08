//! Windows part of the recorder: the wiring of capture/audio/encode/buffer/mux, the hotkey
//! message loop, sounds, foreground polling and Ctrl+C.

#![deny(clippy::undocumented_unsafe_blocks)]

pub mod app;
pub mod audio;
pub mod ctrlc;
pub mod session;
pub mod sounds;
pub mod sys;
pub mod video;

use duoclip_buffer::Packet;

/// Messages from the capture/audio threads to the session.
#[derive(Debug)]
pub enum Event {
    /// An encoded packet (video or the mixed audio track).
    Packet(Packet),
    /// The video encoder in use.
    Encoder {
        /// MFT name.
        name: String,
        /// Hardware MFT.
        hardware: bool,
        /// Properties the encoder rejected (informative).
        warnings: Vec<String>,
    },
    /// A non-fatal problem (Portuguese).
    Warning(String),
    /// The session cannot continue (Portuguese reason).
    Fatal(String),
    /// The capture ended (window closed, ...; Portuguese reason).
    CaptureEnded(String),
}

#[cfg(test)]
mod tests {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;

    use crate::hotkey::{self, Hotkey};

    /// The portable virtual-key and modifier values match the `windows` crate constants.
    #[test]
    fn hotkey_codes_match_the_windows_crate() {
        let vk = |s: &str| Hotkey::parse(s).unwrap().key.vk;
        assert_eq!(vk("F1"), VK_F1.0);
        assert_eq!(vk("F10"), VK_F10.0);
        assert_eq!(vk("F24"), VK_F24.0);
        assert_eq!(vk("Alt+A"), VK_A.0);
        assert_eq!(vk("Alt+Z"), VK_Z.0);
        assert_eq!(vk("Alt+0"), VK_0.0);
        assert_eq!(vk("Alt+9"), VK_9.0);
        assert_eq!(vk("Num0"), VK_NUMPAD0.0);
        assert_eq!(vk("Num9"), VK_NUMPAD9.0);
        assert_eq!(vk("Insert"), VK_INSERT.0);
        assert_eq!(vk("Delete"), VK_DELETE.0);
        assert_eq!(vk("Home"), VK_HOME.0);
        assert_eq!(vk("End"), VK_END.0);
        assert_eq!(vk("PageUp"), VK_PRIOR.0);
        assert_eq!(vk("PageDown"), VK_NEXT.0);
        assert_eq!(vk("Pause"), VK_PAUSE.0);
        assert_eq!(vk("ScrollLock"), VK_SCROLL.0);
        assert_eq!(vk("PrintScreen"), VK_SNAPSHOT.0);
        assert_eq!(vk("Ctrl+Space"), VK_SPACE.0);
        assert_eq!(vk("Up"), VK_UP.0);
        assert_eq!(vk("Down"), VK_DOWN.0);
        assert_eq!(vk("Left"), VK_LEFT.0);
        assert_eq!(vk("Right"), VK_RIGHT.0);
        assert_eq!(hotkey::MOD_ALT, MOD_ALT.0);
        assert_eq!(hotkey::MOD_CONTROL, MOD_CONTROL.0);
        assert_eq!(hotkey::MOD_SHIFT, MOD_SHIFT.0);
        assert_eq!(hotkey::MOD_WIN, MOD_WIN.0);
        assert_eq!(hotkey::MOD_NOREPEAT, MOD_NOREPEAT.0);
    }

    /// The default config path is under %APPDATA% (computed only; nothing is written).
    #[test]
    fn default_config_path_is_under_appdata() {
        if let (Some(p), Some(appdata)) = (
            super::sys::default_config_path(),
            std::env::var_os("APPDATA"),
        ) {
            assert!(p.starts_with(appdata));
            assert!(p.ends_with(r"DuoClip\config.toml"));
        }
    }
}
