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
use std::sync::atomic::{AtomicU64, Ordering};
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

/// A read of `/proc` that failed for a reason other than the process being
/// gone. It is never taken as absence: the pass that met it is incomplete.
/// Every one is made by [`read_failed`], which counts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ReadFailed;

/// Reads of `/proc` that failed in this process since it started, whatever
/// walk met them (a census, a pass discarded as in flux, a freeze, a reap of
/// orphans). Any one withholds the final proof (see [`final_census_proves_empty`]).
static READ_FAILURES: AtomicU64 = AtomicU64::new(0);

/// Test seam: make this many of the next reads in this process fail, on any
/// thread. Set in the keeper from [`INJECT_READ_FAILURES_ENV`], and in a
/// helper process of the walk tests.
#[cfg(test)]
static INJECTED_READ_FAILURES: AtomicU64 = AtomicU64::new(0);

/// Test seam: the keeper started by the tests injects this many read failures
/// (see [`INJECTED_READ_FAILURES`]).
#[cfg(test)]
pub(super) const INJECT_READ_FAILURES_ENV: &str = "DELEGA_TEST_KEEPER_FAIL_READS";

/// Set the test seam [`INJECTED_READ_FAILURES`] from the environment.
#[cfg(test)]
pub(super) fn inject_read_failures_from_env() {
    if let Some(n) = std::env::var(INJECT_READ_FAILURES_ENV)
        .ok()
        .and_then(|v| v.parse().ok())
    {
        INJECTED_READ_FAILURES.store(n, Ordering::SeqCst);
    }
}

/// A read failure, counted.
fn read_failed() -> ReadFailed {
    READ_FAILURES.fetch_add(1, Ordering::SeqCst);
    ReadFailed
}

/// Whether any read of `/proc` failed in this process since it started.
fn any_read_failed() -> bool {
    READ_FAILURES.load(Ordering::SeqCst) > 0
}

#[cfg(test)]
thread_local! {
    /// Test seam: make every children and scan read on this thread fail, as
    /// descriptor exhaustion would.
    static FAIL_READS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Test seam: make the stat of this pid read as missing on this thread,
    /// as `hidepid` would for a process listed in a children file.
    static MISSING_STAT: std::cell::Cell<i32> = const { std::cell::Cell::new(0) };
}

fn forced_failure() -> bool {
    #[cfg(test)]
    {
        let mut left = INJECTED_READ_FAILURES.load(Ordering::SeqCst);
        let injected = loop {
            if left == 0 {
                break false;
            }
            match INJECTED_READ_FAILURES.compare_exchange(
                left,
                left - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break true,
                Err(now) => left = now,
            }
        };
        injected || FAIL_READS.with(std::cell::Cell::get)
    }
    #[cfg(not(test))]
    {
        false
    }
}

/// Whether an error means the process (or task) is gone.
fn is_gone(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound
        || error.raw_os_error() == Some(rustix::io::Errno::SRCH.raw_os_error())
}

/// The state letter and parent of one process, from `/proc/<pid>/stat`:
/// `Ok(None)` when it is gone, an error when it could not be read.
fn stat_read(pid: i32) -> Result<Option<(char, i32)>, ReadFailed> {
    #[cfg(test)]
    if MISSING_STAT.with(std::cell::Cell::get) == pid {
        return Ok(None);
    }
    let bytes = match std::fs::read(format!("/proc/{pid}/stat")) {
        Ok(bytes) => bytes,
        Err(error) if is_gone(&error) => return Ok(None),
        Err(_) => return Err(read_failed()),
    };
    let parse = || -> Option<(char, i32)> {
        let end = bytes.iter().rposition(|&b| b == b')')?;
        let rest = std::str::from_utf8(&bytes[end + 1..]).ok()?;
        let mut fields = rest.split_ascii_whitespace();
        let state = fields.next()?.chars().next()?;
        let parent = fields.next()?.parse().ok()?;
        Some((state, parent))
    };
    parse().map(Some).ok_or_else(read_failed)
}

/// [`stat_read`] where a failure needs no distinction from absence (the
/// caller only reaps or reports).
fn stat(pid: i32) -> Option<(char, i32)> {
    stat_read(pid).ok().flatten()
}

