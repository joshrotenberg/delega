//! The keeper's own view of its tree on Linux, from `/proc`, without `ps`.
//!
//! The keeper is a child subreaper, so every live descendant of the program
//! is reached from the keeper by parent links. Children are read from
//! `/proc/<pid>/task/<tid>/children` (every thread of the parent), or, on a
//! kernel built without that file, from the parent field of every
//! `/proc/<pid>/stat`.
//!
//! Every process is held by a pidfd once found, checked to be a live child of
//! the parent it was found under (a pid reused between the read and the pidfd
//! has another parent), and signalled through the pidfd, so a reused pid is
//! never signalled. A freeze reads a process's children only once its SIGSTOP
//! has taken effect, and ends only when a pass starting again from the
//! keeper's own children finds nothing new and every process stopped.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::os::fd::OwnedFd;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use rustix::process::{Pid, PidfdFlags, pidfd_open, pidfd_send_signal};

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

/// Every descendant of `root` in one pass, by parent links.
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

/// How many descendants of `root` are alive (not zombies). One pass reads the
/// keeper's children once, so a process reparented to the keeper after that
/// read is missed; the walk is repeated until two passes in a row find the
/// same live set, within `KEEPER_FREEZE_PASSES`. A tree that keeps changing
/// (a relay) is reported with the last pass's count, which is not zero while
/// it lives.
pub(super) fn live_descendants(root: i32) -> usize {
    let mut previous: Option<BTreeSet<i32>> = None;
    for _ in 0..KEEPER_FREEZE_PASSES {
        let live: BTreeSet<i32> = descendants(root)
            .into_iter()
            .filter(|p| alive(*p))
            .collect();
        if previous.as_ref() == Some(&live) {
            return live.len();
        }
        previous = Some(live);
        std::thread::sleep(Duration::from_millis(2));
    }
    previous.map_or(0, |live| live.len().max(1))
}

/// A process held for signalling: a pidfd when the kernel has them, so the
/// signal reaches this process or none, never one that took its pid.
struct Held {
    pid: i32,
    fd: Option<OwnedFd>,
}

impl Held {
    /// Hold `pid` if it is still a live child of `parent`. The check comes
    /// after the pidfd is open: a pid reused between the children read and
    /// here has another parent and is not held.
    fn take(pid: i32, parent: i32) -> Option<Self> {
        let fd = Pid::from_raw(pid).and_then(|p| pidfd_open(p, PidfdFlags::empty()).ok());
        let (state, actual) = stat(pid)?;
        (actual == parent && state != 'Z' && state != 'X').then_some(Self { pid, fd })
    }

    fn send(&self, signal: Signal) {
        match &self.fd {
            Some(fd) => {
                let _ = pidfd_send_signal(fd, signal);
            }
            // Before Linux 5.3: by pid, right after the parent check.
            None => {
                let _ = sys::signal_process(self.pid, signal);
            }
        }
    }
}

/// Wait up to `bound` for a SIGSTOP to take effect: a stopped process forks
/// nothing more and reaps none of its children, so its children list is then
/// complete and theirs pids stay theirs. `false` if it is still running (an
/// uninterruptible wait, for example).
fn wait_stopped(pid: i32, bound: Duration) -> bool {
    let until = Instant::now() + bound;
    loop {
        match stat(pid) {
            None => return true,
            Some(('T' | 't' | 'Z' | 'X', _)) => return true,
            Some(_) => {}
        }
        if Instant::now() >= until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Send `signal` (then SIGCONT, so a stopped process receives it) to every
/// descendant of `root` one pass finds, each held and checked as in a freeze.
pub(super) fn signal_tree(root: i32, signal: Signal) {
    let mut seen = HashSet::new();
    let mut queue: VecDeque<(i32, i32)> = children(root).into_iter().map(|c| (c, root)).collect();
    while let Some((pid, parent)) = queue.pop_front() {
        if pid == root || !seen.insert(pid) {
            continue;
        }
        if let Some(held) = Held::take(pid, parent) {
            held.send(signal);
            held.send(Signal::CONT);
        }
        queue.extend(children(pid).into_iter().map(|c| (c, pid)));
    }
}

/// Stop every descendant of `root` (SIGSTOP), walking again from the keeper's
/// own children until a pass finds nothing new and every process it found has
/// stopped, then SIGKILL and SIGCONT them all through their pidfds. Returns how
/// many are still alive a moment later, by [`live_descendants`]; not zero when
/// the freeze could not complete within its bound.
pub(super) fn freeze_kill(root: i32) -> usize {
    let started = Instant::now();
    let mut held: HashMap<i32, Held> = HashMap::new();
    for _ in 0..KEEPER_FREEZE_PASSES {
        let mut new = 0;
        let mut all_stopped = true;
        let mut queue: VecDeque<(i32, i32)> =
            children(root).into_iter().map(|c| (c, root)).collect();
        let mut seen = HashSet::new();
        while let Some((pid, parent)) = queue.pop_front() {
            if pid == root || !seen.insert(pid) {
                continue;
            }
            if let std::collections::hash_map::Entry::Vacant(slot) = held.entry(pid) {
                let Some(process) = Held::take(pid, parent) else {
                    continue;
                };
                process.send(Signal::STOP);
                slot.insert(process);
                new += 1;
            }
            // Read the children only once the stop has taken effect.
            if !wait_stopped(pid, Duration::from_millis(50)) {
                all_stopped = false;
            }
            queue.extend(children(pid).into_iter().map(|c| (c, pid)));
        }
        if new == 0 && all_stopped {
            break;
        }
        if started.elapsed() > Duration::from_secs(1) {
            break;
        }
    }
    for process in held.values() {
        process.send(Signal::KILL);
        process.send(Signal::CONT);
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
