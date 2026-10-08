//! Loading, validating, merging and querying a [`GamesDb`].

use std::collections::{HashMap, HashSet};

use crate::model::{Backend, DbError, GameEntry, GamesDb};
use crate::MAX_DB_BYTES;

/// The database shipped inside the binary.
const EMBEDDED_JSON: &str = include_str!("../games.json");

/// Case-folds a file name the way the database compares them (Unicode simple lowercase,
/// char by char, so that [`eq_folded`] and [`fold`] always agree).
fn fold(s: &str) -> String {
    s.chars().flat_map(char::to_lowercase).collect()
}

fn eq_folded(a: &str, b: &str) -> bool {
    a.chars()
        .flat_map(char::to_lowercase)
        .eq(b.chars().flat_map(char::to_lowercase))
}

/// The file name part of a path or bare name. Accepts `\` and `/`, and ignores surrounding
/// whitespace and double quotes (as found in raw command lines). Returns `""` for a path that
/// ends in a separator.
fn basename(path: &str) -> &str {
    let path = path.trim().trim_matches('"').trim();
    path.rsplit(['/', '\\']).next().unwrap_or("")
}

fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A bare file name (no path, no characters Windows forbids in names, no surrounding
/// whitespace) of the form `<stem>.exe` with a non-empty stem.
fn is_valid_exe_name(name: &str) -> bool {
    const STEM_SUFFIX: &str = ".exe";
    if name.is_empty() || name != name.trim() {
        return false;
    }
    if name.chars().any(|c| {
        c.is_control() || matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
    }) {
        return false;
    }
    let Some(split) = name.len().checked_sub(STEM_SUFFIX.len()) else {
        return false;
    };
    // `get` is false when `split` is not a char boundary, which also means "not ending in .exe".
    name.get(split..)
        .is_some_and(|ext| ext.eq_ignore_ascii_case(STEM_SUFFIX))
        && split > 0
}

fn validate_entry(game: &GameEntry) -> Result<(), DbError> {
    let id = &game.id;
    if !is_valid_id(id) {
        return Err(DbError::InvalidId { id: id.clone() });
    }
    if game.name.trim().is_empty() {
        return Err(DbError::EmptyName { id: id.clone() });
    }
    if game.exe_names.is_empty() {
        return Err(DbError::NoExeNames { id: id.clone() });
    }
    if let Some(exe) = game.exe_names.iter().find(|e| !is_valid_exe_name(e)) {
        return Err(DbError::InvalidExeName {
            id: id.clone(),
            exe: exe.clone(),
        });
    }
    if game.has_anticheat() && game.allowed_backends.contains(&Backend::Hook) {
        return Err(DbError::HookWithAntiCheat { id: id.clone() });
    }
    if game.kernel_anticheat && !game.has_anticheat() {
        return Err(DbError::KernelWithoutAntiCheat { id: id.clone() });
    }
    if let Some(default) = game.default_backend {
        if !game.allowed_backends.contains(&default) {
            return Err(DbError::DefaultNotAllowed { id: id.clone() });
        }
    }
    if !game.allowed_backends.iter().any(|b| *b != Backend::Hook) {
        return Err(DbError::NoNonHookBackend { id: id.clone() });
    }
    Ok(())
}

impl GamesDb {
    /// The database embedded in the binary (`games.json`). Its validity is checked by this
    /// crate's tests; if it were ever broken this returns an empty database (version 0) rather
    /// than panicking.
    pub fn embedded() -> Self {
        Self::from_json(EMBEDDED_JSON).unwrap_or(Self {
            version: 0,
            games: Vec::new(),
        })
    }

    /// Parses and validates a database, e.g. a remote update. Input larger than
    /// [`MAX_DB_BYTES`] (1 MiB) is rejected before parsing.
    pub fn from_json(s: &str) -> Result<Self, DbError> {
        if s.len() > MAX_DB_BYTES {
            return Err(DbError::TooLarge {
                len: s.len(),
                max: MAX_DB_BYTES,
            });
        }
        let db: Self = serde_json::from_str(s).map_err(|e| DbError::Parse(e.to_string()))?;
        db.validate()?;
        Ok(db)
    }

    /// Checks every rule of the SPEC: unique slug ids, `.exe` names unique across games
    /// (case-insensitive), hook only without anti-cheat, `kernel_anticheat` implies an
    /// anti-cheat, `default_backend` in `allowed_backends`, and at least one non-hook backend.
    pub fn validate(&self) -> Result<(), DbError> {
        let mut ids: HashSet<&str> = HashSet::new();
        // folded exe name -> id of the game that owns it
        let mut exes: HashMap<String, &str> = HashMap::new();
        for game in &self.games {
            validate_entry(game)?;
            if !ids.insert(game.id.as_str()) {
                return Err(DbError::DuplicateId {
                    id: game.id.clone(),
                });
            }
            for exe in &game.exe_names {
                if let Some(first) = exes.insert(fold(exe), game.id.as_str()) {
                    return Err(DbError::DuplicateExe {
                        exe: exe.clone(),
                        first: first.to_owned(),
                        second: game.id.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Merges a (remote) `update` into `base`.
    ///
    /// The database with the higher `version` wins for every id present in both; on a tie `base`
    /// wins (an update must be strictly newer to override). Ids present in only one side are
    /// kept. An entry of the losing side whose id or exe names collide with an entry that was
    /// already kept is dropped, so merging two valid databases always yields a valid one. The
    /// result carries the higher version and lists the winner's games first.
    pub fn merged(base: &Self, update: &Self) -> Self {
        let (winner, loser) = if update.version > base.version {
            (update, base)
        } else {
            (base, update)
        };
        let mut games = winner.games.clone();
        let mut ids: HashSet<String> = games.iter().map(|g| g.id.clone()).collect();
        let mut exes: HashSet<String> = games
            .iter()
            .flat_map(|g| g.exe_names.iter().map(|e| fold(e)))
            .collect();
        for game in &loser.games {
            if ids.contains(&game.id) || game.exe_names.iter().any(|e| exes.contains(&fold(e))) {
                continue;
            }
            ids.insert(game.id.clone());
            exes.extend(game.exe_names.iter().map(|e| fold(e)));
            games.push(game.clone());
        }
        Self {
            version: base.version.max(update.version),
            games,
        }
    }

    /// Finds the game whose `exe_names` contain the basename of `exe_path_or_name`
    /// (case-insensitive; `\` and `/` are both separators). Returns `None` for unknown
    /// executables and for paths without a file name.
    pub fn lookup(&self, exe_path_or_name: &str) -> Option<&GameEntry> {
        let name = basename(exe_path_or_name);
        if name.is_empty() {
            return None;
        }
        self.games
            .iter()
            .find(|g| g.exe_names.iter().any(|e| eq_folded(e, name)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AntiCheat;

    fn game(id: &str, exes: &[&str]) -> GameEntry {
        GameEntry {
            id: id.to_owned(),
            name: format!("Game {id}"),
            exe_names: exes.iter().map(|s| (*s).to_owned()).collect(),
            anticheat: vec![],
            kernel_anticheat: false,
            allowed_backends: vec![Backend::DdaCrop, Backend::Wgc],
            default_backend: None,
            notes_pt: String::new(),
            verified: false,
        }
    }

    fn db(version: u32, games: Vec<GameEntry>) -> GamesDb {
        GamesDb { version, games }
    }

    fn valid_db() -> GamesDb {
        db(1, vec![game("a", &["a.exe"]), game("b", &["b.exe"])])
    }

    // ---- embedded data ----

    #[test]
    fn embedded_parses_and_validates() {
        let db = GamesDb::embedded();
        assert!(db.version >= 1);
        assert!(db.games.len() >= 30, "got {}", db.games.len());
        assert_eq!(db.validate(), Ok(()));
        // The embedded JSON itself must go through the strict path, not the empty fallback.
        assert_eq!(GamesDb::from_json(EMBEDDED_JSON).unwrap(), db);
    }

    #[test]
    fn embedded_entries_follow_the_spec() {
        let db = GamesDb::embedded();
        for g in &db.games {
            assert!(!g.verified, "{} must be unverified", g.id);
            assert!(!g.notes_pt.is_empty(), "{} needs a note", g.id);
            assert!(!g.exe_names.is_empty(), "{} needs an exe", g.id);
            if g.allowed_backends.contains(&Backend::Hook) {
                assert!(!g.has_anticheat(), "{} lists hook but has anti-cheat", g.id);
            }
            if g.kernel_anticheat {
                assert!(g.has_anticheat(), "{}", g.id);
            }
        }
    }

    #[test]
    fn embedded_covers_the_required_games() {
        let db = GamesDb::embedded();
        let ids: HashSet<&str> = db.games.iter().map(|g| g.id.as_str()).collect();
        for id in [
            "cs2",
            "valorant",
            "league-of-legends",
            "dota2",
            "fortnite",
            "apex-legends",
            "rainbow-six-siege",
            "pubg",
            "call-of-duty",
            "marvel-rivals",
            "overwatch-2",
            "gta-v",
            "fivem",
            "rust",
            "elden-ring",
            "destiny-2",
            "rocket-league",
            "ea-fc",
            "roblox",
            "minecraft-java",
            "minecraft-bedrock",
            "fall-guys",
            "dead-by-daylight",
            "valheim",
            "terraria",
            "among-us",
            "lethal-company",
            "phasmophobia",
            "stardew-valley",
            "hollow-knight",
        ] {
            assert!(ids.contains(id), "missing {id}");
        }
    }

    #[test]
    fn embedded_anticheat_sanity() {
        let db = GamesDb::embedded();
        let get = |exe: &str| db.lookup(exe).unwrap();
        assert_eq!(
            get("valorant-win64-shipping.exe").anticheat,
            [AntiCheat::Vanguard]
        );
        assert!(get("valorant-win64-shipping.exe").kernel_anticheat);
        assert!(get("cs2.exe").anticheat.contains(&AntiCheat::Vac));
        assert_eq!(get("RobloxPlayerBeta.exe").anticheat, [AntiCheat::Hyperion]);
        assert_eq!(get("cod.exe").anticheat, [AntiCheat::Ricochet]);
        assert!(get("terraria.exe").allows_hook());
        assert!(get("javaw.exe").allows_hook());
        assert!(!get("cs2.exe").allows_hook());
        assert!(!get("Minecraft.Windows.exe").allows_hook());
    }

    // ---- lookup ----

    #[test]
    fn lookup_is_case_insensitive() {
        let db = GamesDb::embedded();
        for name in ["cs2.exe", "CS2.EXE", "Cs2.Exe"] {
            assert_eq!(
                db.lookup(name).map(|g| g.id.as_str()),
                Some("cs2"),
                "{name}"
            );
        }
        assert_eq!(
            db.lookup("LEAGUE OF LEGENDS.EXE").map(|g| g.id.as_str()),
            Some("league-of-legends")
        );
        assert_eq!(
            db.lookup("among us.exe").map(|g| g.id.as_str()),
            Some("among-us")
        );
    }

    #[test]
    fn lookup_handles_both_separators() {
        let db = GamesDb::embedded();
        for path in [
            r"C:\Program Files (x86)\Steam\steamapps\common\Counter-Strike Global Offensive\game\bin\win64\cs2.exe",
            "C:/Games/cs2.exe",
            r"C:\mixed/separators\sub/CS2.exe",
            r"\\?\C:\x\cs2.exe",
            r"\\server\share\cs2.exe",
            "/usr/bin/cs2.exe",
            r#""C:\Games\cs2.exe""#,
            "  cs2.exe  ",
        ] {
            assert_eq!(
                db.lookup(path).map(|g| g.id.as_str()),
                Some("cs2"),
                "{path}"
            );
        }
        assert_eq!(
            db.lookup(r"D:\Riot Games\League of Legends\Game\League of Legends.exe")
                .map(|g| g.id.as_str()),
            Some("league-of-legends")
        );
    }

    #[test]
    fn lookup_rejects_unknown_and_malformed() {
        let db = GamesDb::embedded();
        for name in [
            "",
            "   ",
            "notepad.exe",
            r"C:\Windows\notepad.exe",
            "cs2",
            "xcs2.exe",
            "cs2.exe.bak",
            "cs2.exe.exe",
            r"C:\Games\cs2.exe\",
            r"C:\cs2.exe\notepad.exe",
            "/",
            "\\",
            "\"",
            "\0",
        ] {
            assert!(db.lookup(name).is_none(), "{name:?}");
        }
    }

    #[test]
    fn lookup_unicode_names_fold_consistently() {
        let mut g = game("acao", &["Ação.exe"]);
        g.name = "Ação".into();
        let d = db(1, vec![g]);
        assert_eq!(d.validate(), Ok(()));
        assert!(d.lookup("AÇÃO.EXE").is_some());
        assert!(d.lookup(r"C:\x\ação.exe").is_some());
        assert!(d.lookup("acao.exe").is_none());
    }

    #[test]
    fn lookup_on_empty_db() {
        let d = db(0, vec![]);
        assert!(d.lookup("cs2.exe").is_none());
    }

    // ---- validation ----

    #[test]
    fn validate_accepts_a_valid_db() {
        assert_eq!(valid_db().validate(), Ok(()));
        assert_eq!(db(0, vec![]).validate(), Ok(()));
    }

    #[test]
    fn validate_rejects_bad_ids() {
        for id in ["", "CS2", "cs_2", "cs 2", "cs2!", "ção", "a/b", "a.b"] {
            let d = db(1, vec![game(id, &["x.exe"])]);
            assert_eq!(
                d.validate(),
                Err(DbError::InvalidId { id: id.to_owned() }),
                "{id:?}"
            );
        }
        for id in ["cs2", "a", "0", "rainbow-six-siege", "-", "a--b"] {
            let d = db(1, vec![game(id, &["x.exe"])]);
            assert_eq!(d.validate(), Ok(()), "{id:?}");
        }
    }

    #[test]
    fn validate_rejects_duplicate_ids() {
        let d = db(1, vec![game("a", &["a.exe"]), game("a", &["b.exe"])]);
        assert_eq!(d.validate(), Err(DbError::DuplicateId { id: "a".into() }));
    }

    #[test]
    fn validate_rejects_empty_name() {
        let mut g = game("a", &["a.exe"]);
        g.name = "  ".into();
        assert_eq!(
            db(1, vec![g]).validate(),
            Err(DbError::EmptyName { id: "a".into() })
        );
    }

    #[test]
    fn validate_rejects_missing_or_bad_exe_names() {
        assert_eq!(
            db(1, vec![game("a", &[])]).validate(),
            Err(DbError::NoExeNames { id: "a".into() })
        );
        for exe in [
            "",
            " ",
            ".exe",
            " .exe",
            "a",
            "a.dll",
            "a.exe ",
            " a.exe",
            "a.exe.txt",
            "a.com",
            r"dir\a.exe",
            "dir/a.exe",
            "a:b.exe",
            "a*.exe",
            "a?.exe",
            "a\"b.exe",
            "a<b.exe",
            "a|b.exe",
            "a\0.exe",
            "a\n.exe",
            "é",
            "aé",
            "é.ex",
        ] {
            assert_eq!(
                db(1, vec![game("a", &[exe])]).validate(),
                Err(DbError::InvalidExeName {
                    id: "a".into(),
                    exe: exe.to_owned()
                }),
                "{exe:?}"
            );
        }
        // One bad name among good ones is still rejected.
        assert!(db(1, vec![game("a", &["ok.exe", "bad"])])
            .validate()
            .is_err());
        // Accepted spellings.
        for exe in ["a.exe", "A.EXE", "Among Us.exe", "a.b.exe", "x-y_z.Exe"] {
            assert_eq!(db(1, vec![game("a", &[exe])]).validate(), Ok(()), "{exe:?}");
        }
    }

    #[test]
    fn validate_rejects_duplicate_exes_case_insensitively() {
        let d = db(
            1,
            vec![game("a", &["Shared.exe"]), game("b", &["shared.EXE"])],
        );
        assert_eq!(
            d.validate(),
            Err(DbError::DuplicateExe {
                exe: "shared.EXE".into(),
                first: "a".into(),
                second: "b".into()
            })
        );
        // Also inside one game.
        let d = db(1, vec![game("a", &["x.exe", "X.exe"])]);
        assert!(matches!(d.validate(), Err(DbError::DuplicateExe { .. })));
    }

    #[test]
    fn validate_hook_rules() {
        // hook + real anti-cheat => rejected, for each anti-cheat.
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
            let mut g = game("a", &["a.exe"]);
            g.allowed_backends = vec![Backend::DdaCrop, Backend::Hook];
            g.anticheat = vec![ac];
            assert_eq!(
                db(1, vec![g.clone()]).validate(),
                Err(DbError::HookWithAntiCheat { id: "a".into() }),
                "{ac:?}"
            );
            // A `None` next to a real anti-cheat does not make it hook-safe.
            g.anticheat = vec![AntiCheat::None, ac];
            assert!(db(1, vec![g]).validate().is_err(), "{ac:?} + none");
        }
        // hook with no anti-cheat: both spellings are fine.
        for acs in [vec![], vec![AntiCheat::None]] {
            let mut g = game("a", &["a.exe"]);
            g.allowed_backends = vec![Backend::Wgc, Backend::Hook];
            g.anticheat = acs;
            assert_eq!(db(1, vec![g]).validate(), Ok(()));
        }
        // anti-cheat without hook is fine.
        let mut g = game("a", &["a.exe"]);
        g.anticheat = vec![AntiCheat::Vanguard];
        g.kernel_anticheat = true;
        assert_eq!(db(1, vec![g]).validate(), Ok(()));
    }

    #[test]
    fn validate_kernel_requires_anticheat() {
        for acs in [vec![], vec![AntiCheat::None]] {
            let mut g = game("a", &["a.exe"]);
            g.kernel_anticheat = true;
            g.anticheat = acs;
            assert_eq!(
                db(1, vec![g]).validate(),
                Err(DbError::KernelWithoutAntiCheat { id: "a".into() })
            );
        }
    }

    #[test]
    fn validate_default_backend_must_be_allowed() {
        let mut g = game("a", &["a.exe"]);
        g.allowed_backends = vec![Backend::Wgc];
        g.default_backend = Some(Backend::DdaCrop);
        assert_eq!(
            db(1, vec![g.clone()]).validate(),
            Err(DbError::DefaultNotAllowed { id: "a".into() })
        );
        g.default_backend = Some(Backend::Wgc);
        assert_eq!(db(1, vec![g.clone()]).validate(), Ok(()));
        g.default_backend = Some(Backend::Hook);
        assert!(db(1, vec![g]).validate().is_err());
    }

    #[test]
    fn validate_needs_a_non_hook_backend() {
        for allowed in [
            vec![],
            vec![Backend::Hook],
            vec![Backend::Hook, Backend::Hook],
        ] {
            let mut g = game("a", &["a.exe"]);
            g.allowed_backends = allowed;
            assert_eq!(
                db(1, vec![g]).validate(),
                Err(DbError::NoNonHookBackend { id: "a".into() })
            );
        }
    }

    // ---- from_json ----

    fn json_of(d: &GamesDb) -> String {
        serde_json::to_string(d).unwrap()
    }

    #[test]
    fn from_json_roundtrip() {
        let d = GamesDb::embedded();
        assert_eq!(GamesDb::from_json(&json_of(&d)).unwrap(), d);
    }

    #[test]
    fn from_json_rejects_garbage() {
        for s in [
            "",
            "null",
            "[]",
            "{",
            "{\"version\":1}",
            "{\"version\":-1,\"games\":[]}",
            "\u{0}",
        ] {
            assert!(
                matches!(GamesDb::from_json(s), Err(DbError::Parse(_))),
                "{s:?}"
            );
        }
    }

    #[test]
    fn from_json_validates() {
        let d = db(1, vec![game("Bad Id", &["a.exe"])]);
        assert!(matches!(
            GamesDb::from_json(&json_of(&d)),
            Err(DbError::InvalidId { .. })
        ));
    }

    #[test]
    fn from_json_enforces_size_limit() {
        let ok = json_of(&valid_db());
        // Pad with whitespace up to exactly the limit: accepted.
        let at_limit = format!("{ok}{}", " ".repeat(MAX_DB_BYTES - ok.len()));
        assert_eq!(at_limit.len(), MAX_DB_BYTES);
        assert_eq!(GamesDb::from_json(&at_limit).unwrap(), valid_db());
        // One byte more: rejected before parsing.
        let over = format!("{at_limit} ");
        assert_eq!(
            GamesDb::from_json(&over),
            Err(DbError::TooLarge {
                len: MAX_DB_BYTES + 1,
                max: MAX_DB_BYTES
            })
        );
    }

    #[test]
    fn from_json_deeply_nested_input_does_not_panic() {
        let s = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
        assert!(matches!(GamesDb::from_json(&s), Err(DbError::Parse(_))));
    }

    #[test]
    fn json_anticheat_spellings() {
        let raw = |ac: &str| {
            format!(
                r#"{{"version":1,"games":[{{"id":"a","name":"A","exe_names":["a.exe"],
                "anticheat":["{ac}"],"kernel_anticheat":false,
                "allowed_backends":["dda_crop"],"default_backend":null,
                "notes_pt":"","verified":false}}]}}"#
            )
        };
        let ac_of = |s: String| GamesDb::from_json(&s).unwrap().games[0].anticheat[0];
        assert_eq!(ac_of(raw("battleye")), AntiCheat::BattlEye);
        assert_eq!(ac_of(raw("battl_eye")), AntiCheat::BattlEye);
        assert_eq!(ac_of(raw("gamers_club")), AntiCheat::GamersClub);
        assert_eq!(ac_of(raw("none")), AntiCheat::None);
        // Unknown name => Other (still an anti-cheat).
        assert_eq!(ac_of(raw("some_future_ac")), AntiCheat::Other);
        // Canonical output spelling.
        assert_eq!(
            serde_json::to_string(&AntiCheat::BattlEye).unwrap(),
            "\"battleye\""
        );
        assert_eq!(
            serde_json::to_string(&AntiCheat::Other).unwrap(),
            "\"other\""
        );
    }

    #[test]
    fn json_unknown_backend_and_missing_optionals() {
        let bad = r#"{"version":1,"games":[{"id":"a","name":"A","exe_names":["a.exe"],
            "anticheat":[],"kernel_anticheat":false,"allowed_backends":["quantum"]}]}"#;
        assert!(matches!(GamesDb::from_json(bad), Err(DbError::Parse(_))));
        // notes_pt / verified / default_backend may be omitted; unknown fields are ignored.
        let ok = r#"{"version":1,"future":true,"games":[{"id":"a","name":"A","exe_names":["a.exe"],
            "anticheat":[],"kernel_anticheat":false,"allowed_backends":["wgc"],"extra":1}]}"#;
        let d = GamesDb::from_json(ok).unwrap();
        assert!(!d.games[0].verified);
        assert_eq!(d.games[0].default_backend, None);
        assert_eq!(d.games[0].notes_pt, "");
        // `anticheat` is mandatory: an entry that does not say is not assumed to be clean.
        let no_ac = r#"{"version":1,"games":[{"id":"a","name":"A","exe_names":["a.exe"],
            "kernel_anticheat":false,"allowed_backends":["wgc"]}]}"#;
        assert!(GamesDb::from_json(no_ac).is_err());
    }

    // ---- merged ----

    fn named(id: &str, exes: &[&str], name: &str) -> GameEntry {
        let mut g = game(id, exes);
        g.name = name.to_owned();
        g
    }

    fn names(d: &GamesDb) -> Vec<(&str, &str)> {
        d.games
            .iter()
            .map(|g| (g.id.as_str(), g.name.as_str()))
            .collect()
    }

    #[test]
    fn merged_higher_update_wins_per_id_and_adds_unknown_ids() {
        let base = db(
            1,
            vec![
                named("a", &["a.exe"], "old a"),
                named("b", &["b.exe"], "old b"),
            ],
        );
        let update = db(
            2,
            vec![
                named("b", &["b.exe"], "new b"),
                named("c", &["c.exe"], "new c"),
            ],
        );
        let m = GamesDb::merged(&base, &update);
        assert_eq!(m.version, 2);
        assert_eq!(m.validate(), Ok(()));
        let mut got = names(&m);
        got.sort_unstable();
        assert_eq!(got, [("a", "old a"), ("b", "new b"), ("c", "new c")]);
    }

    #[test]
    fn merged_lower_update_only_adds_unknown_ids() {
        let base = db(5, vec![named("a", &["a.exe"], "base a")]);
        let update = db(
            3,
            vec![
                named("a", &["a.exe"], "stale a"),
                named("c", &["c.exe"], "new c"),
            ],
        );
        let m = GamesDb::merged(&base, &update);
        assert_eq!(m.version, 5);
        assert_eq!(names(&m), [("a", "base a"), ("c", "new c")]);
        assert_eq!(m.validate(), Ok(()));
    }

    #[test]
    fn merged_tie_keeps_base() {
        let base = db(4, vec![named("a", &["a.exe"], "base a")]);
        let update = db(4, vec![named("a", &["a.exe"], "update a")]);
        let m = GamesDb::merged(&base, &update);
        assert_eq!(m.version, 4);
        assert_eq!(names(&m), [("a", "base a")]);
    }

    #[test]
    fn merged_update_can_change_exe_names_of_an_id() {
        let base = db(1, vec![named("a", &["old.exe"], "a")]);
        let update = db(2, vec![named("a", &["new.exe"], "a")]);
        let m = GamesDb::merged(&base, &update);
        assert!(m.lookup("new.exe").is_some());
        assert!(m.lookup("old.exe").is_none());
    }

    #[test]
    fn merged_drops_losing_entries_that_collide_on_exe_names() {
        // Newer update adds "z" claiming base's "a.exe": the newer side wins, base's "a" is dropped.
        let base = db(
            1,
            vec![named("a", &["a.exe"], "a"), named("b", &["b.exe"], "b")],
        );
        let update = db(2, vec![named("z", &["A.EXE"], "z")]);
        let m = GamesDb::merged(&base, &update);
        assert_eq!(names(&m), [("z", "z"), ("b", "b")]);
        assert_eq!(m.validate(), Ok(()));

        // Stale update adds "z" claiming base's "a.exe": base wins, "z" is dropped.
        let base = db(3, vec![named("a", &["a.exe"], "a")]);
        let update = db(
            2,
            vec![named("z", &["a.exe"], "z"), named("y", &["y.exe"], "y")],
        );
        let m = GamesDb::merged(&base, &update);
        assert_eq!(names(&m), [("a", "a"), ("y", "y")]);
        assert_eq!(m.validate(), Ok(()));
    }

    #[test]
    fn merged_with_empty_sides() {
        let a = valid_db();
        let empty = db(0, vec![]);
        assert_eq!(GamesDb::merged(&a, &empty), a);
        assert_eq!(GamesDb::merged(&empty, &a), db(1, a.games.clone()));
        assert_eq!(GamesDb::merged(&empty, &empty), empty);
    }

    #[test]
    fn merged_embedded_with_itself_is_identity() {
        let e = GamesDb::embedded();
        assert_eq!(GamesDb::merged(&e, &e), e);
    }
}
