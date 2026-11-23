//! The keeper's own view of its tree on Linux, from `/proc`, without `ps`.
//!
//! The keeper is a child subreaper, so every live descendant of the program
//! is reached from the keeper by parent links. Children are read from
//! `/proc/<pid>/task/<tid>/children` (every thread of the parent), or, on a
//! kernel built without that file, from the parent field of every
//! `/proc/<pid>/stat`.
//!
//! A freeze stops a process before it reads its children, so every pid it then
//! signals belongs to a child of a stopped process or of the keeper: neither
//! reaps while the walk runs, so none of those pids can have been reused.

use std::collections::{HashSet, VecDeque};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::process::limits::KEEPER_FREEZE_PASSES;
use crate::process::sys::{self, Signal};

/// Whether this kernel has `/proc/<pid>/task/<tid>/children`.
fn children_file() -> bool {
    static PRESENT: OnceLock<bool> = OnceLock::new();
    *PRESENT.get_or_init(|| {
        let me = std::process::id();
        std::fs::metadata(format!("/proc/{me}/task/{me}/children")).is_ok()
    })
}

fn parse_pids(text: &str) -> impl Iterator<Item = i32> + '_ {
    text.split_ascii_whitespace().filter_map(|w| w.parse().ok())
}

/// The state letter and parent of one process, from `/proc/<pid>/stat`.
fn stat(pid: i32) -> Option<(char, i32)> {
    let bytes = std::fs::read(format!("/proc/{pid}/stat")).ok()?;
    let end = bytes.iter().rposition(|&b| b == b')')?;
    let rest = std::str::from_utf8(&bytes[end + 1..]).ok()?;
    let mut fields = rest.split_ascii_whitespace();
    let state = fields.next()?.chars().next()?;
    let parent = fields.next()?.parse().ok()?;
    Some((state, parent))
}

/// The children of `pid`.
pub(super) fn children(pid: i32) -> Vec<i32> {
    if children_file() {
        let mut out = Vec::new();
        if let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) {
            for task in tasks.flatten() {
                let path = task.path().join("children");
                if let Ok(text) = std::fs::read_to_string(path) {
                    out.extend(parse_pids(&text));
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        return out;
    }
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok())
        .filter(|candidate| stat(*candidate).is_some_and(|(_, parent)| parent == pid))
        .collect()
}

fn alive(pid: i32) -> bool {
    stat(pid).is_some_and(|(state, _)| state != 'Z' && state != 'X')
}

/// Every descendant of `root`, by parent links.
pub(super) fn descendants(root: i32) -> Vec<i32> {
    let mut seen = HashSet::new();
    let mut queue: VecDeque<i32> = children(root).into();
    let mut out = Vec::new();
    while let Some(pid) = queue.pop_front() {
        if pid == root || !seen.insert(pid) {
            continue;
        }
        out.push(pid);
        queue.extend(children(pid));
    }
    out
}

/// How many descendants of `root` are alive (not zombies).
pub(super) fn live_descendants(root: i32) -> usize {
    descendants(root).into_iter().filter(|p| alive(*p)).count()
}

/// Stop every descendant of `root` (SIGSTOP), walking again until a pass finds
/// none it had not stopped, then SIGKILL and SIGCONT them all. Returns how many
/// are still alive a moment later.
pub(super) fn freeze_kill(root: i32) -> usize {
    let started = Instant::now();
    let mut stopped: HashSet<i32> = HashSet::new();
    for _ in 0..KEEPER_FREEZE_PASSES {
        let mut new = 0;
        let mut queue: VecDeque<i32> = children(root).into();
        let mut seen = HashSet::new();
        while let Some(pid) = queue.pop_front() {
            if pid == root || !seen.insert(pid) {
                continue;
            }
            if !stopped.contains(&pid) {
                let _ = sys::signal_process(pid, Signal::STOP);
                stopped.insert(pid);
                new += 1;
            }
            // Read only after the stop: the children of a stopped process
            // keep their pids, since it cannot reap them.
            queue.extend(children(pid));
        }
        if new == 0 || started.elapsed() > Duration::from_secs(1) {
            break;
        }
    }
    for pid in &stopped {
        let _ = sys::signal_process(*pid, Signal::KILL);
        let _ = sys::signal_process(*pid, Signal::CONT);
    }
    let until = Instant::now() + Duration::from_millis(500);
    loop {
        let live = live_descendants(root);
        if live == 0 || Instant::now() >= until {
            return live;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Reap every exited child of `root` except `keep`.
pub(super) fn reap_children(root: i32, keep: i32) {
    for child in children(root) {
        if child != keep && stat(child).is_some_and(|(state, _)| state == 'Z') {
            let _ = sys::reap_if_exited(child);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stat_of_this_process_names_its_parent() {
        let me = i32::try_from(std::process::id()).unwrap();
        let (state, parent) = stat(me).expect("own stat");
        assert_ne!(state, 'Z');
        assert_eq!(
            Some(parent),
            rustix::process::getppid().map(|p| p.as_raw_nonzero().get())
        );
    }
}
