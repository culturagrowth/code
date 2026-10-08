//! Portable process-tree logic over a process snapshot (on Windows, `CreateToolhelp32Snapshot`).
//!
//! Process loopback with `PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE` captures a process
//! and all of its descendants, so for Discord we need the **root** process of each running
//! flavor: the main `Discord.exe` spawns the GPU/renderer/utility/voice children, and the voice
//! output is rendered by one of them.
//!
//! Snapshots are inherently racy (processes exit, PIDs get reused, and a reused PID can even
//! make the parent links form a cycle), so every walk here is bounded and cycle-safe.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

/// One entry of a process snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcInfo {
    /// Process id.
    pub pid: u32,
    /// Parent process id at creation time (the parent may have exited, and its PID may have been
    /// reused since).
    pub parent_pid: u32,
    /// Executable file name (e.g. `Discord.exe`). A full path is tolerated: only the file name is
    /// compared.
    pub exe: String,
}

/// Executable names of the Discord flavors (stable, PTB, Canary). Matched case-insensitively.
pub const DISCORD_EXES: &[&str] = &["Discord.exe", "DiscordPTB.exe", "DiscordCanary.exe"];

/// The canonical Discord flavor name (an entry of [`DISCORD_EXES`]) of `exe`, if it is one.
/// Case-insensitive; only the file name part of a path is considered.
pub fn discord_flavor(exe: &str) -> Option<&'static str> {
    let name = file_name(exe);
    DISCORD_EXES
        .iter()
        .copied()
        .find(|flavor| flavor.eq_ignore_ascii_case(name))
}

/// Root Discord processes: a Discord exe whose parent is not a Discord exe of the same flavor
/// (or is absent from the snapshot: an orphan whose parent exited becomes a root),
/// case-insensitive. PTB and Canary running next to stable each get their own root.
///
/// If PID reuse makes the same-flavor parent links form a cycle, the cycle has no natural root;
/// its smallest PID is reported so that those processes are still covered. Every Discord process
/// therefore belongs to the tree of exactly one returned root. The result is sorted ascending and
/// deduplicated; it runs in `O(n)` and terminates on any input.
pub fn discord_roots(procs: &[ProcInfo]) -> Vec<u32> {
    let flavors: Vec<Option<&'static str>> = procs.iter().map(|p| discord_flavor(&p.exe)).collect();
    let index = pid_index(procs);

    // The same-flavor Discord parent of entry `i`, if any.
    let parent_of = |i: usize| -> Option<usize> {
        let flavor = flavors[i]?;
        let parent = *index.get(&procs[i].parent_pid)?;
        (flavors[parent] == Some(flavor)).then_some(parent)
    };

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Mark {
        Unvisited,
        OnPath,
        Done(usize),
    }
    let mut marks = vec![Mark::Unvisited; procs.len()];
    let mut roots = BTreeSet::new();
    let mut path: Vec<usize> = Vec::new();

    for start in 0..procs.len() {
        if flavors[start].is_none() || marks[start] != Mark::Unvisited {
            continue;
        }
        path.clear();
        let mut cur = start;
        let root = loop {
            match marks[cur] {
                Mark::Done(root) => break root,
                Mark::OnPath => {
                    // A cycle: `cur` is on the current path; the cycle is path[pos..].
                    let pos = path.iter().position(|&i| i == cur).unwrap_or(0);
                    break path[pos..]
                        .iter()
                        .copied()
                        .min_by_key(|&i| procs[i].pid)
                        .unwrap_or(cur);
                }
                Mark::Unvisited => {
                    marks[cur] = Mark::OnPath;
                    path.push(cur);
                    match parent_of(cur) {
                        Some(parent) => cur = parent,
                        None => break cur,
                    }
                }
            }
        };
        for &i in &path {
            marks[i] = Mark::Done(root);
        }
        roots.insert(procs[root].pid);
    }
    roots.into_iter().collect()
}