/// The children of `pid`; empty when it is gone, an error when a read
/// failed for another reason.
pub(super) fn children(pid: i32) -> Result<Vec<i32>, ReadFailed> {
    if forced_failure() {
        return Err(read_failed());
    }
    if children_file() {
        let tasks = match std::fs::read_dir(format!("/proc/{pid}/task")) {
            Ok(tasks) => tasks,
            Err(error) if is_gone(&error) => return Ok(Vec::new()),
            Err(_) => return Err(read_failed()),
        };
        let mut out = Vec::new();
        for task in tasks {
            let task = match task {
                Ok(task) => task,
                // A task that ends during the listing is gone, not a failure.
                Err(error) if is_gone(&error) => continue,
                Err(_) => return Err(read_failed()),
            };
            match std::fs::read_to_string(task.path().join("children")) {
                Ok(text) => out.extend(parse_pids(&text)),
                Err(error) if is_gone(&error) => {}
                Err(_) => return Err(read_failed()),
            }
        }
        out.sort_unstable();
        out.dedup();
        return Ok(out);
    }
    Ok(scan()?
        .into_iter()
        .filter(|(_, parent)| *parent == pid)
        .map(|(child, _)| child)
        .collect())
}

/// Every descendant of `root` in one pass, by parent links (no signals).
pub(super) fn descendants(root: i32) -> Result<Vec<i32>, ReadFailed> {
    let mut seen = HashSet::new();
    let mut queue: VecDeque<i32> = children(root)?.into();
    let mut out = Vec::new();
    while let Some(pid) = queue.pop_front() {
        if pid == root || !seen.insert(pid) {
            continue;
        }
        out.push(pid);
        queue.extend(children(pid)?);
    }
    Ok(out)
}

/// Every live process, with its parent, from a full scan of `/proc/*/stat`.
/// This source does not depend on the children files, which can omit a live
/// child while another child of the same parent exits during the read.
fn scan() -> Result<HashMap<i32, i32>, ReadFailed> {
    if forced_failure() {
        return Err(read_failed());
    }
    let entries = std::fs::read_dir("/proc").map_err(|_| read_failed())?;
    let mut table = HashMap::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            // A process that ends during the listing is gone, not a failure.
            Err(error) if is_gone(&error) => continue,
            Err(_) => return Err(read_failed()),
        };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<i32>().ok())
        else {
            continue;
        };
        if let Some((state, parent)) = stat_read(pid)?
            && state != 'Z'
            && state != 'X'
        {
            table.insert(pid, parent);
        }
    }
    Ok(table)
}

/// The live descendants of `root` by the full scan.
fn scanned_descendants(root: i32) -> Result<BTreeSet<i32>, ReadFailed> {
    let table = scan()?;
    let mut out = BTreeSet::new();
    let mut frontier = vec![root];
    while let Some(parent) = frontier.pop() {
        for (pid, _) in table.iter().filter(|(_, p)| **p == parent) {
            if *pid != root && out.insert(*pid) {
                frontier.push(*pid);
            }
        }
    }
    Ok(out)
}

/// Whether a process listed in a children file is alive. A stat that reads
/// as missing is absence only when the process's pidfd shows it gone; a
/// process the kernel lists but hides (`hidepid`) is a failed read, so the
/// pass that met it is incomplete.
fn alive_listed(pid: i32) -> Result<bool, ReadFailed> {
    match stat_read(pid)? {
        Some((state, _)) => Ok(state != 'Z' && state != 'X'),
        None if gone_by_pidfd(pid) => Ok(false),
        None => Err(read_failed()),
    }
}

/// Whether the pidfd of `pid` shows it gone: no such process, or exited.
fn gone_by_pidfd(pid: i32) -> bool {
    let Some(raw) = Pid::from_raw(pid) else {
        return true;
    };
    match pidfd_open(raw, PidfdFlags::empty()) {
        Err(rustix::io::Errno::SRCH) => true,
        Ok(fd) => sys::pidfd_exited(&fd),
        Err(_) => false,
    }
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
    /// Reads of `/proc` that failed for a reason other than the process
    /// being gone. Unlike a tree that keeps changing, a failed read is a
    /// hole in what the keeper could see.
    pub read_failures: u32,
    /// Whether the result would be complete if nothing had been refused: for
    /// a census the same as `complete` (a census holds nothing, so it refuses
    /// nothing); for a freeze, whether it froze the tree and its census after
    /// the kill is complete. The owner keeps a refused hold apart from other
    /// holes, because a later final census can supersede a refusal (see
    /// [`Walked::proves_empty`]) and nothing supersedes the others.
    pub complete_apart_from_refusals: bool,
}

