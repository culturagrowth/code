# duoclip-gamesdb — SPEC (games database + capture-backend selection)

Pure Rust, platform-independent, `#![forbid(unsafe_code)]`. Implements docs sections 4.5–4.8:
- the default capture is **non-injection**;
- the optional **hook** backend is allowed only for games **without anti-cheat**, only when the user enabled it for that game, and only when the
  hook component is installed (future phase).

## API

```rust
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)] #[serde(rename_all = "snake_case")]
pub enum Backend { DdaCrop, Wgc, Hook }

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)] #[serde(rename_all = "snake_case")]
pub enum AntiCheat { None, Vanguard, Eac, BattlEye, Vac, Faceit, GamersClub, Ricochet, Hyperion, Javelin, Ace, Adhesive, Other }

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct GameEntry {
    pub id: String,                  // stable slug, e.g. "cs2"
    pub name: String,
    pub exe_names: Vec<String>,      // matched case-insensitively against the process image basename
    pub anticheat: Vec<AntiCheat>,   // empty or [None] => no anti-cheat
    pub kernel_anticheat: bool,
    pub allowed_backends: Vec<Backend>,
    pub default_backend: Option<Backend>,  // None => use the OS default
    pub notes_pt: String,            // short Portuguese hint shown in the UI (e.g. "use tela cheia sem bordas")
    pub verified: bool,              // false until confirmed by a real test
}

pub struct GamesDb { pub version: u32, pub games: Vec<GameEntry> }
impl GamesDb {
    pub fn embedded() -> Self;                                  // include_str!("../games.json"), validated in tests
    pub fn from_json(s: &str) -> Result<Self, DbError>;         // remote update (max 1 MiB); validates
    pub fn merged(base: &Self, update: &Self) -> Self;          // higher version wins per id; unknown ids added
    pub fn lookup(&self, exe_path_or_name: &str) -> Option<&GameEntry>; // basename, case-insensitive, handles \ and /
    pub fn validate(&self) -> Result<(), DbError>;
}

pub struct OsInfo { pub build: u32 }   // Windows build number (19045 = Win10 22H2, >= 22000 = Win11)
pub struct Environment { pub os: OsInfo, pub borderless_wgc_available: bool, pub hook_installed: bool }
pub struct UserPrefs { pub per_game_backend: std::collections::HashMap<String, Backend>, pub hook_enabled_games: std::collections::HashSet<String> }

#[derive(Debug, Clone, PartialEq)]
pub struct BackendChoice { pub primary: Backend, pub fallbacks: Vec<Backend>, pub reason_pt: String }

pub fn choose_backend(game: Option<&GameEntry>, env: &Environment, prefs: &UserPrefs) -> BackendChoice;
```

## Selection rules (choose_backend)

1. OS default: if `build < 22000` (Windows 10), or Win11 without `borderless_wgc_available` → `DdaCrop`, then fallback `Wgc`.
   If Win11 with `borderless_wgc_available` → `Wgc`, then fallback `DdaCrop`.
2. A known game's `default_backend` overrides the OS default, if it is allowed. A user per-game preference overrides that, if it is allowed.
3. `Hook` may be chosen as primary **only if** ALL of these hold: the game has no anti-cheat (`anticheat` empty or only `None`), `Hook` is in
   `allowed_backends`, the user enabled hook for that game id, and `hook_installed`. Otherwise `Hook` is removed and the next rule applies.
   `Hook` is **never** a fallback.
4. Unknown games (no entry) get the OS default (never hook).
5. `reason_pt` explains the choice in Portuguese (one sentence).

## Validation rules

- Ids are unique slugs `[a-z0-9-]`. Exe names are non-empty, end with `.exe`, and are unique across games (case-insensitive).
- `Hook` in `allowed_backends` is only allowed when there is no anti-cheat, and `kernel_anticheat` implies a non-empty anticheat list.
- `default_backend` must be in `allowed_backends`. `allowed_backends` must contain at least one non-hook backend.

## Embedded data (`crates/duoclip-gamesdb/games.json`)

About 30 games popular in Brazil:

| Grupo | Jogos |
|---|---|
| Competitivos | CS2, Valorant, League of Legends, Dota 2, Fortnite, Apex Legends, Rainbow Six Siege, PUBG, Call of Duty/Warzone, Marvel Rivals, Overwatch 2 |
| Mundo aberto e sobrevivência | GTA V, FiveM, Rust, Elden Ring, Destiny 2 |
| Esportes | Rocket League, EA FC |
| Infantis e casuais | Roblox, Minecraft Java (`javaw.exe`, note: shared with other Java apps → match only with window-title hints later, mark in notes), Minecraft Bedrock, Fall Guys |
| Cooperativos e indies | Dead by Daylight, Valheim, Terraria, Among Us, Lethal Company, Phasmophobia, Stardew Valley, Hollow Knight |

- Use your best knowledge of each game's anti-cheat, and set `verified: false` everywhere (the research pass will confirm later).
- Only games with no anti-cheat may list `hook`.
- Notes in Portuguese, e.g. "CS2: use tela cheia sem bordas" when relevant.

## Tests (required)

- The embedded DB parses and validates.
- `lookup`: case, full paths with `\` and `/`, and an unknown exe.
- `choose_backend` truth table:
  - Win10 vs Win11, with and without borderless;
  - known vs unknown game;
  - user preference allowed vs disallowed;
  - hook only when every condition holds (test each failing condition, including anti-cheat present, not installed, not enabled, not allowed);
  - hook never appears in fallbacks.
- Validation rejects every rule violation. `merged` handles version precedence.
- `cargo clippy -p duoclip-gamesdb --all-targets -- -D warnings` is clean and `cargo fmt` is applied.

## Implementation notes (decisions the SPEC left open)

- **Layout:** `model.rs` (types, `DbError`), `db.rs` (`embedded`/`from_json`/`validate`/`merged`/`lookup`), `select.rs` (`choose_backend`).
  Extra public items: `MAX_DB_BYTES` (1 MiB), `DbError`, `GameEntry::{has_anticheat, allows_hook}`, `OsInfo::is_windows_11`, and `Default`
  for `UserPrefs`.
- **Anti-cheat JSON names:** `snake_case`, except `BattlEye`, which is `"battleye"` (`"battl_eye"` is accepted too). A name this version
  does not know is read as `Other` (still counts as an anti-cheat), so a newer remote DB never breaks an older client.
  `has_anticheat()` is true when any entry other than `None` is listed (`[]` and `[None]` mean "no anti-cheat").
- **Validation extras:** the name is non-empty, `exe_names` is non-empty, exe names are bare file names (no path separators or characters
  Windows forbids, no surrounding whitespace, non-empty stem before `.exe`), and `kernel_anticheat` requires a real anti-cheat (not just
  `[None]`). `from_json` ignores unknown fields; `notes_pt` and `verified` may be omitted (`verified` defaults to `false`).
- **`embedded()`** returns an empty DB (version 0) instead of panicking if the embedded JSON were ever broken; the tests make sure it is not.
- **`merged`:** the DB with the higher `version` wins for ids present in both; on a tie `base` wins. Ids present on one side only are kept.
  An entry of the losing side whose id or exe names collide with an already kept entry is dropped, so merging two valid DBs is always valid.
  Result version = max of both; the winner's games come first.
- **`lookup`:** the basename is taken after trimming whitespace and `"`; matching is Unicode-lowercase, exact (no wildcards). FiveM therefore
  lists the known `FiveM_bNNNN_GTAProcess.exe` builds explicitly.
- **`choose_backend`:**
  - the hook is a primary only when it was *requested* (user preference `Hook`, or no usable preference and the game's `default_backend` is
    `Hook`) AND every rule-3 condition holds. `hook_enabled_games` alone does not select the hook;
  - candidates in order: user preference, game default, OS default. A candidate that is not in `allowed_backends` (or a blocked hook) is
    skipped, and `reason_pt` says why;
  - fallbacks are the other non-hook backends in OS-default order, limited to the game's `allowed_backends`;
  - an inconsistent entry that allows no usable backend yields the plain OS default (never a panic, never the hook).
- **Data:** 30 games in `games.json`, all `verified: false`, `default_backend: null`. Only games without anti-cheat list `hook`
  (Minecraft Java, Valheim, Terraria, Among Us, Lethal Company, Phasmophobia, Stardew Valley, Hollow Knight); Minecraft Bedrock has no
  anti-cheat but does not list `hook` (protected Store/GDK process). Entries marked "(a confirmar)" in the notes are the least certain
  (Marvel Rivals, League of Legends/Vanguard, Phasmophobia).
