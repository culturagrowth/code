//! Capture-backend selection policy (`choose_backend`).

use std::collections::{HashMap, HashSet};

use crate::model::{Backend, GameEntry};

/// First Windows 11 build number.
const WIN11_FIRST_BUILD: u32 = 22000;

/// Operating system information.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OsInfo {
    /// Windows build number (19045 = Windows 10 22H2, >= 22000 = Windows 11).
    pub build: u32,
}

impl OsInfo {
    /// True for Windows 11 (build 22000 or later).
    pub fn is_windows_11(&self) -> bool {
        self.build >= WIN11_FIRST_BUILD
    }
}

/// What the machine can do right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Environment {
    pub os: OsInfo,
    /// Borderless WGC (`IsBorderRequired(false)`) was confirmed to work on this machine.
    pub borderless_wgc_available: bool,
    /// The (future) `duoclip-hook` component is installed.
    pub hook_installed: bool,
}

/// The user's per-game choices.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UserPrefs {
    /// Preferred backend per game id. `Hook` here is a request, honoured only when every hook
    /// condition holds.
    pub per_game_backend: HashMap<String, Backend>,
    /// Game ids for which the user switched the hook mode on.
    pub hook_enabled_games: HashSet<String>,
}

/// The outcome of [`choose_backend`].
#[derive(Debug, Clone, PartialEq)]
pub struct BackendChoice {
    /// Backend to start with.
    pub primary: Backend,
    /// Backends to try, in order, if `primary` fails to start. Never contains `Hook` nor
    /// `primary`.
    pub fallbacks: Vec<Backend>,
    /// One sentence in Portuguese explaining the choice.
    pub reason_pt: String,
}

/// Why the hook backend cannot be used for a game (checked in this order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HookBlock {
    AntiCheat,
    NotAllowed,
    NotEnabled,
    NotInstalled,
}

impl HookBlock {
    fn why_pt(self) -> &'static str {
        match self {
            Self::AntiCheat => "o jogo tem anti-cheat",
            Self::NotAllowed => "o hook não é permitido para este jogo",
            Self::NotEnabled => "o usuário não ligou o hook para este jogo",
            Self::NotInstalled => "o componente de hook não está instalado",
        }
    }
}

/// `None` when the hook may be used as primary backend for `game`, otherwise the first failing
/// condition.
fn hook_block(game: &GameEntry, env: &Environment, prefs: &UserPrefs) -> Option<HookBlock> {
    if game.has_anticheat() {
        Some(HookBlock::AntiCheat)
    } else if !game.allowed_backends.contains(&Backend::Hook) {
        Some(HookBlock::NotAllowed)
    } else if !prefs.hook_enabled_games.contains(&game.id) {
        Some(HookBlock::NotEnabled)
    } else if !env.hook_installed {
        Some(HookBlock::NotInstalled)
    } else {
        None
    }
}

/// Non-injection backends in order of preference for this machine (rule 1).
fn os_default_order(env: &Environment) -> [Backend; 2] {
    if env.os.is_windows_11() && env.borderless_wgc_available {
        [Backend::Wgc, Backend::DdaCrop]
    } else {
        [Backend::DdaCrop, Backend::Wgc]
    }
}

fn os_reason_pt(env: &Environment) -> &'static str {
    if !env.os.is_windows_11() {
        "porque no Windows 10 o WGC mostra borda amarela"
    } else if env.borderless_wgc_available {
        "porque o Windows 11 permite WGC sem borda"
    } else {
        "porque o WGC sem borda não está disponível neste Windows 11"
    }
}

fn label_pt(b: Backend) -> &'static str {
    match b {
        Backend::DdaCrop => "Desktop Duplication recortado",
        Backend::Wgc => "Windows Graphics Capture (WGC)",
        Backend::Hook => "hook",
    }
}

/// Where the chosen primary backend came from.
#[derive(Clone, Copy)]
enum Source {
    User,
    GameDefault,
    OsDefault,
}