impl Walked {
    pub(super) fn detail(&self) -> String {
        format!(
            "passes={} complete={} refused={} read_failures={} complete_apart_from_refusals={}",
            self.passes,
            self.complete,
            self.refused,
            self.read_failures,
            self.complete_apart_from_refusals
        )
    }

    /// Whether this answer, taken as the final census at the reap, proves the
    /// keeper's tree empty: it is complete (two passes in a row agreed, the
    /// full scan of `/proc` found the same set, and the tree was not in flux),
    /// it found nothing alive, and within this census no hold was refused and
    /// no read failed. A live process in the tree, held or not, is counted by
    /// every pass that reads its stat, and a stat that cannot be read is a
    /// read failure, so a live process never yields this answer.
    pub(super) fn proves_empty(&self) -> bool {
        self.complete && self.live == 0 && self.refused == 0 && self.read_failures == 0
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
/// the reap then clears it for the next one. A read that fails counts as flux.
fn in_flux(root: i32, keep: i32) -> bool {
    let flux = match children(root) {
        Ok(kids) => kids.into_iter().any(|c| {
            c != keep
                && match stat_read(c) {
                    Ok(Some((state, _))) => state == 'Z' || state == 'X',
                    Ok(None) => false,
                    Err(ReadFailed) => true,
                }
        }),
        Err(ReadFailed) => true,
    };
    reap_children(root, keep);
    flux
}

/// One census pass: the live set by the children files and by the full scan,
/// or an error when a read failed.
fn census_pass(root: i32) -> Result<(BTreeSet<i32>, BTreeSet<i32>), ReadFailed> {
    let mut walked = BTreeSet::new();
    for pid in descendants(root)? {
        if alive_listed(pid)? {
            walked.insert(pid);
        }
    }
    Ok((walked, scanned_descendants(root)?))
}

/// How many descendants of `root` are alive. A count is complete only when
/// two passes in a row find the same live set by the children files, the full
/// scan of `/proc` finds the same set, no read failed, and the tree was not in
/// flux around either. A tree that keeps changing (a relay) never meets that.
pub(super) fn census(root: i32, keep: i32) -> Walked {
    let mut previous: Option<BTreeSet<i32>> = None;
    let mut passes = 0;
    let mut last = 0;
    let mut read_failures = 0;
    for _ in 0..KEEPER_FREEZE_PASSES {
        passes += 1;
        let flux_before = in_flux(root, keep);
        let pass = census_pass(root);
        let flux_after = in_flux(root, keep);
        match pass {
            Ok((walked, scanned)) if !flux_before && !flux_after && walked == scanned => {
                last = walked.len();
                if previous.as_ref() == Some(&walked) {
                    return Walked {
                        live: walked.len(),
                        passes,
                        complete: true,
                        refused: 0,
                        read_failures,
                        complete_apart_from_refusals: true,
                    };
                }
                previous = Some(walked);
            }
            Ok((walked, scanned)) => {
                last = walked.len().max(scanned.len());
                previous = None;
            }
            Err(ReadFailed) => {
                read_failures += 1;
                previous = None;
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    Walked {
        live: last.max(1),
        passes,
        complete: false,
        refused: 0,
        read_failures,
        complete_apart_from_refusals: false,
    }
}

/// The final census at the reap, and whether it proves the keeper's tree
/// empty: the census itself proves it ([`Walked::proves_empty`]), and no read
/// of `/proc` failed in this process at any point since it started, in this
/// census (a pass discarded as in flux included) or in any walk before it.
/// Coarser than counting the failures of one census, and enough: a keeper
/// that never met a failed read can prove its tree empty, one that met any
/// cannot.
pub(super) fn final_census_proves_empty(root: i32, keep: i32) -> bool {
    let walked = census(root, keep);
    walked.proves_empty() && !any_read_failed()
}

/// Why a process could not be held.
pub(super) enum NotHeld {
    /// It is gone.
    Gone,
    /// It is alive, or could not be shown gone, and could not be verified (no
    /// pidfd, an unreadable stat, or a parent other than the one it was found
    /// under): it is not signalled, and nothing below it is walked.
    Refused,
}

/// A process held for signalling: a pidfd, so the signal reaches this
/// process or none, and its start time, so the pid can be checked to still
/// name it before its children are read.
pub(super) struct Held {
    pid: i32,
    fd: OwnedFd,
    ticks: u64,
}

impl Held {
    /// Hold `pid` if it is still a live child of `parent`. The check comes
    /// after the pidfd is open: a pid reused between the children read and
    /// here has another parent and is not held. Without a pidfd nothing is
    /// held: no process is ever signalled by number.
    pub(super) fn take(pid: i32, parent: i32) -> Result<Self, NotHeld> {
        let raw = Pid::from_raw(pid).ok_or(NotHeld::Gone)?;
        let fd = match pidfd_open(raw, PidfdFlags::empty()) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::SRCH) => return Err(NotHeld::Gone),
            Err(_) => return Err(NotHeld::Refused),
        };
        let Some(stat) = sys::proc_stat(pid) else {
            return Err(if sys::pidfd_exited(&fd) {
                NotHeld::Gone
            } else {
                NotHeld::Refused
            });
        };
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

    /// The children of the held process, read by its pid and kept only if
    /// the process had not exited by the end of the read (its pidfd says so):
    /// once it exits and is reaped its pid can name a foreign process, whose
    /// children must not be taken for its own. `Ok(None)` when the read was
    /// discarded.
    pub(super) fn children(&self) -> Result<Option<Vec<i32>>, ReadFailed> {
        self.children_with(|| children(self.pid))
    }

    fn children_with(
        &self,
        read: impl FnOnce() -> Result<Vec<i32>, ReadFailed>,
    ) -> Result<Option<Vec<i32>>, ReadFailed> {
        let kids = read()?;
        Ok((!sys::pidfd_exited(&self.fd)).then_some(kids))
    }
}

/// Hold a queued child found in the children list of `parent`, whose hold
/// is `parent_hold` (`None` for the keeper itself). The child is taken under
/// that parent only while the parent's pidfd shows it alive: once it has
/// exited its pid may name a foreign process, so the child counts only if it
/// is now the keeper's own child (reparented); anything else is refused.
fn take_queued(
    pid: i32,
    parent: i32,
    parent_hold: Option<&Held>,
    root: i32,
) -> Result<Held, NotHeld> {
    match parent_hold {
        Some(hold) if sys::pidfd_exited(&hold.fd) => Held::take(pid, root),
        _ => Held::take(pid, parent),
    }
}

/// Wait until `until` for a SIGSTOP to take effect: a stopped process forks
/// nothing more and reaps none of its children, so its children list is then
/// complete and their pids stay theirs. `Some(true)` stopped, `Some(false)`
/// still running or unreadable, `None` gone.
fn wait_stopped(held: &Held, until: Instant) -> Option<bool> {
    loop {
        match held.current() {
            None if sys::pidfd_exited(&held.fd) => return None,
            None => {}
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
/// held is not signalled, nothing below it is walked, and a children read is
/// kept only while the parent has not exited.
pub(super) fn signal_tree(root: i32, signal: Signal) {
    let mut seen = HashSet::new();
    // Each queued child carries its parent's hold, so it is taken under that
    // parent only while the parent is still alive (see `take_queued`).
    let mut queue: VecDeque<(i32, i32, Option<std::rc::Rc<Held>>)> = children(root)
        .unwrap_or_default()
        .into_iter()
        .map(|c| (c, root, None))
        .collect();
    while let Some((pid, parent, parent_hold)) = queue.pop_front() {
        if pid == root || !seen.insert(pid) {
            continue;
        }
        if let Ok(held) = take_queued(pid, parent, parent_hold.as_deref(), root) {
            held.send(signal);
            held.send(Signal::CONT);
            if let Ok(Some(kids)) = held.children() {
                let held = std::rc::Rc::new(held);
                queue.extend(kids.into_iter().map(|c| (c, pid, Some(held.clone()))));
            }
        }
    }
}

/// Whether the full scan finds no live process outside the held set whose
/// parent is the keeper or a held process. With everything held stopped
/// nothing can fork or exit, so this second source confirms that the walk
/// missed nothing. A failed scan confirms nothing.
fn scan_confirms(root: i32, held: &HashMap<i32, Held>) -> bool {
    let Ok(table) = scan() else {
        return false;
    };
    table.iter().all(|(pid, parent)| {
        *pid == root
            || !(*parent == root || held.contains_key(parent))
            || held.get(pid).is_some_and(|h| {
                h.current()
                    .is_some_and(|stat| matches!(stat.state, 'T' | 't'))
            })
    })
}

/// Whether a freeze is proven: its walk froze the tree, it refused nothing
/// in any pass, and the census after the kill is complete. A refused hold
/// anywhere leaves a process that was never signalled, so the result is not
/// proven whatever later passes found.
pub(super) fn freeze_proven(frozen: bool, refused: u32, census_complete: bool) -> bool {
    frozen && refused == 0 && census_complete
}

/// Stop every descendant of `root` (SIGSTOP), walking again from the keeper's
/// own children until a pass proves the tree frozen: it found nothing new,
/// held and stopped everything it found, refused nothing, no read failed or
/// was discarded, the tree was not in flux, and the full scan of `/proc`
/// confirms nothing outside the held set hangs below the keeper or a held
/// process. Then SIGKILL and SIGCONT go to everything held, through the
/// pidfds, which are then released, and a complete census must find the tree
/// empty. Bounded by `KEEPER_FREEZE_PASSES` passes and one second, checked for
/// every process; a freeze that runs out, or that refused any process, is not
/// complete, and its count is never zero.
pub(super) fn freeze_kill(root: i32, keep: i32) -> Walked {
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut held: HashMap<i32, Held> = HashMap::new();
    let mut passes = 0;
    let mut frozen = false;
    let mut refused_total = 0;
    let mut read_failures = 0;
    for _ in 0..KEEPER_FREEZE_PASSES {
        passes += 1;
        let mut sound = !in_flux(root, keep);
        let mut new = 0;
        let mut queue: VecDeque<(i32, i32)> = match children(root) {
            Ok(kids) => kids.into_iter().map(|c| (c, root)).collect(),
            Err(ReadFailed) => {
                read_failures += 1;
                sound = false;
                VecDeque::new()
            }
        };
        let mut seen = HashSet::new();
        while let Some((pid, parent)) = queue.pop_front() {
            if Instant::now() >= deadline {
                sound = false;
                break;
            }
            if pid == root || !seen.insert(pid) {
                continue;
            }
            if !held.contains_key(&pid) {
                match take_queued(pid, parent, held.get(&parent), root) {
                    Ok(process) => {
                        process.send(Signal::STOP);
                        held.insert(pid, process);
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
            // and whose stop has taken effect, and keep the read only if it
            // had not exited by its end.
            let until = (Instant::now() + Duration::from_millis(50)).min(deadline);
            match wait_stopped(process, until) {
                Some(true) => match process.children() {
                    Ok(Some(kids)) => queue.extend(kids.into_iter().map(|c| (c, pid))),
                    Ok(None) => sound = false,
                    Err(ReadFailed) => {
                        read_failures += 1;
                        sound = false;
                    }
                },
                Some(false) => sound = false,
                None => {}
            }
        }
        sound = sound && !in_flux(root, keep);
        if new == 0 && sound && scan_confirms(root, &held) {
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
    // The pidfds are not needed any more; releasing them leaves descriptors
    // for the census reads.
    drop(held);
    let until = Instant::now() + Duration::from_millis(500);
    loop {
        let counted = census(root, keep);
        let complete = freeze_proven(frozen, refused_total, counted.complete);
        if (complete && counted.live == 0) || Instant::now() >= until {
            return Walked {
                live: counted.live,
                passes,
                complete,
                refused: refused_total,
                read_failures: read_failures + counted.read_failures,
                complete_apart_from_refusals: frozen && counted.complete,
            };
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Reap every exited child of `root` except `keep`.
pub(super) fn reap_children(root: i32, keep: i32) {
    for child in children(root).unwrap_or_default() {
        if child != keep && stat(child).is_some_and(|(state, _)| state == 'Z') {
            let _ = sys::reap_if_exited(child);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_hold_anywhere_leaves_the_freeze_unproven_and_never_zero() {
        assert!(!freeze_proven(true, 1, true));
        assert!(freeze_proven(true, 0, true));
        let walked = Walked {
            live: 0,
            passes: 3,
            complete: freeze_proven(true, 1, true),
            refused: 1,
            read_failures: 0,
            complete_apart_from_refusals: true,
        };
        assert_eq!(walked.effective_live(), 1);
    }

    /// Only a complete census that found nothing, refused nothing and had no
    /// failed read is a proof; a freeze's earlier refusals are not carried
    /// into the final census, which is its own walk.
    #[test]
    fn only_a_complete_empty_census_without_refusals_or_failed_reads_proves_the_tree_empty() {
        let proof = Walked {
            live: 0,
            passes: 2,
            complete: true,
            refused: 0,
            read_failures: 0,
            complete_apart_from_refusals: true,
        };
        assert!(proof.proves_empty());
        assert!(!Walked { live: 1, ..proof }.proves_empty());
        assert!(
            !Walked {
                complete: false,
                ..proof
            }
            .proves_empty()
        );
        assert!(
            !Walked {
                refused: 1,
                ..proof
            }
            .proves_empty()
        );
        assert!(
            !Walked {
                read_failures: 1,
                ..proof
            }
            .proves_empty()
        );
    }

    /// Set to a mode to make [`census_helper_entry`] run one census as a
    /// process of its own (see [`in_a_helper`]).
    const HELPER_ENV: &str = "DELEGA_TEST_CENSUS_HELPER";
    /// The file the helper writes its findings to.
    const HELPER_OUT_ENV: &str = "DELEGA_TEST_CENSUS_OUT";

    /// Not a test: when this binary is started as a helper, run the census
    /// the mode names with this process as the root, write what it found, and
    /// exit. A census reaps the root's exited children and counts all its live
    /// ones, so in the shared test process it would take other tests'
    /// children; here the root has only what the mode starts.
    #[test]
    fn census_helper_entry() {
        use crate::test_spawn::Guarded;
        let (Some(mode), Some(out)) = (
            std::env::var_os(HELPER_ENV),
            std::env::var_os(HELPER_OUT_ENV),
        ) else {
            return;
        };
        let me = i32::try_from(std::process::id()).expect("a pid");
        let found = match mode.to_str() {
            Some("empty") => format!("proven={}", final_census_proves_empty(me, 0)),
            Some("one-child") => {
                let mut child = std::process::Command::new("sleep")
                    .arg("30")
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .guarded_spawn()
                    .expect("start a child");
                let pid = i32::try_from(child.id()).expect("a pid");
                let walked = census(me, 0);
                let proven = final_census_proves_empty(me, 0);
                let alive = !gone_by_pidfd(pid);
                let _ = child.kill();
                let _ = child.wait();
                format!(
                    "complete={} live={} alive={alive} proven={proven}",
                    walked.complete, walked.live
                )
            }
            Some("transient") => {
                // One read fails: the first of the census, in its check for
                // flux, so that pass is discarded and is not counted in the
                // census's own read failures.
                INJECTED_READ_FAILURES.store(1, Ordering::SeqCst);
                let walked = census(me, 0);
                let proven = final_census_proves_empty(me, 0);
                format!(
                    "census_alone_proves={} injected_left={} proven={proven}",
                    walked.proves_empty(),
                    INJECTED_READ_FAILURES.load(Ordering::SeqCst)
                )
            }
            _ => "unknown mode".to_string(),
        };
        std::fs::write(out, found).expect("write the findings");
        std::process::exit(0);
    }

    /// Run [`census_helper_entry`] in `mode` as a process of its own and
    /// return what it wrote.
    fn in_a_helper(mode: &str) -> String {
        use crate::test_spawn::Guarded;
        let dir = tempfile::tempdir().expect("a scratch directory");
        let out = dir.path().join("found");
        let exe = std::env::current_exe().expect("the test binary's path");
        let status = std::process::Command::new(exe)
            .args([
                "--exact",
                "process::keeper::walk::tests::census_helper_entry",
                "--test-threads=1",
                "-q",
            ])
            .env(HELPER_ENV, mode)
            .env(HELPER_OUT_ENV, &out)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .guarded_output()
            .expect("run the census helper")
            .status;
        assert!(status.success(), "the census helper failed: {status:?}");
        std::fs::read_to_string(out).expect("the helper's findings")
    }

    /// A root with nothing below it proves its tree empty, so the cases below
    /// fail for what they change, not for the helper.
    #[test]
    fn an_empty_tree_with_no_failed_read_is_proven_empty() {
        assert_eq!(in_a_helper("empty"), "proven=true");
    }

    /// A live process in the tree, the root's only child, is counted exactly
    /// once by a complete census, and keeps the final census from proving the
    /// tree empty, though nothing ever held or refused it.
    #[test]
    fn a_live_descendant_keeps_the_final_census_from_proving_the_tree_empty() {
        assert_eq!(
            in_a_helper("one-child"),
            "complete=true live=1 alive=true proven=false"
        );
    }

    /// A read that fails once, in a pass the census then discards as in flux,
    /// followed by two clean passes that agree on an empty tree: the census
    /// alone would prove the tree empty, and the final census does not.
    #[test]
    fn a_transient_read_failure_followed_by_two_clean_passes_does_not_prove_the_tree_empty() {
        assert_eq!(
            in_a_helper("transient"),
            "census_alone_proves=true injected_left=0 proven=false"
        );
    }

    /// A census that met a failed read is no proof, even with nothing alive.
    #[test]
    fn a_census_with_a_failed_read_does_not_prove_the_tree_empty() {
        FAIL_READS.with(|fail| fail.set(true));
        let me = i32::try_from(std::process::id()).expect("a pid");
        let walked = census(me, 0);
        FAIL_READS.with(|fail| fail.set(false));
        assert!(!walked.proves_empty(), "{walked:?}");
    }

    #[test]
    fn a_read_error_makes_the_census_incomplete_and_never_zero() {
        FAIL_READS.with(|fail| fail.set(true));
        let me = i32::try_from(std::process::id()).expect("a pid");
        let walked = census(me, 0);
        FAIL_READS.with(|fail| fail.set(false));
        assert!(!walked.complete, "{walked:?}");
        assert!(walked.effective_live() >= 1, "{walked:?}");
    }

    #[test]
    fn a_queued_child_of_an_exited_parent_is_refused_unless_reparented_to_the_keeper() {
        use crate::test_spawn::Guarded;
        let mut parent = std::process::Command::new("sleep")
            .arg("0.1")
            .guarded_spawn()
            .expect("start a stand-in parent");
        let parent_pid = i32::try_from(parent.id()).expect("a pid");
        let me = i32::try_from(std::process::id()).expect("a pid");
        let Ok(hold) = Held::take(parent_pid, me) else {
            let _ = parent.wait();
            panic!("the stand-in parent could not be held");
        };
        let until = Instant::now() + Duration::from_secs(5);
        while !sys::pidfd_exited(&hold.fd) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        // The parent has exited, so a child read from its pid is taken only
        // as the keeper's own child: init's child (pid 1's parent is 0) is
        // refused, not taken as the exited parent's.
        assert!(matches!(
            take_queued(1, parent_pid, Some(&hold), me),
            Err(NotHeld::Refused)
        ));
        let _ = parent.wait();
    }

    #[test]
    fn a_listed_child_whose_stat_is_hidden_makes_the_pass_incomplete() {
        use crate::test_spawn::Guarded;
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .guarded_spawn()
            .expect("start a child");
        let pid = i32::try_from(child.id()).expect("a pid");
        MISSING_STAT.with(|missing| missing.set(pid));
        let hidden = alive_listed(pid);
        let _ = child.kill();
        let _ = child.wait();
        let gone = alive_listed(pid);
        MISSING_STAT.with(|missing| missing.set(0));
        // Alive but unreadable is a failed read, not absence.
        assert_eq!(hidden, Err(ReadFailed));
        // Once its pidfd shows it gone, the missing stat is absence.
        assert_eq!(gone, Ok(false));
    }

    #[test]
    fn a_children_read_is_discarded_once_the_parent_has_exited() {
        use crate::test_spawn::Guarded;
        let mut child = std::process::Command::new("sleep")
            .arg("0.2")
            .guarded_spawn()
            .expect("start a child");
        let pid = i32::try_from(child.id()).expect("a pid");
        let me = i32::try_from(std::process::id()).expect("a pid");
        let Ok(held) = Held::take(pid, me) else {
            let _ = child.wait();
            panic!("the child could not be held");
        };
        // While it runs, a read is kept.
        assert_eq!(held.children_with(|| Ok(vec![42])), Ok(Some(vec![42])));
        // Once it has exited (a zombie until reaped below), a read of its pid
        // is discarded: the pid may already name another process.
        let until = Instant::now() + Duration::from_secs(5);
        while !sys::pidfd_exited(&held.fd) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(held.children_with(|| Ok(vec![42])), Ok(None));
        let _ = child.wait();
    }

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
