//! Clip file names: `DuoClip_AAAA-MM-DD_HH-MM-SS_<jogo>.mp4` (local wall time, used only for
//! the name), with the game name sanitized for Windows file names.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

/// Local wall-clock time (only for file names; recording uses QPC).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalTime {
    /// Year, e.g. 2026.
    pub year: u16,
    /// 1..=12.
    pub month: u16,
    /// 1..=31.
    pub day: u16,
    /// 0..=23.
    pub hour: u16,
    /// 0..=59.
    pub minute: u16,
    /// 0..=59.
    pub second: u16,
}

/// Longest sanitized game name (characters).
pub const MAX_GAME_NAME: usize = 40;

/// Keeps letters and digits (any script), turns everything else into `-`, collapses repeats,
/// trims, caps at [`MAX_GAME_NAME`] characters; empty → `"jogo"`. The result never contains
/// characters Windows forbids in file names, spaces or dots.
pub fn sanitize_game_name(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_alphanumeric() {
            if out.chars().count() >= MAX_GAME_NAME {
                break;
            }
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-');
    if out.is_empty() {
        "jogo".to_string()
    } else {
        out.to_string()
    }
}

/// `DuoClip_2026-10-08_21-05-09_Counter-Strike-2.mp4`.
pub fn clip_file_name(t: &LocalTime, game: &str) -> String {
    format!(
        "DuoClip_{:04}-{:02}-{:02}_{:02}-{:02}-{:02}_{}.mp4",
        t.year,
        t.month,
        t.day,
        t.hour,
        t.minute,
        t.second,
        sanitize_game_name(game)
    )
}

/// `dir/name`, or `dir/<stem>_2.mp4`, `_3`... when `exists` says the name is taken.
pub fn unique_path(dir: &Path, name: &str, exists: impl Fn(&Path) -> bool) -> PathBuf {
    let first = dir.join(name);
    if !exists(&first) {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s, format!(".{e}")),
        None => (name, String::new()),
    };
    for n in 2..10_000u32 {
        let p = dir.join(format!("{stem}_{n}{ext}"));
        if !exists(&p) {
            return p;
        }
    }
    dir.join(format!("{stem}_{}{ext}", u32::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizing() {
        assert_eq!(sanitize_game_name("Counter-Strike 2"), "Counter-Strike-2");
        assert_eq!(
            sanitize_game_name("  Tom Clancy's Rainbow Six® Siege "),
            "Tom-Clancy-s-Rainbow-Six-Siege"
        );
        assert_eq!(
            sanitize_game_name(r#"a<b>c:d"e/f\g|h?i*j"#),
            "a-b-c-d-e-f-g-h-i-j"
        );
        assert_eq!(sanitize_game_name("javaw.exe"), "javaw-exe");
        assert_eq!(sanitize_game_name("Pokémon ação"), "Pokémon-ação");
        assert_eq!(sanitize_game_name(""), "jogo");
        assert_eq!(sanitize_game_name("..."), "jogo");
        assert_eq!(sanitize_game_name("\u{0}\u{7}\n"), "jogo");
        assert_eq!(sanitize_game_name("CON"), "CON"); // reserved only as a whole name; ours has a prefix
        let long = sanitize_game_name(&"é".repeat(500));
        assert_eq!(long.chars().count(), MAX_GAME_NAME);
        let s = sanitize_game_name("a — b — c");
        assert_eq!(s, "a-b-c");
    }

    #[test]
    fn file_names() {
        let t = LocalTime {
            year: 2026,
            month: 10,
            day: 8,
            hour: 21,
            minute: 5,
            second: 9,
        };
        assert_eq!(
            clip_file_name(&t, "Counter-Strike 2"),
            "DuoClip_2026-10-08_21-05-09_Counter-Strike-2.mp4"
        );
        let name = clip_file_name(&t, "x/y");
        assert!(!name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|', ' ']));
    }

    #[test]
    fn unique_paths() {
        let dir = Path::new("clips");
        let taken = [dir.join("a.mp4"), dir.join("a_2.mp4")];
        let p = unique_path(dir, "a.mp4", |p| taken.iter().any(|t| t == p));
        assert_eq!(p, dir.join("a_3.mp4"));
        assert_eq!(unique_path(dir, "b.mp4", |_| false), dir.join("b.mp4"));
        assert_eq!(
            unique_path(dir, "noext", |p| p == dir.join("noext")),
            dir.join("noext_2")
        );
        // Everything taken: still returns a path, never loops forever.
        let _ = unique_path(dir, "c.mp4", |_| true);
    }
}