/// Picks the capture backend for `game` (`None` = unknown game).
///
/// 1. OS default: Windows 10, or Windows 11 without borderless WGC, gives `DdaCrop` then `Wgc`;
///    Windows 11 with borderless WGC gives `Wgc` then `DdaCrop`.
/// 2. A known game's `default_backend` overrides it when allowed; the user's per-game preference
///    overrides that when allowed.
/// 3. `Hook` is chosen only if the game has no anti-cheat, `Hook` is in `allowed_backends`, the
///    user enabled hook for that game id and `hook_installed` holds. Otherwise it is skipped.
///    It is a primary only when requested (user preference, or the game's default) and is never
///    a fallback.
/// 4. Unknown games get the OS default and never the hook.
///
/// Fallbacks are the other non-injection backends in OS-default order (limited to the game's
/// `allowed_backends`). The function never fails: if an inconsistent entry allows no usable
/// backend, the OS default is used.
pub fn choose_backend(
    game: Option<&GameEntry>,
    env: &Environment,
    prefs: &UserPrefs,
) -> BackendChoice {
    let os_order = os_default_order(env);

    let Some(game) = game else {
        return BackendChoice {
            primary: os_order[0],
            fallbacks: vec![os_order[1]],
            reason_pt: format!(
                "Jogo desconhecido: captura sem injeção com {}, {}.",
                label_pt(os_order[0]),
                os_reason_pt(env)
            ),
        };
    };

    let requests = [
        (prefs.per_game_backend.get(&game.id).copied(), Source::User),
        (game.default_backend, Source::GameDefault),
    ];

    let mut notes: Vec<String> = Vec::new();
    let mut chosen: Option<(Backend, Source)> = None;
    for (backend, source) in requests {
        let Some(backend) = backend else { continue };
        if backend == Backend::Hook {
            match hook_block(game, env, prefs) {
                None => {
                    chosen = Some((backend, source));
                    break;
                }
                Some(block) => notes.push(format!("hook bloqueado ({})", block.why_pt())),
            }
        } else if game.allowed_backends.contains(&backend) {
            chosen = Some((backend, source));
            break;
        } else {
            notes.push(format!(
                "{} não é permitido para este jogo",
                label_pt(backend)
            ));
        }
    }

    let (primary, source) = chosen.unwrap_or_else(|| {
        let first_allowed = os_order
            .into_iter()
            .find(|b| game.allowed_backends.contains(b))
            // Inconsistent entry (validation rejects it): fall back to the plain OS default.
            .unwrap_or(os_order[0]);
        (first_allowed, Source::OsDefault)
    });

    let fallbacks: Vec<Backend> = os_order
        .into_iter()
        .filter(|b| *b != primary && game.allowed_backends.contains(b))
        .collect();

    let how = match (primary, source) {
        (Backend::Hook, _) => "hook ativado (jogo sem anti-cheat e ligado pelo usuário), com captura sem injeção como reserva".to_owned(),
        (b, Source::User) => format!("preferência do usuário: {}", label_pt(b)),
        (b, Source::GameDefault) => format!("método padrão do banco de jogos: {}", label_pt(b)),
        (b, Source::OsDefault) => {
            format!("captura sem injeção com {}, {}", label_pt(b), os_reason_pt(env))
        }
    };
    notes.push(how);
    let reason_pt = format!("{}: {}.", game.name, notes.join("; "));

    BackendChoice {
        primary,
        fallbacks,
        reason_pt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AntiCheat;
    use crate::GamesDb;

    const WIN10: u32 = 19045;
    const WIN11: u32 = 22631;

    fn env(build: u32, borderless: bool, hook_installed: bool) -> Environment {
        Environment {
            os: OsInfo { build },
            borderless_wgc_available: borderless,
            hook_installed,
        }
    }

    fn plain_env(build: u32, borderless: bool) -> Environment {
        env(build, borderless, false)
    }

    /// A game with no anti-cheat that allows everything.
    fn open_game() -> GameEntry {
        GameEntry {
            id: "open".into(),
            name: "Open Game".into(),
            exe_names: vec!["open.exe".into()],
            anticheat: vec![],
            kernel_anticheat: false,
            allowed_backends: vec![Backend::DdaCrop, Backend::Wgc, Backend::Hook],
            default_backend: None,
            notes_pt: String::new(),
            verified: false,
        }
    }

    /// A game with anti-cheat (no hook allowed).
    fn guarded_game() -> GameEntry {
        GameEntry {
            id: "guarded".into(),
            name: "Guarded Game".into(),
            anticheat: vec![AntiCheat::Eac],
            kernel_anticheat: true,
            allowed_backends: vec![Backend::DdaCrop, Backend::Wgc],
            ..open_game()
        }
    }

    fn prefs_hook(id: &str) -> UserPrefs {
        UserPrefs {
            per_game_backend: HashMap::from([(id.to_owned(), Backend::Hook)]),
            hook_enabled_games: HashSet::from([id.to_owned()]),
        }
    }

    fn prefs_backend(id: &str, b: Backend) -> UserPrefs {
        UserPrefs {
            per_game_backend: HashMap::from([(id.to_owned(), b)]),
            hook_enabled_games: HashSet::new(),
        }
    }

    fn assert_choice(c: &BackendChoice, primary: Backend, fallbacks: &[Backend]) {
        assert_eq!(c.primary, primary, "{c:?}");
        assert_eq!(c.fallbacks, fallbacks, "{c:?}");
        assert!(!c.fallbacks.contains(&Backend::Hook), "{c:?}");
        assert!(!c.fallbacks.contains(&c.primary), "{c:?}");
        assert!(c.reason_pt.ends_with('.'), "{c:?}");
        assert!(!c.reason_pt.is_empty());
    }

    // ---- rule 1 + 4: OS default and unknown games ----

    #[test]
    fn os_default_truth_table_for_unknown_games() {
        use Backend::{DdaCrop, Wgc};
        let p = UserPrefs::default();
        // (build, borderless, primary, fallbacks)
        let cases = [
            (WIN10, false, DdaCrop, [Wgc]),
            (WIN10, true, DdaCrop, [Wgc]), // borderless flag is meaningless on Windows 10
            (21999, true, DdaCrop, [Wgc]), // last Windows 10 build
            (22000, false, DdaCrop, [Wgc]),
            (22000, true, Wgc, [DdaCrop]), // first Windows 11 build
            (WIN11, false, DdaCrop, [Wgc]),
            (WIN11, true, Wgc, [DdaCrop]),
            (0, false, DdaCrop, [Wgc]),
            (u32::MAX, true, Wgc, [DdaCrop]),
        ];
        for (build, borderless, primary, fallbacks) in cases {
            let c = choose_backend(None, &plain_env(build, borderless), &p);
            assert_choice(&c, primary, &fallbacks);
            assert!(c.reason_pt.contains("desconhecido"), "{c:?}");
        }
    }

    #[test]
    fn unknown_game_never_gets_hook_even_if_everything_is_on() {
        let e = env(WIN11, true, true);
        let mut p = prefs_hook("open");
        p.hook_enabled_games.insert("anything".into());
        let c = choose_backend(None, &e, &p);
        assert_choice(&c, Backend::Wgc, &[Backend::DdaCrop]);
    }

    #[test]
    fn known_game_without_overrides_gets_os_default() {
        use Backend::{DdaCrop, Wgc};
        let g = guarded_game();
        let p = UserPrefs::default();
        assert_choice(
            &choose_backend(Some(&g), &plain_env(WIN10, false), &p),
            DdaCrop,
            &[Wgc],
        );
        assert_choice(
            &choose_backend(Some(&g), &plain_env(WIN11, false), &p),
            DdaCrop,
            &[Wgc],
        );
        assert_choice(
            &choose_backend(Some(&g), &plain_env(WIN11, true), &p),
            Wgc,
            &[DdaCrop],
        );
        let c = choose_backend(Some(&g), &plain_env(WIN10, false), &p);
        assert!(c.reason_pt.starts_with("Guarded Game: "), "{c:?}");
    }

    // ---- rule 2: game default and user preference ----

    #[test]
    fn game_default_overrides_os_default_when_allowed() {
        use Backend::{DdaCrop, Wgc};
        let mut g = guarded_game();
        g.default_backend = Some(Wgc);
        let p = UserPrefs::default();
        assert_choice(
            &choose_backend(Some(&g), &plain_env(WIN10, false), &p),
            Wgc,
            &[DdaCrop],
        );
        g.default_backend = Some(DdaCrop);
        assert_choice(
            &choose_backend(Some(&g), &plain_env(WIN11, true), &p),
            DdaCrop,
            &[Wgc],
        );
    }

    #[test]
    fn game_default_not_allowed_is_ignored() {
        use Backend::{DdaCrop, Wgc};
        let mut g = guarded_game();
        g.allowed_backends = vec![DdaCrop];
        g.default_backend = Some(Wgc); // inconsistent: validation would reject it
        let c = choose_backend(Some(&g), &plain_env(WIN11, true), &UserPrefs::default());
        // OS default would be Wgc, which the game does not allow => DdaCrop, no fallbacks.
        assert_choice(&c, DdaCrop, &[]);
        assert!(c.reason_pt.contains("não é permitido"), "{c:?}");
    }

    #[test]
    fn user_preference_overrides_game_default_when_allowed() {
        use Backend::{DdaCrop, Wgc};
        let mut g = guarded_game();
        g.default_backend = Some(DdaCrop);
        let p = prefs_backend("guarded", Wgc);
        let c = choose_backend(Some(&g), &plain_env(WIN10, false), &p);
        assert_choice(&c, Wgc, &[DdaCrop]);
        assert!(c.reason_pt.contains("preferência do usuário"), "{c:?}");
    }

    #[test]
    fn user_preference_not_allowed_falls_back_to_game_default_then_os() {
        use Backend::{DdaCrop, Wgc};
        let mut g = guarded_game();
        g.allowed_backends = vec![DdaCrop, Wgc];
        // Preference for a backend the game does not list.
        let mut only_dda = g.clone();
        only_dda.allowed_backends = vec![DdaCrop];
        let p = prefs_backend("guarded", Wgc);
        let c = choose_backend(Some(&only_dda), &plain_env(WIN11, true), &p);
        assert_choice(&c, DdaCrop, &[]);
        assert!(c.reason_pt.contains("não é permitido"), "{c:?}");

        // Disallowed preference => the game default is used.
        let mut g2 = only_dda.clone();
        g2.default_backend = Some(DdaCrop);
        let c = choose_backend(Some(&g2), &plain_env(WIN11, true), &p);
        assert_choice(&c, DdaCrop, &[]);
        assert!(c.reason_pt.contains("método padrão do banco"), "{c:?}");

        // Preference for a *disallowed* backend never wins over an allowed default, whatever the OS.
        let c = choose_backend(Some(&g2), &plain_env(WIN10, false), &p);
        assert_choice(&c, DdaCrop, &[]);
    }

    #[test]
    fn user_preference_for_other_game_is_ignored() {
        use Backend::{DdaCrop, Wgc};
        let g = guarded_game();
        let p = prefs_backend("some-other-game", Wgc);
        assert_choice(
            &choose_backend(Some(&g), &plain_env(WIN10, false), &p),
            DdaCrop,
            &[Wgc],
        );
    }

    // ---- rule 3: hook ----

    fn hook_env() -> Environment {
        env(WIN10, false, true)
    }

    #[test]
    fn hook_is_chosen_when_every_condition_holds() {
        use Backend::{DdaCrop, Wgc};
        let g = open_game();
        let c = choose_backend(Some(&g), &hook_env(), &prefs_hook("open"));
        assert_choice(&c, Backend::Hook, &[DdaCrop, Wgc]);
        assert!(c.reason_pt.contains("hook"), "{c:?}");

        // Fallback order follows the OS default.
        let c = choose_backend(Some(&g), &env(WIN11, true, true), &prefs_hook("open"));
        assert_choice(&c, Backend::Hook, &[Wgc, DdaCrop]);
    }

    #[test]
    fn hook_as_game_default_also_needs_the_user_opt_in() {
        use Backend::{DdaCrop, Wgc};
        let mut g = open_game();
        g.default_backend = Some(Backend::Hook);
        let mut p = UserPrefs::default();
        p.hook_enabled_games.insert("open".into());
        let c = choose_backend(Some(&g), &hook_env(), &p);
        assert_choice(&c, Backend::Hook, &[DdaCrop, Wgc]);
        // Not enabled by the user: falls to the OS default.
        let c = choose_backend(Some(&g), &hook_env(), &UserPrefs::default());
        assert_choice(&c, DdaCrop, &[Wgc]);
        assert!(c.reason_pt.contains("não ligou"), "{c:?}");
    }

    #[test]
    fn hook_is_not_used_unless_requested() {
        // Enabled + installed + allowed + no anti-cheat, but nobody asked for it.
        let g = open_game();
        let mut p = UserPrefs::default();
        p.hook_enabled_games.insert("open".into());
        let c = choose_backend(Some(&g), &hook_env(), &p);
        assert_choice(&c, Backend::DdaCrop, &[Backend::Wgc]);
        // A different explicit preference wins over the enabled flag.
        let mut p = prefs_backend("open", Backend::Wgc);
        p.hook_enabled_games.insert("open".into());
        let c = choose_backend(Some(&g), &hook_env(), &p);
        assert_choice(&c, Backend::Wgc, &[Backend::DdaCrop]);
    }

    #[test]
    fn hook_blocked_by_each_failing_condition() {
        use Backend::{DdaCrop, Wgc};
        let base_env = hook_env();
        let base_prefs = prefs_hook("open");
        let g = open_game();
        // Sanity: the base case selects hook.
        assert_eq!(
            choose_backend(Some(&g), &base_env, &base_prefs).primary,
            Backend::Hook
        );

        // 1. anti-cheat present (each anti-cheat; the entry is inconsistent but must be safe).
        for ac in [
            AntiCheat::Vanguard,
            AntiCheat::Eac,
            AntiCheat::BattlEye,
            AntiCheat::Vac,
            AntiCheat::Faceit,
            AntiCheat::GamersClub,
            AntiCheat::Ricochet,
            AntiCheat::Hyperion,
            AntiCheat::Javelin,
            AntiCheat::Ace,
            AntiCheat::Adhesive,
            AntiCheat::Other,
        ] {
            for acs in [vec![ac], vec![AntiCheat::None, ac]] {
                let mut g = open_game(); // Hook is (wrongly) in allowed_backends
                g.anticheat = acs;
                let c = choose_backend(Some(&g), &base_env, &base_prefs);
                assert_choice(&c, DdaCrop, &[Wgc]);
                assert!(c.reason_pt.contains("anti-cheat"), "{c:?}");
            }
        }

        // 2. hook not in allowed_backends
        let mut g2 = open_game();
        g2.allowed_backends = vec![DdaCrop, Wgc];
        let c = choose_backend(Some(&g2), &base_env, &base_prefs);
        assert_choice(&c, DdaCrop, &[Wgc]);
        assert!(c.reason_pt.contains("não é permitido"), "{c:?}");

        // 3. user did not enable hook for this game (preference alone is not enough)
        let mut p = base_prefs.clone();
        p.hook_enabled_games.clear();
        let c = choose_backend(Some(&g), &base_env, &p);
        assert_choice(&c, DdaCrop, &[Wgc]);
        assert!(c.reason_pt.contains("não ligou"), "{c:?}");
        // enabled for a different game only
        p.hook_enabled_games.insert("other".into());
        assert_eq!(choose_backend(Some(&g), &base_env, &p).primary, DdaCrop);

        // 4. hook component not installed
        let c = choose_backend(Some(&g), &env(WIN10, false, false), &base_prefs);
        assert_choice(&c, DdaCrop, &[Wgc]);
        assert!(c.reason_pt.contains("não está instalado"), "{c:?}");
    }

    #[test]
    fn blocked_hook_uses_game_default_before_os_default() {
        use Backend::{DdaCrop, Wgc};
        let mut g = open_game();
        g.default_backend = Some(Wgc);
        let mut p = prefs_hook("open");
        p.hook_enabled_games.clear(); // blocked
        let c = choose_backend(Some(&g), &hook_env(), &p);
        assert_choice(&c, Wgc, &[DdaCrop]);
        assert!(c.reason_pt.contains("hook bloqueado"), "{c:?}");
        assert!(c.reason_pt.contains("método padrão"), "{c:?}");
    }

    #[test]
    fn hook_is_never_a_fallback_for_any_combination() {
        let builds = [WIN10, WIN11];
        let bools = [false, true];
        let backends = [
            None,
            Some(Backend::DdaCrop),
            Some(Backend::Wgc),
            Some(Backend::Hook),
        ];
        let all_allowed: [&[Backend]; 5] = [
            &[Backend::DdaCrop],
            &[Backend::Wgc],
            &[Backend::DdaCrop, Backend::Wgc],
            &[Backend::DdaCrop, Backend::Wgc, Backend::Hook],
            &[Backend::Hook],
        ];
        let mut n = 0;
        for build in builds {
            for borderless in bools {
                for installed in bools {
                    for enabled in bools {
                        for pref in backends {
                            for default in backends {
                                for allowed in all_allowed {
                                    for has_ac in bools {
                                        let mut g = open_game();
                                        g.allowed_backends = allowed.to_vec();
                                        g.default_backend = default;
                                        if has_ac {
                                            g.anticheat = vec![AntiCheat::Vac];
                                        }
                                        let mut p = UserPrefs::default();
                                        if let Some(b) = pref {
                                            p.per_game_backend.insert("open".into(), b);
                                        }
                                        if enabled {
                                            p.hook_enabled_games.insert("open".into());
                                        }
                                        let e = env(build, borderless, installed);
                                        let c = choose_backend(Some(&g), &e, &p);
                                        n += 1;
                                        assert!(!c.fallbacks.contains(&Backend::Hook), "{c:?}");
                                        assert!(!c.fallbacks.contains(&c.primary), "{c:?}");
                                        let mut dedup = c.fallbacks.clone();
                                        dedup.dedup();
                                        assert_eq!(dedup, c.fallbacks);
                                        assert!(!c.reason_pt.is_empty());
                                        if c.primary == Backend::Hook {
                                            // Every condition must hold.
                                            assert!(!has_ac && installed && enabled);
                                            assert!(allowed.contains(&Backend::Hook));
                                            // Hook must have been requested by the user or the DB.
                                            assert!(
                                                pref == Some(Backend::Hook)
                                                    || default == Some(Backend::Hook)
                                            );
                                        } else if allowed.iter().any(|b| *b != Backend::Hook) {
                                            // The primary is always an allowed non-hook backend.
                                            assert!(allowed.contains(&c.primary), "{c:?}");
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(n, 2 * 2 * 2 * 2 * 4 * 4 * 5 * 2);
    }

    #[test]
    fn inconsistent_entry_with_only_hook_still_yields_a_safe_choice() {
        let mut g = open_game();
        g.allowed_backends = vec![Backend::Hook];
        let c = choose_backend(Some(&g), &plain_env(WIN10, false), &prefs_hook("open"));
        assert_eq!(c.primary, Backend::DdaCrop);
        assert!(c.fallbacks.is_empty());
        // Even with a game that allows nothing at all.
        g.allowed_backends.clear();
        let c = choose_backend(Some(&g), &plain_env(WIN11, true), &UserPrefs::default());
        assert_eq!(c.primary, Backend::Wgc);
        assert!(c.fallbacks.is_empty());
    }

    // ---- against the embedded database ----

    #[test]
    fn embedded_games_behave_as_documented() {
        use Backend::{DdaCrop, Wgc};
        let db = GamesDb::embedded();
        let env10 = env(WIN10, false, true);

        // Anti-cheat game: hook can never be selected, even when the user asks for it.
        let cs2 = db.lookup(r"C:\x\cs2.exe").unwrap();
        let c = choose_backend(Some(cs2), &env10, &prefs_hook("cs2"));
        assert_choice(&c, DdaCrop, &[Wgc]);
        assert!(c.reason_pt.contains("hook bloqueado"), "{c:?}");

        // No anti-cheat game: hook only with the opt-in.
        let terraria = db.lookup("Terraria.exe").unwrap();
        let c = choose_backend(Some(terraria), &env10, &prefs_hook("terraria"));
        assert_eq!(c.primary, Backend::Hook);
        let c = choose_backend(Some(terraria), &env10, &UserPrefs::default());
        assert_eq!(c.primary, DdaCrop);

        // Every embedded game, every environment: never hook unless it is allowed AND opted in.
        for g in &db.games {
            for e in [env10, env(WIN11, true, true), env(WIN11, false, false)] {
                let none = choose_backend(Some(g), &e, &UserPrefs::default());
                assert_ne!(none.primary, Backend::Hook, "{}", g.id);
                let asked = choose_backend(Some(g), &e, &prefs_hook(&g.id));
                if asked.primary == Backend::Hook {
                    assert!(g.allows_hook() && e.hook_installed, "{}", g.id);
                }
                assert!(!asked.fallbacks.contains(&Backend::Hook));
            }
        }
    }

    #[test]
    fn helpers() {
        assert!(OsInfo { build: 22000 }.is_windows_11());
        assert!(!OsInfo { build: 21999 }.is_windows_11());
        let mut g = open_game();
        assert!(g.allows_hook());
        g.anticheat = vec![AntiCheat::None];
        assert!(g.allows_hook() && !g.has_anticheat());
        g.anticheat = vec![AntiCheat::Hyperion];
        assert!(!g.allows_hook() && g.has_anticheat());
    }
}