/// The process tree rooted at `root`, for diagnostics: `root` first, then its descendants in
/// breadth-first order (children in snapshot order). Each PID appears once, so cycles created by
/// PID reuse terminate; a process listing itself as its parent is not its own child. Returns an
/// empty vector when `root` is not in the snapshot.
pub fn descendants(procs: &[ProcInfo], root: u32) -> Vec<u32> {
    if !procs.iter().any(|p| p.pid == root) {
        return Vec::new();
    }
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for p in procs.iter().filter(|p| p.pid != p.parent_pid) {
        children.entry(p.parent_pid).or_default().push(p.pid);
    }
    let mut seen = HashSet::from([root]);
    let mut queue = VecDeque::from([root]);
    let mut out = Vec::new();
    while let Some(pid) = queue.pop_front() {
        out.push(pid);
        for &child in children.get(&pid).map(Vec::as_slice).unwrap_or_default() {
            if seen.insert(child) {
                queue.push_back(child);
            }
        }
    }
    out
}

/// PID → index of its first entry (a well-formed snapshot has unique PIDs; duplicates are
/// tolerated by keeping the first).
fn pid_index(procs: &[ProcInfo]) -> HashMap<u32, usize> {
    let mut index = HashMap::with_capacity(procs.len());
    for (i, p) in procs.iter().enumerate() {
        index.entry(p.pid).or_insert(i);
    }
    index
}

