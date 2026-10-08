//! duoclip-gamesdb: the games database (anti-cheat per game) and the capture-backend selection
//! policy. See `SPEC.md` in this crate.
//!
//! The default capture never injects code into the game. The optional hook backend is only ever
//! chosen for games without anti-cheat, when the user enabled it for that game and when the hook
//! component is installed; it is never offered as a fallback.
//!
//! Nothing in this crate panics on untrusted input (the remote games database update is
//! untrusted): every failure surfaces as [`DbError`].
//!
//! ```
//! use duoclip_gamesdb::{choose_backend, Backend, Environment, GamesDb, OsInfo, UserPrefs};
//!
//! let db = GamesDb::embedded();
//! let game = db.lookup(r"C:\Games\Steam\steamapps\common\Counter-Strike Global Offensive\game\bin\win64\CS2.EXE");
//! let env = Environment { os: OsInfo { build: 19045 }, borderless_wgc_available: false, hook_installed: false };
//! let choice = choose_backend(game, &env, &UserPrefs::default());
//! assert_eq!(choice.primary, Backend::DdaCrop);
//! ```
#![forbid(unsafe_code)]

mod db;
mod model;
mod select;

pub use model::{AntiCheat, Backend, DbError, GameEntry, GamesDb};
pub use select::{choose_backend, BackendChoice, Environment, OsInfo, UserPrefs};

/// Maximum size, in bytes, accepted by [`GamesDb::from_json`] (a remote update is untrusted).
pub const MAX_DB_BYTES: usize = 1024 * 1024;
