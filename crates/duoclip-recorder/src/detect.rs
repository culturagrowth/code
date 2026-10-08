//! Which foreground process is a game, and which Discord processes to record (portable logic;
//! the Windows side feeds it the foreground process image path and a process snapshot).

#![forbid(unsafe_code)]

use duoclip_audio::{discord_roots, ProcInfo};
use duoclip_gamesdb::{GameEntry, GamesDb};

/// A detected game.
#[derive(Clone, Debug, PartialEq)]
pub struct GameMatch {
    /// Display name (games database name, or the configured exe without `.exe`).
    pub name: String,
    /// The database entry, when the exe is in the games database.
    pub entry: Option<GameEntry>,
}

/// File name part of a Windows or POSIX path.
pub fn basename(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// Matches a process image path against the games database and the configured `jogo.exe`
/// (case-insensitive file name). The database entry wins for the name when both match.
pub fn match_game(db: &GamesDb, extra_exe: Option<&str>, image_path: &str) -> Option<GameMatch> {
    let exe = basename(image_path.trim());
    if exe.is_empty() {
        return None;
    }
    if let Some(entry) = db.lookup(exe) {
        return Some(GameMatch {
            name: entry.name.clone(),
            entry: Some(entry.clone()),
        });
    }
    let extra = extra_exe?.trim();
    if !extra.is_empty() && extra.eq_ignore_ascii_case(exe) {
        // The last 4 bytes are ASCII ".exe", so `len - 4` is a char boundary.
        let stem = if exe.len() > 4 && exe.to_ascii_lowercase().ends_with(".exe") {
            &exe[..exe.len() - 4]
        } else {
            exe
        };
        return Some(GameMatch {
            name: stem.to_string(),
            entry: None,
        });
    }
    None
}

/// Root Discord processes to record: `discord_roots` minus the ignored exe names
/// (case-insensitive). Returns `(pid, exe)`.
pub fn discord_targets(procs: &[ProcInfo], ignore: &[String]) -> Vec<(u32, String)> {
    discord_roots(procs)
        .into_iter()
        .filter_map(|pid| procs.iter().find(|p| p.pid == pid))
        .filter(|p| {
            !ignore
                .iter()
                .any(|i| i.trim().eq_ignore_ascii_case(basename(&p.exe)))
        })
        .map(|p| (p.pid, basename(&p.exe).to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn games_from_db_and_config() {
        let db = GamesDb::embedded();
        let m = match_game(&db, None, r"C:\Steam\common\cs2\game\bin\win64\CS2.EXE").unwrap();
        assert_eq!(m.name, "Counter-Strike 2");
        assert_eq!(m.entry.unwrap().id, "cs2");
        assert!(match_game(&db, None, r"C:\Windows\explorer.exe").is_none());
        let m = match_game(&db, Some("MeuJogo.exe"), r"D:\x\meujogo.EXE").unwrap();
        assert_eq!(m.name, "meujogo");
        assert!(m.entry.is_none());
        // javaw.exe is in the database (Minecraft Java): the database name wins.
        let m = match_game(&db, Some("javaw.exe"), r"C:\jdk\bin\javaw.exe").unwrap();
        assert!(m.entry.is_some());
        assert!(match_game(&db, Some(""), "").is_none());
        assert!(match_game(&db, Some(".exe"), r"C:\x\.exe").is_some());
        let _ = match_game(&db, Some("é.exe"), "é.exe");
        let _ = match_game(&db, Some("ab"), "ab");
    }

    fn p(pid: u32, parent: u32, exe: &str) -> ProcInfo {
        ProcInfo {
            pid,
            parent_pid: parent,
            exe: exe.into(),
        }
    }

    #[test]
    fn discord_with_ignore_list() {
        let procs = vec![
            p(10, 1, "Discord.exe"),
            p(11, 10, "Discord.exe"),
            p(20, 1, "DiscordCanary.exe"),
            p(21, 20, "DiscordCanary.exe"),
            p(30, 1, "DiscordPTB.exe"),
        ];
        let all = discord_targets(&procs, &[]);
        assert_eq!(all.len(), 3);
        let some = discord_targets(
            &procs,
            &["discordcanary.exe ".into(), "DiscordPTB.exe".into()],
        );
        assert_eq!(some, vec![(10, "Discord.exe".to_string())]);
        assert!(discord_targets(&[], &[]).is_empty());
    }
}