/// The file-name part of a Windows or POSIX path.
fn file_name(exe: &str) -> &str {
    exe.rsplit(['\\', '/']).next().unwrap_or(exe)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, parent_pid: u32, exe: &str) -> ProcInfo {
        ProcInfo {
            pid,
            parent_pid,
            exe: exe.to_owned(),
        }
    }

    /// A typical desktop: explorer launches Squirrel's Update.exe, which launches the main
    /// Discord.exe, which spawns the GPU, renderer, utility and crashpad children.
    fn normal_tree() -> Vec<ProcInfo> {
        vec![
            p(0, 0, "[System Process]"),
            p(4, 0, "System"),
            p(800, 4, "wininit.exe"),
            p(1200, 800, "explorer.exe"),
            p(3000, 1200, "Update.exe"),
            p(3100, 3000, "Discord.exe"),
            p(3110, 3100, "Discord.exe"),
            p(3120, 3100, "Discord.exe"),
            p(3130, 3100, "Discord.exe"),
            p(3140, 3120, "Discord.exe"),
            p(3150, 3100, "crashpad_handler.exe"),
            p(5000, 1200, "game.exe"),
        ]
    }

    #[test]
    fn flavor_matching() {
        assert_eq!(discord_flavor("Discord.exe"), Some("Discord.exe"));
        assert_eq!(discord_flavor("discordptb.EXE"), Some("DiscordPTB.exe"));
        assert_eq!(
            discord_flavor(r"C:\Users\a\AppData\Local\DiscordCanary\app-1.0\DiscordCanary.exe"),
            Some("DiscordCanary.exe")
        );
        assert_eq!(
            discord_flavor("/opt/discord/Discord.exe"),
            Some("Discord.exe")
        );
        assert_eq!(discord_flavor("Discord"), None);
        assert_eq!(discord_flavor("NotDiscord.exe"), None);
        assert_eq!(discord_flavor("Discord.exe.bak"), None);
        assert_eq!(discord_flavor(""), None);
    }

    #[test]
    fn roots_normal_tree() {
        assert_eq!(discord_roots(&normal_tree()), vec![3100]);
    }

    #[test]
    fn roots_ptb_and_canary_together() {
        let mut procs = normal_tree();
        procs.extend([
            p(6000, 1200, "DiscordPTB.exe"),
            p(6010, 6000, "DiscordPTB.exe"),
            p(6020, 6000, "DiscordPTB.exe"),
            p(7000, 1200, "DiscordCanary.exe"),
            p(7010, 7000, "DiscordCanary.exe"),
            // A Canary process whose parent is a PTB process: different flavor, so a root.
            p(7100, 6010, "DiscordCanary.exe"),
        ]);
        assert_eq!(discord_roots(&procs), vec![3100, 6000, 7000, 7100]);
    }

    #[test]
    fn roots_case_variations() {
        let procs = vec![
            p(10, 1, "explorer.exe"),
            p(20, 10, "DISCORD.EXE"),
            p(21, 20, "discord.exe"),
            p(22, 21, "Discord.Exe"),
            p(30, 10, "discordptb.exe"),
            p(31, 30, "DiscordPtb.EXE"),
            p(40, 10, r"C:\Apps\DiscordCanary\DISCORDCANARY.exe"),
        ];
        assert_eq!(discord_roots(&procs), vec![20, 30, 40]);
    }

    #[test]
    fn roots_orphaned_child_becomes_root() {
        // The main process (3100) exited; its children keep parent_pid = 3100.
        let procs: Vec<ProcInfo> = normal_tree()
            .into_iter()
            .filter(|p| p.pid != 3100)
            .collect();
        // 3110, 3120 and 3130 are orphans (roots); 3140's parent 3120 is still a Discord.exe.
        assert_eq!(discord_roots(&procs), vec![3110, 3120, 3130]);
    }

    #[test]
    fn roots_cycle_terminates_and_is_covered() {
        // PID reuse: 100's parent PID (200) was reused by a process that 100 itself created.
        let procs = vec![
            p(1, 0, "explorer.exe"),
            p(200, 100, "Discord.exe"),
            p(100, 200, "Discord.exe"),
            p(300, 100, "Discord.exe"),
            // A longer cycle, entered from a tail ("rho" shape).
            p(500, 502, "DiscordPTB.exe"),
            p(501, 500, "DiscordPTB.exe"),
            p(502, 501, "DiscordPTB.exe"),
            p(510, 501, "DiscordPTB.exe"),
            // Self-parented (pid == parent_pid).
            p(900, 900, "DiscordCanary.exe"),
        ];
        assert_eq!(discord_roots(&procs), vec![100, 500, 900]);
    }

    #[test]
    fn roots_pid_reuse_and_duplicates_terminate() {
        // Duplicate PIDs (malformed snapshot) and a parent PID reused by a non-Discord process.
        let procs = vec![
            p(50, 40, "Discord.exe"),
            p(50, 50, "Discord.exe"),
            p(60, 50, "Discord.exe"),
            p(40, 60, "notepad.exe"),
            p(70, 70, "notepad.exe"),
        ];
        assert_eq!(discord_roots(&procs), vec![50]);
    }

    #[test]
    fn roots_long_chain_is_linear() {
        // 50k-deep same-flavor chain: must not blow the stack or go quadratic.
        let n = 50_000u32;
        let mut procs: Vec<ProcInfo> = (1..=n).map(|i| p(i, i - 1, "Discord.exe")).collect();
        procs.reverse();
        procs.push(p(0, 0, "Update.exe"));
        assert_eq!(discord_roots(&procs), vec![1]);
    }

    #[test]
    fn roots_no_discord() {
        let procs: Vec<ProcInfo> = normal_tree()
            .into_iter()
            .filter(|p| discord_flavor(&p.exe).is_none())
            .collect();
        assert!(discord_roots(&procs).is_empty());
        assert!(discord_roots(&[]).is_empty());
    }

    #[test]
    fn descendants_tree_walk() {
        let procs = normal_tree();
        assert_eq!(
            descendants(&procs, 3100),
            vec![3100, 3110, 3120, 3130, 3150, 3140]
        );
        assert_eq!(descendants(&procs, 5000), vec![5000]);
        assert_eq!(descendants(&procs, 3000)[..2], [3000, 3100]);
        // Pid 0 lists itself as parent but is not its own child.
        let all = descendants(&procs, 0);
        assert_eq!(all.len(), procs.len());
        assert_eq!(all[0], 0);
    }

    #[test]
    fn descendants_missing_root_is_empty() {
        assert!(descendants(&normal_tree(), 4242).is_empty());
        assert!(descendants(&[], 0).is_empty());
    }

    #[test]
    fn descendants_cycle_safe() {
        let procs = vec![
            p(100, 200, "a.exe"),
            p(200, 100, "b.exe"),
            p(300, 200, "c.exe"),
            p(400, 400, "d.exe"),
            p(100, 300, "dup.exe"),
        ];
        assert_eq!(descendants(&procs, 100), vec![100, 200, 300]);
        assert_eq!(descendants(&procs, 300), vec![300, 100, 200]);
        assert_eq!(descendants(&procs, 400), vec![400]);
    }
}
