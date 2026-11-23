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

/// What a census or a freeze found, for the answer and the diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Walked {
    /// Live descendants of the keeper at the end.
    pub live: usize,
    /// Passes walked.
    pub passes: u32,
    /// Whether the last pass was complete: (census) it agreed with the one
    /// before and the tree was not in flux; (freeze) it found nothing new,
    /// every process stopped, no live process refused and no exited child of
    /// the keeper left to reap.
    pub complete: bool,
    /// Live processes a freeze could not hold because their parent had changed
    /// since they were found (reparented to the keeper); a later pass holds
    /// them under the keeper.
    pub refused: u32,
}

impl Walked {
    pub(super) fn detail(&self) -> String {
        format!(
            "passes={} complete={} refused={}",
            self.passes, self.complete, self.refused
        )
    }
}

/// Whether the tree is in flux: a direct child of `root` other than `keep`
/// has exited and is not reaped. A process exits as the keeper's child only
/// after it was reparented to it, and its own children are reparented at that
/// moment too, so a pass that read the tree around it can have missed them.
/// Zombies that can be reaped are reaped first, so only an exit since then
/// counts.
fn in_flux(root: i32, keep: i32) -> bool {
    reap_children(root, keep);
    children(root)
        .into_iter()
        .any(|c| c != keep && stat(c).is_some_and(|(state, _)| state == 'Z' || state == 'X'))
}

/// How many descendants of `root` are alive (not zombies), `keep` (the
/// program, which the keeper holds unreaped) not counted when it has exited.
/// One pass reads the keeper's children once, and a process reparented to
/// the keeper after that read is missed, so a count is accepted only when two
/// passes in a row find the same live set and the tree was not in flux around
/// either (see [`in_flux`]). A tree that keeps changing (a relay) never meets
/// that, and is reported with the last pass's count, at least 1.
pub(super) fn census(root: i32, keep: i32) -> Walked {
    let mut previous: Option<BTreeSet<i32>> = None;
    let mut passes = 0;
    for _ in 0..KEEPER_FREEZE_PASSES {
        passes += 1;
        let flux_before = in_flux(root, keep);
        let live: BTreeSet<i32> = descendants(root)
            .into_iter()
            .filter(|p| alive(*p))
            .collect();
        let flux_after = in_flux(root, keep);
        if !flux_before && !flux_after {
            if previous.as_ref() == Some(&live) {
                return Walked {
                    live: live.len(),
                    passes,
                    complete: true,
                    refused: 0,
                };
            }
            previous = Some(live);
        } else {
            previous = None;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    Walked {
        live: previous.map_or(1, |live| live.len().max(1)),
        passes,
        complete: false,
        refused: 0,
    }
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
/// own children until a pass is complete: it found nothing new, every process
/// it found has stopped, it refused no live process (one reparented to the
/// keeper since it was found, held by a later pass), and no exited child of
/// the keeper was left to reap (see [`in_flux`]). Then SIGKILL and SIGCONT go
/// to all of them through their pidfds, and the census says what is still
/// alive; not zero when the freeze could not complete within its bound.
pub(super) fn freeze_kill(root: i32, keep: i32) -> Walked {
    let started = Instant::now();
    let mut held: HashMap<i32, Held> = HashMap::new();
    let mut passes = 0;
    let mut complete = false;
    let mut refused_total = 0;
    for _ in 0..KEEPER_FREEZE_PASSES {
        passes += 1;
        let mut new = 0;
        let mut all_stopped = true;
        let mut refused = 0;
        let flux_before = in_flux(root, keep);
        let mut queue: VecDeque<(i32, i32)> =
            children(root).into_iter().map(|c| (c, root)).collect();
        let mut seen = HashSet::new();
        while let Some((pid, parent)) = queue.pop_front() {
            if pid == root || !seen.insert(pid) {
                continue;
            }
            if let std::collections::hash_map::Entry::Vacant(slot) = held.entry(pid) {
                let Some(process) = Held::take(pid, parent) else {
                    // Gone, or no longer the child of `parent`: when it is
                    // alive it was reparented (to the keeper) and the next
                    // pass, starting from the keeper's children, holds it.
                    if alive(pid) {
                        refused += 1;
                    }
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
        refused_total += refused;
        let flux_after = in_flux(root, keep);
        if new == 0 && all_stopped && refused == 0 && !flux_before && !flux_after {
            complete = true;
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
        let counted = census(root, keep);
        if counted.live == 0 || Instant::now() >= until {
            return Walked {
                live: counted.live,
                passes,
                complete: complete && counted.live == 0,
                refused: refused_total,
            };
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
