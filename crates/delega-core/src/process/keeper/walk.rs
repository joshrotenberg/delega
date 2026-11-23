//! The keeper's own view of its tree on Linux, from `/proc`, without `ps`.
//!
//! The keeper is a child subreaper, so every live descendant of the program
//! is reached from the keeper by parent links. Children are read from
//! `/proc/<pid>/task/<tid>/children` (every thread of the parent), or, on a
//! kernel built without that file, from the parent field of every
//! `/proc/<pid>/stat`.
//!
//! Nothing is signalled by pid number: a process is signalled only through a
//! pidfd opened after it was found and checked to be a live child of the
//! parent it was found under (a pid reused between the read and the pidfd has
//! another parent), with its start time kept so the pid can be checked again
//! before its children are read. A process that cannot be held is not
//! signalled and nothing below it is walked. A zero is trusted only when it is
//! proven (see [`Walked::complete`]), with a full scan of `/proc` as a second
//! source.

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

/// Every descendant of `root` in one pass, by parent links (no signals).
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

/// Every live process, with its parent, from a full scan of `/proc/*/stat`.
/// This source does not depend on the children files, which can omit a live
/// child while another child of the same parent exits during the read.
fn scan() -> HashMap<i32, i32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return HashMap::new();
    };
    entries
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok())
        .filter_map(|pid| match stat(pid) {
            Some((state, parent)) if state != 'Z' && state != 'X' => Some((pid, parent)),
            _ => None,
        })
        .collect()
}

