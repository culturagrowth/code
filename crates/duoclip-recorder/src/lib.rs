//! duoclip-recorder — the local recorder (see `SPEC.md`).
//!
//! Waits for a known game in the foreground, records it (cropped Desktop Duplication → GPU
//! H.264, plus game/Discord/microphone audio mixed into one AAC track) into a RAM ring buffer,
//! and on the hotkey saves `segundos_antes` + `segundos_depois` as an MP4 in the clip folder.
//!
//! - **Portable, unit-tested modules** (no `unsafe`): [`config`], [`hotkey`], [`presets`],
//!   [`mixer`], [`naming`], [`wav`], [`clip`], [`detect`], [`cli`].
//! - **Windows** (`win`): wiring of capture/audio/encode/buffer/mux, the hotkey message loop,
//!   sounds, foreground polling and Ctrl+C.

#![deny(unsafe_op_in_unsafe_fn)]
#![cfg_attr(not(windows), forbid(unsafe_code))]
#![warn(missing_docs)]

pub mod cli;
pub mod clip;
pub mod config;
pub mod detect;
pub mod hotkey;
pub mod mixer;
pub mod naming;
pub mod presets;
pub mod wav;

#[cfg(windows)]
pub mod win;
