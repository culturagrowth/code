//! Data model of the games database.

use serde::{Deserialize, Serialize};

/// A capture backend. `DdaCrop` and `Wgc` never touch the game process; `Hook` injects a DLL.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    /// DXGI Desktop Duplication cropped to the game window.
    DdaCrop,
    /// Windows.Graphics.Capture of the game window.
    Wgc,
    /// Injected hook (optional future mode, games without anti-cheat only).
    Hook,
}

/// An anti-cheat system. In JSON it is spelled in `snake_case`, except that `BattlEye` is
/// `"battleye"` (the spelling `"battl_eye"` produced by a naive `snake_case` rule is accepted
/// too). A name this version does not know is read as [`AntiCheat::Other`], so a newer remote
/// database never breaks an older client, and the unknown system still counts as an anti-cheat.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AntiCheat {
    None,
    Vanguard,
    Eac,
    #[serde(rename = "battleye", alias = "battl_eye")]
    BattlEye,
    Vac,
    Faceit,
    GamersClub,
    Ricochet,
    Hyperion,
    Javelin,
    Ace,
    Adhesive,
    /// Any other (or not yet known) anti-cheat. Must stay the last variant.
    #[serde(other)]
    Other,
}

/// One game of the database.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct GameEntry {
    /// Stable slug (`[a-z0-9-]`), e.g. `"cs2"`.
    pub id: String,
    pub name: String,
    /// Matched case-insensitively against the process image basename.
    pub exe_names: Vec<String>,
    /// Empty or `[None]` means no anti-cheat.
    pub anticheat: Vec<AntiCheat>,
    pub kernel_anticheat: bool,
    pub allowed_backends: Vec<Backend>,
    /// `None` means "use the OS default".
    pub default_backend: Option<Backend>,
    /// Short Portuguese hint shown in the UI (e.g. "use tela cheia sem bordas").
    #[serde(default)]
    pub notes_pt: String,
    /// `false` until confirmed by a real test.
    #[serde(default)]
    pub verified: bool,
}

impl GameEntry {
    /// True when at least one real anti-cheat is listed (`[]` and `[None]` mean no anti-cheat).
    pub fn has_anticheat(&self) -> bool {
        self.anticheat.iter().any(|a| *a != AntiCheat::None)
    }

    /// True when the hook backend could ever be used for this game: no anti-cheat and `Hook` is
    /// in `allowed_backends`. (The user's opt-in and the installed component are separate gates.)
    pub fn allows_hook(&self) -> bool {
        !self.has_anticheat() && self.allowed_backends.contains(&Backend::Hook)
    }
}

/// The games database.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct GamesDb {
    pub version: u32,
    pub games: Vec<GameEntry>,
}

/// Why a games database was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DbError {
    #[error("games database is {len} bytes, the maximum is {max}")]
    TooLarge { len: usize, max: usize },
    #[error("invalid games database JSON: {0}")]
    Parse(String),
    #[error("game {id:?}: the name is empty")]
    EmptyName { id: String },
    #[error("game id {id:?} is not a valid slug (non-empty, only a-z, 0-9 and '-')")]
    InvalidId { id: String },
    #[error("duplicate game id {id:?}")]
    DuplicateId { id: String },
    #[error("game {id:?}: exe_names is empty")]
    NoExeNames { id: String },
    #[error("game {id:?}: invalid exe name {exe:?} (must be a bare file name ending in .exe)")]
    InvalidExeName { id: String, exe: String },
    #[error("exe name {exe:?} is used by both {first:?} and {second:?}")]
    DuplicateExe {
        exe: String,
        first: String,
        second: String,
    },
    #[error("game {id:?}: hook is allowed but the game has an anti-cheat")]
    HookWithAntiCheat { id: String },
    #[error("game {id:?}: kernel_anticheat is set but no anti-cheat is listed")]
    KernelWithoutAntiCheat { id: String },
    #[error("game {id:?}: default_backend is not in allowed_backends")]
    DefaultNotAllowed { id: String },
    #[error("game {id:?}: allowed_backends has no non-hook backend")]
    NoNonHookBackend { id: String },
}