/// The live descendants of `root` by the full scan.
fn scanned_descendants(root: i32) -> BTreeSet<i32> {
    let table = scan();
    let mut out = BTreeSet::new();
    let mut frontier = vec![root];
    while let Some(parent) = frontier.pop() {
        for (pid, _) in table.iter().filter(|(_, p)| **p == parent) {
            if *pid != root && out.insert(*pid) {
                frontier.push(*pid);
            }
        }
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
    /// Whether the result is proven: (census) two passes in a row agreed, the
    /// full scan agreed with the walk, and the tree was not in flux; (freeze)
    /// besides that, a pass found nothing new, held and stopped everything it
    /// found, and refused nothing. A result that is not complete is never
    /// taken as an empty tree.
    pub complete: bool,
    /// Live processes a freeze could not hold: no pidfd, or a parent that had
    /// changed since they were found. They were not signalled.
    pub refused: u32,
}

impl Walked {
    pub(super) fn detail(&self) -> String {
        format!(
            "passes={} complete={} refused={}",
            self.passes, self.complete, self.refused
        )
    }

    /// The count to act on: an unproven result is never zero.
    pub(super) fn effective_live(&self) -> usize {
        if self.complete {
            self.live
        } else {
            self.live.max(1)
        }
    }
}

/// Whether the tree is in flux: a direct child of `root` other than `keep`
/// has exited and is not reaped. A process exits as the keeper's child only
/// after it was reparented to it, and its own children are reparented at that
/// moment too, so a pass that read the tree around it can have missed them.
/// The check comes before the reap, so an exit since the last check counts;
/// the reap then clears it for the next one.
fn in_flux(root: i32, keep: i32) -> bool {
    let flux = children(root)
        .into_iter()
        .any(|c| c != keep && stat(c).is_some_and(|(state, _)| state == 'Z' || state == 'X'));
    reap_children(root, keep);
    flux
}

/// How many descendants of `root` are alive. A count is complete only when
/// two passes in a row find the same live set by the children files, the full
/// scan of `/proc` finds the same set, and the tree was not in flux around
/// either. A tree that keeps changing (a relay) never meets that.
pub(super) fn census(root: i32, keep: i32) -> Walked {
    let mut previous: Option<BTreeSet<i32>> = None;
    let mut passes = 0;
    let mut last = 0;
    for _ in 0..KEEPER_FREEZE_PASSES {
        passes += 1;
        let flux_before = in_flux(root, keep);
        let walked: BTreeSet<i32> = descendants(root)
            .into_iter()
            .filter(|p| alive(*p))
            .collect();
        let scanned = scanned_descendants(root);
        let flux_after = in_flux(root, keep);
        last = walked.len().max(scanned.len());
        if !flux_before && !flux_after && walked == scanned {
            if previous.as_ref() == Some(&walked) {
                return Walked {
                    live: walked.len(),
                    passes,
                    complete: true,
                    refused: 0,
                };
            }
            previous = Some(walked);
        } else {
            previous = None;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    Walked {
        live: last.max(1),
        passes,
        complete: false,
        refused: 0,
    }
}

/// Why a process could not be held.
enum NotHeld {
    /// It is gone.
    Gone,
    /// It is alive and could not be verified (no pidfd, or a parent other than
    /// the one it was found under): it is not signalled, and nothing below it
    /// is walked.
    Refused,
}

/// A process held for signalling: a pidfd, so the signal reaches this
/// process or none, and its start time, so the pid can be checked to still
/// name it before its children are read.
struct Held {
    pid: i32,
    fd: OwnedFd,
    ticks: u64,
}

impl Held {
    /// Hold `pid` if it is still a live child of `parent`. The check comes
    /// after the pidfd is open: a pid reused between the children read and
    /// here has another parent and is not held. Without a pidfd nothing is
    /// held: no process is ever signalled by number.
    fn take(pid: i32, parent: i32) -> Result<Self, NotHeld> {
        let raw = Pid::from_raw(pid).ok_or(NotHeld::Gone)?;
        let fd = match pidfd_open(raw, PidfdFlags::empty()) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::SRCH) => return Err(NotHeld::Gone),
            Err(_) => return Err(NotHeld::Refused),
        };
        let stat = sys::proc_stat(pid).ok_or(NotHeld::Gone)?;
        if stat.state == 'Z' || stat.state == 'X' {
            return Err(NotHeld::Gone);
        }
        if stat.parent != parent {
            return Err(NotHeld::Refused);
        }
        Ok(Self {
            pid,
            fd,
            ticks: stat.start_ticks,
        })
    }

    /// The process's stat if the pid still names it.
    fn current(&self) -> Option<sys::ProcStat> {
        sys::proc_stat(self.pid).filter(|stat| stat.start_ticks == self.ticks)
    }

    fn send(&self, signal: Signal) {
        let _ = pidfd_send_signal(&self.fd, signal);
    }
}

/// Wait until `until` for a SIGSTOP to take effect: a stopped process forks
/// nothing more and reaps none of its children, so its children list is then
/// complete and their pids stay theirs. `Some(true)` stopped, `Some(false)`
/// still running, `None` gone.
fn wait_stopped(held: &Held, until: Instant) -> Option<bool> {
    loop {
        match held.current() {
            None => return None,
            Some(stat) if matches!(stat.state, 'T' | 't') => return Some(true),
            Some(stat) if matches!(stat.state, 'Z' | 'X') => return None,
            Some(_) => {}
        }
        if Instant::now() >= until {
            return Some(false);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Send `signal` (then SIGCONT, so a stopped process receives it) to every
/// descendant of `root` one pass finds and can hold; a process that cannot be
/// held is not signalled and nothing below it is walked.
pub(super) fn signal_tree(root: i32, signal: Signal) {
    let mut seen = HashSet::new();
    let mut queue: VecDeque<(i32, i32)> = children(root).into_iter().map(|c| (c, root)).collect();
    while let Some((pid, parent)) = queue.pop_front() {
        if pid == root || !seen.insert(pid) {
            continue;
        }
        if let Ok(held) = Held::take(pid, parent) {
            held.send(signal);
            held.send(Signal::CONT);
            queue.extend(children(pid).into_iter().map(|c| (c, pid)));
        }
    }
}

/// Whether the full scan finds no live process outside the held set whose
/// parent is the keeper or a held process. With everything held stopped
/// nothing can fork or exit, so this second source confirms that the walk
/// missed nothing.
fn scan_confirms(root: i32, keep: i32, held: &HashMap<i32, Held>) -> bool {
    scan().iter().all(|(pid, parent)| {
        *pid == root
            || *pid == keep && !alive(keep)
            || !(*parent == root || held.contains_key(parent))
            || held.get(pid).is_some_and(|h| {
                h.current()
                    .is_some_and(|stat| matches!(stat.state, 'T' | 't'))
            })
    })
}

/// Stop every descendant of `root` (SIGSTOP), walking again from the keeper's
/// own children until a pass proves the tree frozen: it found nothing new,
/// held and stopped everything it found, refused nothing, the tree was not in
/// flux, and the full scan of `/proc` confirms nothing outside the held set
/// hangs below the keeper or a held process. Then SIGKILL and SIGCONT go to
/// everything held, through the pidfds, and a complete census must find the
/// tree empty. Bounded by `KEEPER_FREEZE_PASSES` passes and one second,
/// checked for every process; a freeze that runs out is not complete, and its
/// count is never zero.
pub(super) fn freeze_kill(root: i32, keep: i32) -> Walked {
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut held: HashMap<i32, Held> = HashMap::new();
    let mut passes = 0;
    let mut frozen = false;
    let mut refused_total = 0;
    for _ in 0..KEEPER_FREEZE_PASSES {
        passes += 1;
        let mut sound = !in_flux(root, keep);
        let mut new = 0;
        let mut queue: VecDeque<(i32, i32)> =
            children(root).into_iter().map(|c| (c, root)).collect();
        let mut seen = HashSet::new();
        while let Some((pid, parent)) = queue.pop_front() {
            if Instant::now() >= deadline {
                sound = false;
                break;
            }
            if pid == root || !seen.insert(pid) {
                continue;
            }
            if let std::collections::hash_map::Entry::Vacant(slot) = held.entry(pid) {
                match Held::take(pid, parent) {
                    Ok(process) => {
                        process.send(Signal::STOP);
                        slot.insert(process);
                        new += 1;
                    }
                    Err(NotHeld::Gone) => continue,
                    Err(NotHeld::Refused) => {
                        refused_total += 1;
                        sound = false;
                        continue;
                    }
                }
            }
            let Some(process) = held.get(&pid) else {
                continue;
            };
            // Read the children only of a process that is still the one held
            // and whose stop has taken effect.
            let until = (Instant::now() + Duration::from_millis(50)).min(deadline);
            match wait_stopped(process, until) {
                Some(true) => queue.extend(children(pid).into_iter().map(|c| (c, pid))),
                Some(false) => sound = false,
                None => {}
            }
        }
        sound = sound && !in_flux(root, keep);
        if new == 0 && sound && scan_confirms(root, keep, &held) {
            frozen = true;
            break;
        }
        if Instant::now() >= deadline {
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
        let complete = frozen && counted.complete;
        if (complete && counted.live == 0) || Instant::now() >= until {
            return Walked {
                live: counted.live,
                passes,
                complete,
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
