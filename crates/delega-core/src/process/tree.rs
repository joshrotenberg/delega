//! Descendant tracking.
//!
//! Some programs create another process group, so signalling the child's own
//! group does not reach everything they start. Ancestry is observed with `ps`
//! while the child is alive. Start times are kept with each pid, so a recycled
//! pid is never signalled by number alone.
//!
//! What is owned. In every listing the owned set starts from the child and
//! from every live process that is in the child's process group or session (the
//! session is read per pid, from `/proc/<pid>/stat` on Linux and with `getsid`
//! elsewhere, which the kernel answers exactly), and from
//! every process already tracked with a matching start time. Every process
//! whose parent is in the owned set is then owned, repeatedly, and is tracked
//! from then on. So a descendant that calls `setsid` is kept as long as one
//! listing saw it while its parent was still owned, whether that parent is the
//! child or any other member. The child's own row is only a seed: once it
//! exits its children are reparented, but the members it left behind are still
//! found through the session.
//!
//! A descendant that was never in such a listing (started and detached between
//! two scans, whose parent then exited before the next) is not known to this
//! tracker. That is a known gap, see the module documentation of
//! [`crate::process`].

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::io::AsyncReadExt;
use tokio::time::Instant;

use super::limits::{MAX_ERROR_CHARS, PS_MAX_BYTES, PS_READ_CHUNK, PS_TIMEOUT};
use super::marker::MarkedRow;
use super::sys::{self, Signal};

/// Why a process table could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InspectError {
    /// `ps` did not finish within its deadline.
    TimedOut,
    /// `ps` could not be started at all (missing or not executable), so
    /// retrying will not help.
    Unavailable(String),
    /// Any other failure, as text.
    Failed(String),
}

impl fmt::Display for InspectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InspectError::TimedOut => f.write_str("process listing timed out"),
            InspectError::Unavailable(message) => f.write_str(message),
            InspectError::Failed(message) => f.write_str(message),
        }
    }
}

/// One line of the process table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProcessRow {
    pub pid: i32,
    pub parent: i32,
    /// The start time as `ps` prints it. Compared, never parsed.
    pub started: String,
    pub group: i32,
    pub zombie: bool,
    /// Stopped by a signal (`T` in the state column).
    pub stopped: bool,
}

/// Parse one process table including state and group membership.
pub(crate) fn parse_rows(stdout: &str) -> Vec<ProcessRow> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let pid = parts.next()?.parse().ok()?;
            let parent = parts.next()?.parse().ok()?;
            let group = parts.next()?.parse().ok()?;
            let state = parts.next()?;
            let zombie = state.starts_with('Z');
            let stopped = state.starts_with('T');
            let started = parts.collect::<Vec<_>>().join(" ");
            (!started.is_empty()).then_some(ProcessRow {
                pid,
                parent,
                group,
                zombie,
                stopped,
                started,
            })
        })
        .collect()
}

/// Where `ps` is looked for, in order.
const PS_PROGRAMS: [&str; 2] = ["/bin/ps", "/usr/bin/ps"];

/// The deadline for one `ps` run. A test build allows far longer than any
/// listing takes, so a busy machine cannot turn a slow `ps` into a failed
/// inspection; the tests about a failed or hung listing inject their own
/// lister instead.
fn ps_timeout() -> std::time::Duration {
    if cfg!(test) {
        std::time::Duration::from_secs(60)
    } else {
        PS_TIMEOUT
    }
}

/// Run bounded process inspection with the first of `programs` that starts.
pub(crate) async fn list_processes_from(
    programs: &[PathBuf],
) -> Result<Vec<ProcessRow>, InspectError> {
    let mut spawned = None;
    let mut failures = Vec::new();
    for program in programs {
        let mut command = tokio::process::Command::new(program);
        command
            .args(["-axo", "pid=,ppid=,pgid=,stat=,lstart="])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        match super::spawn::spawn(&mut command) {
            Ok(child) => {
                spawned = Some(child);
                break;
            }
            Err(error) => failures.push((program.clone(), error)),
        }
    }
    let Some(child) = spawned else {
        return Err(not_started(&failures));
    };
    read_listing(child, PS_MAX_BYTES)
        .await
        .and_then(|s| validate_listing(&s, std::process::id()))
}

/// Read one listing from a started `ps`, within `PS_TIMEOUT` and `max` bytes,
/// and reap it whatever happens.
async fn read_listing(
    mut child: tokio::process::Child,
    max: usize,
) -> Result<String, InspectError> {
    let failed = |e: std::io::Error| InspectError::Failed(format!("ps: {e}"));
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| InspectError::Failed("ps: no stdout pipe".into()))?;
    let read = async {
        let mut bytes = Vec::new();
        let mut chunk = vec![0u8; PS_READ_CHUNK];
        loop {
            let n = stdout.read(&mut chunk).await.map_err(failed)?;
            if n == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..n]);
            if bytes.len() > max {
                return Err(InspectError::Failed("ps: output too large".into()));
            }
        }
        let status = child.wait().await.map_err(failed)?;
        if !status.success() {
            return Err(InspectError::Failed(format!(
                "ps failed (exit {:?})",
                status.code()
            )));
        }
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    };
    let result = match tokio::time::timeout(ps_timeout(), read).await {
        Ok(result) => result,
        Err(_) => Err(InspectError::TimedOut),
    };
    if result.is_err() {
        // The inspector is a direct child only. Never signal a process group
        // here, and reap even on timeout or max-buffer failures.
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
    result
}

/// The error for a listing program that could not be started. Every path
/// tried is named with its own error, in the order tried, so a refusal on one
/// path is not hidden behind "not found" on the next. The whole message stays
/// within [`MAX_ERROR_CHARS`], see [`bounded_failures`].
pub(crate) fn not_started(failures: &[(PathBuf, std::io::Error)]) -> InspectError {
    const OPEN: &str = "ps could not be started (";
    const CLOSE: &str = ")";
    let detail = if failures.is_empty() {
        "no ps program configured".to_string()
    } else {
        let entries: Vec<(String, String)> = failures
            .iter()
            .map(|(program, error)| (program.display().to_string(), error.to_string()))
            .collect();
        let budget = MAX_ERROR_CHARS - OPEN.chars().count() - CLOSE.chars().count();
        bounded_failures(&entries, budget)
    };
    InspectError::Unavailable(format!("{OPEN}{detail}{CLOSE}"))
}

/// What a shortened path or a cut text is marked with.
const OMITTED: &str = "...";

/// The fewest characters a shortened path keeps, mark included, so that it
/// still names the file. Pairs are left out before paths get shorter.
const MIN_SHORTENED_PATH: usize = 40;

/// Join `(path, error)` pairs as `path: error, path: error` within `budget`
/// characters. Error descriptions are kept whole: paths are shortened first
/// (to their last characters, after [`OMITTED`]), then trailing pairs are
/// left out and counted as `... and N more path(s)`. Only when a single
/// error cannot fit on its own is the text cut, and the cut is marked.
fn bounded_failures(entries: &[(String, String)], budget: usize) -> String {
    let len = |text: &str| text.chars().count();
    for kept in (1..=entries.len()).rev() {
        let shown = &entries[..kept];
        let note = match entries.len() - kept {
            0 => String::new(),
            1 => format!(", {OMITTED} and 1 more path"),
            n => format!(", {OMITTED} and {n} more paths"),
        };
        let fixed: usize = shown
            .iter()
            .map(|(_, error)| len(": ") + len(error))
            .sum::<usize>()
            + len(", ") * (kept - 1)
            + len(&note);
        let Some(for_paths) = budget.checked_sub(fixed) else {
            continue;
        };
        let Some(cap) = path_cap(shown.iter().map(|(path, _)| len(path)), for_paths) else {
            continue;
        };
        let joined = shown
            .iter()
            .map(|(path, error)| format!("{}: {error}", shorten(path, cap)))
            .collect::<Vec<_>>()
            .join(", ");
        return format!("{joined}{note}");
    }
    // Not even the first error fits whole. Cut it, and say so.
    let (path, error) = &entries[0];
    let mut text = format!("{path}: {error}");
    let more = entries.len() - 1;
    if more > 0 {
        text.push_str(&format!(
            " and {more} more path{}",
            if more == 1 { "" } else { "s" }
        ));
    }
    let keep = budget.saturating_sub(len(OMITTED));
    format!("{}{OMITTED}", clip(&text, keep))
}

/// The longest any path may be so that all of `lengths` fit in `room`
/// characters, with paths shorter than that kept whole. `None` when even a
/// [`MIN_SHORTENED_PATH`] characters of every long path do not fit. `usize::MAX` means no path
/// needs shortening.
fn path_cap(lengths: impl Iterator<Item = usize>, room: usize) -> Option<usize> {
    let mut sorted: Vec<usize> = lengths.collect();
    if sorted.iter().sum::<usize>() <= room {
        return Some(usize::MAX);
    }
    sorted.sort_unstable();
    let mut room = room;
    let mut count = sorted.len();
    for length in sorted {
        let share = room / count;
        if length > share {
            return (share >= MIN_SHORTENED_PATH).then_some(share);
        }
        room -= length;
        count -= 1;
    }
    Some(usize::MAX)
}

/// `path` as is when it is at most `cap` characters, otherwise its last
/// characters after [`OMITTED`], `cap` characters in all.
fn shorten(path: &str, cap: usize) -> String {
    let chars = path.chars().count();
    if chars <= cap {
        return path.to_string();
    }
    let tail: String = path.chars().skip(chars - (cap - OMITTED.len())).collect();
    format!("{OMITTED}{tail}")
}

/// Run one listing command, bounded as [`list_processes_from`] is.
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub(crate) async fn run_listing(
    mut command: tokio::process::Command,
    max: usize,
) -> Result<String, InspectError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let child = super::spawn::spawn(&mut command).map_err(|error| {
        let program = PathBuf::from(command.as_std().get_program());
        not_started(&[(program, error)])
    })?;
    read_listing(child, max).await
}

/// Run bounded process inspection with the system `ps`.
#[cfg(test)]
pub(crate) async fn list_processes() -> Result<Vec<ProcessRow>, InspectError> {
    list_processes_from(&system_programs()).await
}

/// Run one process listing the way cleanup does and say whether it gave a usable
/// table. This is the check `delega doctor` makes: when it fails, every cleanup
/// would be reported as not confirmed. The error is text and names no process.
/// It honours the same test-only variable as cleanup.
pub async fn check_inspection() -> Result<(), String> {
    list_processes_from(&system_programs())
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// Test-only seam for `acceptance/`: when this variable is set in the
/// environment of the process, whatever its value, no program is used to list
/// processes, so inspection is unavailable and cleanup is reported as not
/// confirmed. It cannot substitute a program, so it can only make the verdict
/// weaker. Nothing sets it outside that harness.
pub(crate) const NO_LISTING_ENV: &str = "DELEGA_TEST_NO_PROCESS_LISTING";

fn system_programs() -> Vec<PathBuf> {
    if std::env::var_os(NO_LISTING_ENV).is_some() {
        return Vec::new();
    }
    PS_PROGRAMS.iter().map(PathBuf::from).collect()
}

/// Parse a process listing and refuse one that cannot be right. A valid
/// listing is never empty, holds this process (`own_pid`), and has no line
/// that does not parse. Anything else is a failed inspection, never "nothing
/// found": a listing that shows no descendants must not confirm cleanup.
pub(crate) fn validate_listing(
    stdout: &str,
    own_pid: u32,
) -> Result<Vec<ProcessRow>, InspectError> {
    let lines = stdout.lines().filter(|l| !l.trim().is_empty()).count();
    let rows = parse_rows(stdout);
    if rows.is_empty() {
        return Err(InspectError::Failed(
            "the process listing held no rows".into(),
        ));
    }
    if rows.len() < lines {
        return Err(InspectError::Failed(format!(
            "the process listing had {} lines that could not be parsed",
            lines - rows.len()
        )));
    }
    if !rows
        .iter()
        .any(|row| u32::try_from(row.pid).is_ok_and(|pid| pid == own_pid))
    {
        return Err(InspectError::Failed(
            "the process listing does not contain this process".into(),
        ));
    }
    Ok(rows)
}

/// Source of process rows; injectable for deterministic tests.
pub(crate) type Lister = Arc<
    dyn Fn() -> Pin<Box<dyn Future<Output = Result<Vec<ProcessRow>, InspectError>> + Send>>
        + Send
        + Sync,
>;

pub(crate) fn system_lister() -> Lister {
    lister_for(system_programs())
}

/// A lister that runs the first of `programs` that starts. Tests point it at a
/// missing program to exercise unavailable inspection.
pub(crate) fn lister_for(programs: Vec<PathBuf>) -> Lister {
    Arc::new(move || {
        let programs = programs.clone();
        Box::pin(async move { list_processes_from(&programs).await })
    })
}

/// Constant-space inspection receipts, shared outside the async tree lock.
/// Completion means the inspector returned; successes count usable tables only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InspectionStats {
    /// Inspections started.
    pub attempts: u64,
    /// Inspections that returned a usable table.
    pub successes: u64,
    /// Inspections that failed, including timeouts.
    pub failures: u64,
    /// Inspections that timed out.
    pub timeouts: u64,
    /// Duration of the latest inspection, in milliseconds.
    pub last_duration_ms: Option<u64>,
    /// Duration of the slowest inspection, in milliseconds.
    pub max_duration_ms: u64,
    /// The latest failure, clipped.
    pub last_error: Option<String>,
    /// Whether the child's own start identity was ever observed.
    pub root_identity_observed: bool,
    /// How many descendant identities are retained.
    pub retained_descendant_identities: usize,
    /// Rows that were alive in a listing but gone when their session was read.
    /// They are not counted as members: a process that short-lived is
    /// usually unrelated, and a relay of them is a known gap.
    pub vanished_rows: u64,
    /// Readings of the run marker taken (a kept run's sweeps only).
    pub marker_readings: u64,
    /// Marked processes found outside the group, the session and the tracked
    /// descendants, summed over those readings.
    pub marked_rows: u64,
    /// Signals not sent because the process could not be verified (Linux: no
    /// pidfd, or no recorded start time). Any makes the verdict `Unconfirmed`.
    pub unverified_signals: u64,
}

/// The shared receipts: the public numbers plus the instant of the latest
/// success, which stays internal.
#[derive(Debug, Default)]
pub(crate) struct Observations {
    pub stats: InspectionStats,
    pub last_success: Option<Instant>,
    /// Of `stats.unverified_signals`, those meant for a process found only by
    /// the run marker. Such a process need not be a descendant (a process
    /// started through a service carries the marker too), so the keeper's
    /// final census, which covers its own tree only, cannot supersede them.
    pub unverified_marked: u64,
}

pub(crate) type SharedObservations = Arc<Mutex<Observations>>;
pub(crate) type Warnings = Arc<Mutex<Vec<String>>>;

/// The tracked descendants alive at the latest snapshot, each with its start
/// time in clock ticks where the system gives one.
pub(crate) type Mirror = Arc<Mutex<Vec<(i32, Option<u64>)>>>;

/// Add a warning once.
pub(crate) fn push_warning(warnings: &Warnings, warning: String) {
    let mut guard = warnings.lock().unwrap_or_else(|e| e.into_inner());
    if !guard.contains(&warning) {
        guard.push(warning);
    }
}

/// Whether the child's pid still names its group and session, shared between
/// the owner's group signals, the tree's member signals and the keeper link,
/// which releases it when the keeper says it reaps the child on its own or
/// its end closes. Signals that rely on the pin are sent while holding the
/// lock, so none is in flight once it is released.
#[derive(Debug)]
pub(crate) struct GroupPin(Mutex<bool>);

impl Default for GroupPin {
    fn default() -> Self {
        Self(Mutex::new(true))
    }
}

impl GroupPin {
    pub(crate) fn lock(&self) -> std::sync::MutexGuard<'_, bool> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn release(&self) {
        *self.lock() = false;
    }

    pub(crate) fn pinned(&self) -> bool {
        *self.lock()
    }
}

/// How a pid was found, which decides what it must still be when it is
/// signalled.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
enum Found {
    /// A member of the child's group: its group id must still be the child's.
    Group,
    /// A member of the child's session: its session id must still be the
    /// child's.
    Session,
    /// A tracked descendant: its start time, when recorded, must match.
    Tracked,
    /// Found by the run marker: its start time, when recorded, must match,
    /// and with `true` it must still be stopped.
    Marked(Option<u64>, bool),
}

/// Whether a pid may ever be owned: not the child (it seeds the set), not init
/// or the kernel, not the process that runs this code, and not the keeper.
fn adoptable(child: i32, keeper: Option<i32>, pid: i32) -> bool {
    pid > 1
        && pid != child
        && Some(pid) != keeper
        && u32::try_from(pid).is_ok_and(|p| p != std::process::id())
}

fn clip(text: &str, chars: usize) -> String {
    text.chars().take(chars).collect()
}
/// One listing and the session of each of its live rows that is not in the
/// child's group, read once so adoption and classification agree.
pub(crate) struct Snapshot {
    pub rows: Vec<ProcessRow>,
    sessions: HashMap<i32, sys::SessionLookup>,
    /// Processes that carry the run marker, when it was read.
    marked: Vec<MarkedRow>,
}

pub(crate) struct ProcessTree {
    pid: i32,
    root_started: Option<String>,
    known: HashMap<i32, String>,
    /// Shared outside the async tree lock so a cleanup-failure report never
    /// loses warnings while a capture holds the tree.
    pub warnings: Warnings,
    pub observations: SharedObservations,
    /// The tracked descendants alive at the latest snapshot, readable without
    /// the tree lock, for the paths that cannot run a scan (an owner that is
    /// dropped, a report written after the owner was lost).
    mirror: Mirror,
    lister: Lister,
    enabled: bool,
    /// The keeper, a direct child of this process and the program's parent.
    /// Its children are owned; it never is.
    keeper: Option<i32>,
    /// The run marker's own value, for a kept run.
    marker: Option<String>,
    /// Marked processes this tree stopped, with the start time the marker
    /// reading showed. Only these may receive SIGKILL, and only when a later
    /// reading shows them still stopped with that start time.
    marked_stopped: HashMap<i32, String>,
    /// Processes outside the group that a freeze has stopped.
    frozen: HashSet<i32>,
    /// The latest freeze snapshot, which [`ProcessTree::kill_frozen`] signals.
    last_freeze: Option<(Snapshot, SweepView)>,
    /// Whether the child's pid still names its group and session: true while
    /// the child is unreaped by this process or by the keeper. When the keeper
    /// is lost it is false, and only recorded identities are followed.
    pinned: bool,
    /// The pin shared with the keeper link, for a kept run.
    pin: Option<Arc<GroupPin>>,
    /// Start times in clock ticks of tracked processes (Linux), checked
    /// through a pidfd when they are signalled.
    ticks: HashMap<i32, u64>,
    /// The child's start time in ticks, read while it was pinned.
    root_ticks: Option<u64>,
    /// Test seam: every signal to a single process is refused.
    #[cfg(test)]
    pub(crate) refuse_signals: bool,
    /// Test seam: created whenever a refused signal is recorded.
    #[cfg(test)]
    pub(crate) refusal_file: Option<std::path::PathBuf>,
}

/// What one cleanup sweep found alive. Zombies are not alive: they have run to
/// their end and wait for a parent to collect them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SweepView {
    /// Members of the child's process group.
    pub group: Vec<i32>,
    /// Processes whose session id is the child's, outside the group.
    pub session: Vec<i32>,
    /// Tracked descendants with a matching start identity, outside the group
    /// and the session.
    pub tracked: Vec<i32>,
    /// Processes that carry the run marker and are none of the above.
    pub marked: Vec<i32>,
    /// Rows alive in the listing but gone, or unreadable, when their session
    /// was read. Counted, not treated as members.
    pub vanished: usize,
}

impl SweepView {
    /// Whether the sweep found nothing alive.
    pub fn is_empty(&self) -> bool {
        self.group.is_empty()
            && self.session.is_empty()
            && self.tracked.is_empty()
            && self.marked.is_empty()
    }

    /// Every pid found, once.
    pub fn all(&self) -> Vec<i32> {
        let mut all = self.owned();
        all.extend(&self.marked);
        all.sort_unstable();
        all.dedup();
        all
    }

    /// The pids found by membership or ancestry, once: what may be signalled
    /// directly.
    pub fn owned(&self) -> Vec<i32> {
        let mut all: Vec<i32> = self
            .group
            .iter()
            .chain(&self.session)
            .chain(&self.tracked)
            .copied()
            .collect();
        all.sort_unstable();
        all.dedup();
        all
    }
}

impl ProcessTree {
    pub fn new(pid: i32, lister: Lister, enabled: bool) -> Self {
        Self {
            pid,
            root_started: None,
            known: HashMap::new(),
            warnings: Default::default(),
            observations: Default::default(),
            mirror: Default::default(),
            lister,
            enabled,
            keeper: None,
            marker: None,
            marked_stopped: HashMap::new(),
            frozen: HashSet::new(),
            last_freeze: None,
            pinned: true,
            pin: None,
            ticks: HashMap::new(),
            root_ticks: None,
            #[cfg(test)]
            refuse_signals: false,
            #[cfg(test)]
            refusal_file: None,
        }
    }

    /// The pin shared with the keeper link.
    #[must_use]
    pub fn with_pin(mut self, pin: Arc<GroupPin>) -> Self {
        self.pin = Some(pin);
        self
    }

    /// Record `pid` as tracked with this start, and its start time in ticks
    /// where the system gives one.
    /// Record `row` as tracked. On Linux its start time in ticks is recorded
    /// too, read from `/proc` when the row is classified, but only when that
    /// read shows the process now holding the pid is ours: it is in the
    /// child's session or group (pinned), or its parent is the keeper (which
    /// has only processes of this tree as children), the child itself
    /// (pinned), or the parent the listing showed, whose own recorded start
    /// time still matches. Otherwise no start
    /// time is recorded, and a signal that would rely on one is refused, which
    /// makes the verdict `Unconfirmed`.
    fn track(&mut self, row: &ProcessRow) {
        // Tried again on a later listing while the listing still shows the
        // same process (same start), so one read that could not verify it is
        // not final.
        let same = self
            .known
            .get(&row.pid)
            .is_none_or(|started| *started == row.started);
        if same
            && !self.ticks.contains_key(&row.pid)
            && let Some(ticks) = self.verified_ticks(row)
        {
            self.ticks.insert(row.pid, ticks);
        }
        if !same {
            self.ticks.remove(&row.pid);
        }
        self.known.insert(row.pid, row.started.clone());
    }

    #[cfg(target_os = "linux")]
    fn verified_ticks(&self, row: &ProcessRow) -> Option<u64> {
        // The pin's lock is held across the read: the keeper reaps the child
        // only after the owner answered `released`, which takes this lock, so
        // the child's pid cannot be freed (and its session or group reused)
        // between the check and the read.
        let held = self.pin.as_ref().map(|pin| pin.lock());
        let pinned = self.pinned && held.as_ref().is_none_or(|pinned| **pinned);
        let stat = sys::proc_stat(row.pid)?;
        // A member of the child's session or group is ours by that alone
        // while the child's pid pins them; this covers a member first seen
        // after the child exited and it was reparented.
        let member = pinned && (stat.session == self.pid || stat.group == self.pid);
        let parent_is_ours = Some(stat.parent) == self.keeper
            || (stat.parent == self.pid && pinned)
            || (stat.parent == row.parent
                && self
                    .ticks
                    .get(&row.parent)
                    .is_some_and(|t| sys::start_ticks(row.parent) == Some(*t)));
        (member || parent_is_ours).then_some(stat.start_ticks)
    }

    #[cfg(not(target_os = "linux"))]
    fn verified_ticks(&self, _row: &ProcessRow) -> Option<u64> {
        None
    }

    /// The child's pid no longer names its group and session (the keeper that
    /// held it unreaped is gone). From now on the child itself is followed by
    /// its recorded start time, like a tracked descendant, and nothing is a
    /// member by its group or session id.
    pub fn unpin(&mut self) {
        if self.pinned {
            self.pinned = false;
            if let Some(started) = self.root_started.clone() {
                self.known.insert(self.pid, started);
                if let Some(ticks) = self.root_ticks {
                    self.ticks.insert(self.pid, ticks);
                }
            }
        }
    }

    /// The keeper's pid: its children are owned, and it never is.
    #[must_use]
    pub fn with_keeper(mut self, keeper: i32) -> Self {
        self.keeper = Some(keeper);
        self
    }

    /// The run marker's value: sweeps also find the processes that carry it.
    #[must_use]
    pub fn with_marker(mut self, token: String) -> Self {
        self.marker = Some(token);
        self
    }

    /// Publish the live tracked descendants to `mirror` after each snapshot.
    #[must_use]
    pub fn with_mirror(mut self, mirror: Mirror) -> Self {
        self.mirror = mirror;
        self
    }

    fn warn(&self, error: &InspectError) {
        push_warning(
            &self.warnings,
            format!("Descendant tracking unavailable: {error}"),
        );
    }

    /// Bounded inspection; a failed snapshot never proves that a process exited.
    async fn rows(&self) -> Result<Vec<ProcessRow>, InspectError> {
        let started = Instant::now();
        {
            let mut obs = self.observations.lock().unwrap_or_else(|e| e.into_inner());
            obs.stats.attempts += 1;
        }
        // The production inspector owns its timeout and async reap. Run it as
        // its own task so cancelling this future never cancels it, and so a
        // panicking inspector, or a panicking factory, is caught here and
        // reported as a failure.
        let lister = self.lister.clone();
        let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| lister())) {
            Ok(future) => match tokio::spawn(future).await {
                Ok(rows) => rows,
                Err(_) => Err(InspectError::Failed("process inspector panicked".into())),
            },
            Err(_) => Err(InspectError::Failed(
                "process inspector factory panicked".into(),
            )),
        };
        let mut obs = self.observations.lock().unwrap_or_else(|e| e.into_inner());
        let finished = Instant::now();
        let duration = finished.saturating_duration_since(started).as_millis() as u64;
        obs.stats.last_duration_ms = Some(duration);
        obs.stats.max_duration_ms = obs.stats.max_duration_ms.max(duration);
        match &result {
            Ok(_) => {
                obs.stats.successes += 1;
                obs.last_success = Some(finished);
            }
            Err(error) => {
                obs.stats.failures += 1;
                if *error == InspectError::TimedOut {
                    obs.stats.timeouts += 1;
                }
                obs.stats.last_error = Some(clip(&error.to_string(), MAX_ERROR_CHARS));
            }
        }
        result
    }

    /// The session of every live row outside the child's group.
    fn read_sessions(&self, rows: &[ProcessRow]) -> HashMap<i32, sys::SessionLookup> {
        rows.iter()
            .filter(|r| !r.zombie && r.group != self.pid)
            .map(|r| (r.pid, sys::session_lookup(r.pid)))
            .collect()
    }

    /// Adopt ancestry from every inspection, including cleanup and signal
    /// snapshots. The owned set is seeded with the child, the live members of
    /// its group and session, and the tracked processes with a matching start
    /// time, and grows by every process whose parent is owned. A process is
    /// never owned by its number alone, by being a child of pid 1, or by
    /// anything but one of these links.
    ///
    /// The group and session seeds need the child's own row with its recorded
    /// start time: while the child is unreaped its pid cannot name another
    /// group or session, and a listing that does not show it is not trusted for
    /// them.
    fn adopt(&mut self, rows: &[ProcessRow], sessions: &HashMap<i32, sys::SessionLookup>) {
        if !self.enabled {
            return;
        }
        let root_pid = self.pid;
        let keeper = self.keeper;
        // The keeper is a direct child of this process, so its pid cannot
        // name another process while the owner holds it unreaped. Its row
        // must say so before its children are taken as owned.
        let keeper_seed = keeper.filter(|k| {
            rows.iter().any(|r| {
                r.pid == *k
                    && !r.zombie
                    && u32::try_from(r.parent).is_ok_and(|p| p == std::process::id())
            })
        });
        let root = rows.iter().find(|row| row.pid == self.pid);
        if self.root_started.is_none() {
            self.root_started = root.map(|r| r.started.clone());
            // The child is pinned now (unreaped), so its pid is its own.
            if self.root_started.is_some() && self.pinned {
                self.root_ticks = sys::start_ticks(self.pid);
                if let Some(ticks) = self.root_ticks {
                    self.ticks.insert(self.pid, ticks);
                }
            }
        }
        let mut owned: HashSet<i32> = HashSet::new();
        if let Some(root) = root
            && self.pinned
            && Some(&root.started) == self.root_started.as_ref()
        {
            owned.insert(self.pid);
            for row in rows
                .iter()
                .filter(|r| !r.zombie && adoptable(root_pid, keeper, r.pid))
            {
                let member = row.group == self.pid
                    || sessions.get(&row.pid) == Some(&sys::SessionLookup::In(self.pid));
                if member {
                    owned.insert(row.pid);
                    self.track(row);
                }
            }
        }
        for row in rows {
            if self.known.get(&row.pid) == Some(&row.started) {
                owned.insert(row.pid);
                if !row.zombie && !self.ticks.contains_key(&row.pid) {
                    self.track(row);
                }
            }
        }
        for _ in 0..rows.len() {
            let before = owned.len();
            for row in rows {
                if adoptable(root_pid, keeper, row.pid)
                    && (owned.contains(&row.parent) || Some(row.parent) == keeper_seed)
                    && !owned.contains(&row.pid)
                {
                    owned.insert(row.pid);
                    self.track(row);
                }
            }
            if before == owned.len() {
                break;
            }
        }
    }

    async fn snapshot(&mut self, with_marker: bool) -> Result<Snapshot, InspectError> {
        let token = self.marker.clone().filter(|_| with_marker);
        // Without `/proc` the marker reading is a listing of its own that does
        // not need the rows, so the two run at once.
        #[cfg(not(target_os = "linux"))]
        let (rows, early) = match &token {
            Some(token) => {
                let (rows, marked) = tokio::join!(self.rows(), super::marker::marked(&[], token));
                (rows?, Some(marked))
            }
            None => (self.rows().await?, None),
        };
        #[cfg(target_os = "linux")]
        let (rows, early): (_, Option<Result<Vec<MarkedRow>, InspectError>>) =
            (self.rows().await?, None);
        // The keeper may have let go of the child while the listing ran.
        if self.pin.as_ref().is_some_and(|pin| !pin.pinned()) {
            self.unpin();
        }
        let sessions = self.read_sessions(&rows);
        self.adopt(&rows, &sessions);
        let marked = match token {
            Some(token) => {
                let found = match early {
                    Some(found) => found,
                    None => super::marker::marked(&rows, &token).await,
                };
                let mut obs = self.observations.lock().unwrap_or_else(|e| e.into_inner());
                obs.stats.marker_readings += 1;
                match found {
                    Ok(found) => found
                        .into_iter()
                        .filter(|m| {
                            Some(m.pid) != self.keeper
                                && u32::try_from(m.pid).is_ok_and(|p| p != std::process::id())
                        })
                        .collect(),
                    Err(error) => {
                        obs.stats.failures += 1;
                        obs.stats.last_error =
                            Some(clip(&format!("marker reading: {error}"), MAX_ERROR_CHARS));
                        return Err(error);
                    }
                }
            }
            None => Vec::new(),
        };
        let tracked: Vec<(i32, Option<u64>)> = rows
            .iter()
            .filter(|r| !r.zombie && self.known.get(&r.pid) == Some(&r.started))
            .map(|r| (r.pid, self.ticks.get(&r.pid).copied()))
            .collect();
        *self.mirror.lock().unwrap_or_else(|e| e.into_inner()) = tracked;
        let mut obs = self.observations.lock().unwrap_or_else(|e| e.into_inner());
        obs.stats.root_identity_observed = self.root_started.is_some();
        obs.stats.retained_descendant_identities = self.known.len();
        drop(obs);
        Ok(Snapshot {
            rows,
            sessions,
            marked,
        })
    }

    /// Classify the live processes in one table. The child is the session
    /// leader, so its pid is the session id; a process outside its group that
    /// still reports that session id was started by the child and never left.
    /// The session id is read per pid (`/proc` on Linux, `getsid` elsewhere),
    /// because `ps` keywords for it differ between systems (macOS reports 0 for
    /// every process). A session of 0 is a real answer that is never the child's.
    fn view(&self, snapshot: &Snapshot) -> SweepView {
        let mut view = SweepView::default();
        for row in snapshot.rows.iter().filter(|r| !r.zombie) {
            if !self.pinned {
                if self.known.get(&row.pid) == Some(&row.started) {
                    view.tracked.push(row.pid);
                }
                continue;
            }
            if row.group == self.pid {
                view.group.push(row.pid);
                continue;
            }
            let tracked = self.known.get(&row.pid) == Some(&row.started);
            match snapshot.sessions.get(&row.pid) {
                Some(sys::SessionLookup::In(session)) if *session == self.pid => {
                    view.session.push(row.pid);
                }
                Some(sys::SessionLookup::In(_)) => {
                    if tracked {
                        view.tracked.push(row.pid);
                    }
                }
                Some(sys::SessionLookup::Gone | sys::SessionLookup::Unknown) | None => {
                    if tracked {
                        view.tracked.push(row.pid);
                    } else {
                        view.vanished += 1;
                    }
                }
            }
        }
        let owned: HashSet<i32> = view.owned().into_iter().collect();
        for m in snapshot
            .marked
            .iter()
            .filter(|m| m.pid != self.pid && !owned.contains(&m.pid))
        {
            // Without `/proc` the marker reading is a listing of its own, so it
            // can hold a member of the child's session that started after the
            // ordinary listing. Such a process is a member, signalled as one
            // (TERM in the polite stop), not stopped as an escaped process:
            // stopping it could hold up the child's own handling of TERM. Every
            // group member is in the session, so the session read covers both.
            if self.pinned && sys::session_lookup(m.pid) == sys::SessionLookup::In(self.pid) {
                view.session.push(m.pid);
            } else {
                view.marked.push(m.pid);
            }
        }
        view
    }

    /// One sweep from a fresh snapshot, adopting descendants still attached to
    /// an identified owned process. With `signal`, everything the sweep found
    /// is signalled from that same snapshot; a TERM is followed by CONT so a
    /// stopped process receives it.
    ///
    /// A process found only by the run marker is never signalled by number
    /// alone: it is stopped (SIGSTOP) when first seen, and receives SIGKILL
    /// (then SIGCONT) from a KILL sweep only when that sweep's reading shows it
    /// still stopped with the start time it had when it was stopped. A stopped
    /// process cannot exit by itself, so its pid cannot have been reused.
    pub async fn sweep(&mut self, signal: Option<Signal>) -> Result<SweepView, InspectError> {
        let snapshot = self.snapshot(true).await?;
        let view = self.view(&snapshot);
        self.note_view(&view);
        if let Some(signal) = signal {
            for pid in view.owned() {
                let found = Self::found(&view, pid);
                self.signal_found(pid, found, signal);
                if signal == Signal::TERM {
                    self.signal_found(pid, found, Signal::CONT);
                }
            }
            for marked in snapshot
                .marked
                .iter()
                .filter(|m| view.marked.contains(&m.pid))
            {
                self.signal_marked(marked, signal == Signal::KILL);
            }
        }
        Ok(view)
    }

    fn note_view(&self, view: &SweepView) {
        let mut obs = self.observations.lock().unwrap_or_else(|e| e.into_inner());
        obs.stats.vanished_rows += view.vanished as u64;
        obs.stats.marked_rows += view.marked.len() as u64;
    }

    fn found(view: &SweepView, pid: i32) -> Found {
        if view.group.contains(&pid) {
            Found::Group
        } else if view.session.contains(&pid) {
            Found::Session
        } else {
            Found::Tracked
        }
    }

    /// Signal one process if it is still what it was found as. On Linux the
    /// check is made through a pidfd (see [`sys::signal_checked`]), so a pid
    /// reused since the listing is never signalled. Elsewhere a session member
    /// is checked again just before the signal and the rest go by pid, the
    /// residual in the module documentation. A group or session member is
    /// signalled only while the child's pid pins them, holding the pin's lock.
    fn signal_found(&self, pid: i32, found: Found, signal: Signal) {
        #[cfg(test)]
        if self.refuse_signals {
            self.note_refused(found);
            return;
        }
        let member = matches!(found, Found::Group | Found::Session);
        let held = self.pin.as_ref().map(|pin| pin.lock());
        if member && (!self.pinned || held.as_ref().is_some_and(|pinned| !**pinned)) {
            return;
        }
        #[cfg(target_os = "linux")]
        let result = {
            let child = self.pid;
            let ticks = match found {
                Found::Group | Found::Session => None,
                Found::Tracked => self.ticks.get(&pid).copied(),
                Found::Marked(ticks, _) => ticks,
            };
            // A tracked or marked process without a recorded start time
            // cannot be verified at delivery: it is not signalled. It is a
            // refusal only while something may still hold its pid; a pid
            // that names no live process means it has exited (a relay
            // generation that ended between the listing and here).
            if matches!(found, Found::Tracked | Found::Marked(..)) && ticks.is_none() {
                Ok(if sys::pid_gone(pid) {
                    sys::Delivery::Gone
                } else {
                    sys::Delivery::Refused
                })
            } else {
                sys::signal_checked(pid, signal, |stat| match found {
                    Found::Group => stat.group == child,
                    Found::Session => stat.session == child,
                    Found::Tracked => ticks == Some(stat.start_ticks),
                    Found::Marked(_, stopped) => {
                        ticks == Some(stat.start_ticks)
                            && (!stopped || matches!(stat.state, 'T' | 't'))
                    }
                })
            }
        };
        #[cfg(not(target_os = "linux"))]
        let result = {
            if matches!(found, Found::Session)
                && sys::session_lookup(pid) != sys::SessionLookup::In(self.pid)
            {
                return;
            }
            sys::signal_checked(pid, signal, |_| true)
        };
        drop(held);
        match result {
            Ok(sys::Delivery::Sent | sys::Delivery::Gone) => {}
            Ok(sys::Delivery::Refused) => self.note_refused(found),
            Err(error) => push_warning(
                &self.warnings,
                format!("Descendant signal failed: {error:?}"),
            ),
        }
    }

    /// Count a signal that was not sent because its process could not be
    /// verified, keeping those meant for a marked process apart.
    fn note_refused(&self, found: Found) {
        let mut obs = self.observations.lock().unwrap_or_else(|e| e.into_inner());
        obs.stats.unverified_signals += 1;
        if matches!(found, Found::Marked(..)) {
            obs.unverified_marked += 1;
        }
        #[cfg(test)]
        if let Some(path) = &self.refusal_file {
            let _ = std::fs::write(path, "refused\n");
        }
    }

    /// Stop a marked process, or kill one this tree stopped earlier that is
    /// still stopped with the same start time. Returns whether it was new.
    fn signal_marked(&mut self, marked: &MarkedRow, kill: bool) -> bool {
        let known = self.marked_stopped.get(&marked.pid) == Some(&marked.started);
        if known && marked.stopped {
            if kill {
                let found = Found::Marked(marked.ticks, true);
                self.signal_found(marked.pid, found, Signal::KILL);
                self.signal_found(marked.pid, Found::Marked(marked.ticks, false), Signal::CONT);
            }
            return false;
        }
        self.signal_found(marked.pid, Found::Marked(marked.ticks, false), Signal::STOP);
        self.marked_stopped
            .insert(marked.pid, marked.started.clone());
        true
    }

    /// One round of a freeze: a fresh snapshot, and SIGSTOP to every process
    /// it finds outside the child's group (owned by ancestry or session, or
    /// marked) that this freeze has not already stopped. Returns how many were
    /// new; a round that finds none means everything found is stopped and
    /// cannot start anything more. The group is left to the group signal.
    pub async fn freeze_round(&mut self) -> Result<usize, InspectError> {
        let snapshot = self.snapshot(true).await?;
        let view = self.view(&snapshot);
        self.note_view(&view);
        let mut new = 0;
        let outside: Vec<i32> = view.session.iter().chain(&view.tracked).copied().collect();
        for pid in outside {
            let stopped = snapshot.rows.iter().any(|r| r.pid == pid && r.stopped);
            if stopped && self.frozen.contains(&pid) {
                continue;
            }
            self.signal_found(pid, Self::found(&view, pid), Signal::STOP);
            self.frozen.insert(pid);
            new += 1;
        }
        let marked: Vec<MarkedRow> = snapshot
            .marked
            .iter()
            .filter(|m| view.marked.contains(&m.pid))
            .cloned()
            .collect();
        for m in &marked {
            if self.signal_marked(m, false) {
                new += 1;
            }
        }
        self.last_freeze = Some((snapshot, view));
        Ok(new)
    }

    /// End a freeze: SIGKILL, then SIGCONT, to what the latest freeze round
    /// found outside the group. Owned processes are signalled as a sweep
    /// signals them; marked ones only when that round showed them stopped by
    /// this tree with the same start time.
    pub fn kill_frozen(&mut self) -> SweepView {
        let Some((snapshot, view)) = self.last_freeze.take() else {
            return SweepView::default();
        };
        for pid in view
            .session
            .iter()
            .chain(&view.tracked)
            .copied()
            .collect::<Vec<_>>()
        {
            let found = Self::found(&view, pid);
            self.signal_found(pid, found, Signal::KILL);
            self.signal_found(pid, found, Signal::CONT);
        }
        for m in snapshot
            .marked
            .iter()
            .filter(|m| view.marked.contains(&m.pid))
        {
            if self.marked_stopped.get(&m.pid) == Some(&m.started) && m.stopped {
                self.signal_found(m.pid, Found::Marked(m.ticks, true), Signal::KILL);
                self.signal_found(m.pid, Found::Marked(m.ticks, false), Signal::CONT);
            }
        }
        self.frozen.clear();
        view
    }

    #[cfg(test)]
    pub async fn cleanup_state(&mut self) -> Result<SweepView, InspectError> {
        self.sweep(None).await
    }

    #[cfg(test)]
    pub async fn group_live(&mut self) -> Result<bool, InspectError> {
        self.sweep(None).await.map(|view| !view.group.is_empty())
    }

    #[cfg(test)]
    pub async fn alive_known(&mut self) -> Result<usize, InspectError> {
        if !self.enabled {
            return Ok(0);
        }
        self.sweep(None).await.map(|view| view.tracked.len())
    }

    #[cfg(test)]
    pub fn known_count(&self) -> usize {
        self.known.len()
    }

    /// Capture descendants of the leader, retaining reparented ones.
    pub async fn capture(&mut self) -> CaptureOutcome {
        if !self.enabled {
            return CaptureOutcome::TrackingDisabled;
        }
        match self.snapshot(false).await {
            Ok(_) => CaptureOutcome::Success,
            Err(error) => {
                self.warn(&error);
                if error == InspectError::TimedOut {
                    CaptureOutcome::Timeout
                } else {
                    CaptureOutcome::Failed
                }
            }
        }
    }
}

/// The result of one periodic capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaptureOutcome {
    Success,
    Timeout,
    Failed,
    TrackingDisabled,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    fn failing(error: InspectError) -> Lister {
        Arc::new(move || {
            let error = error.clone();
            Box::pin(async move { Err(error) })
        })
    }

    const ROW: &str = "{pid} 1 {pid} S Mon Nov 18 10:00:00 2026\n";

    fn row(pid: u32) -> String {
        ROW.replace("{pid}", &pid.to_string())
    }

    #[test]
    fn a_row_whose_session_reads_as_zero_is_not_owned() {
        let lister: Lister = Arc::new(|| Box::pin(async { Ok(Vec::new()) }));
        let mut tree = ProcessTree::new(10, lister, true);
        let rows = parse_rows(
            "10 1 10 S Mon Nov 18 10:00:00 2026\n\
             2 0 0 S Mon Nov 18 10:00:00 2026\n\
             40 1 40 S Mon Nov 18 10:00:00 2026\n",
        );
        let sessions = HashMap::from([
            (2, sys::SessionLookup::In(0)),
            (40, sys::SessionLookup::In(0)),
        ]);
        tree.adopt(&rows, &sessions);
        assert!(!tree.known.contains_key(&2));
        assert!(!tree.known.contains_key(&40));
        let view = tree.view(&Snapshot {
            rows,
            sessions,
            marked: Vec::new(),
        });
        assert!(view.session.is_empty() && view.tracked.is_empty());
        assert!(view.group.contains(&10));
    }

    #[test]
    fn a_marked_process_in_the_childs_session_is_a_member_not_an_escaped_process() {
        // The "child" here is this process's own session leader, so this
        // process is a member of its session; it appears only in the marker
        // reading, as a process started after the ordinary listing would.
        let own = i32::try_from(std::process::id()).expect("a pid");
        let Some(session) = sys::session_of(own).filter(|s| *s > 1 && *s != own) else {
            eprintln!("this process leads its own session or it cannot be read; skipped");
            return;
        };
        let lister: Lister = Arc::new(|| Box::pin(async { Ok(Vec::new()) }));
        let tree = ProcessTree::new(session, lister, true);
        let marked = vec![MarkedRow {
            pid: own,
            started: "now".into(),
            stopped: false,
            ticks: None,
        }];
        let view = tree.view(&Snapshot {
            rows: Vec::new(),
            sessions: HashMap::new(),
            marked,
        });
        assert_eq!(view.session, vec![own]);
        assert!(view.marked.is_empty());
    }

    /// Not run on macOS; runs in the Linux CI job. A member of the child's
    /// session gets its start time recorded when it is classified even when
    /// its parent cannot be verified (the leader exited and it was
    /// reparented), so the pidfd check can verify it when it is signalled.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_session_member_with_an_unverified_parent_still_gets_a_start_time() {
        let own = i32::try_from(std::process::id()).expect("a pid");
        let Some(session) = sys::session_of(own).filter(|s| *s > 1 && *s != own) else {
            eprintln!("this process leads its own session or it cannot be read; skipped");
            return;
        };
        let lister: Lister = Arc::new(|| Box::pin(async { Ok(Vec::new()) }));
        let tree = ProcessTree::new(session, lister, true);
        let row = ProcessRow {
            pid: own,
            // A parent with no recorded start time, as init after a reparent.
            parent: 1,
            started: "now".into(),
            group: own,
            zombie: false,
            stopped: false,
        };
        assert_eq!(tree.verified_ticks(&row), sys::start_ticks(own));
        assert!(tree.verified_ticks(&row).is_some());
    }

    /// Not run on macOS; runs in the Linux CI job. Once the pin shared with
    /// the keeper is released, membership no longer proves identity: a
    /// session member with an unverified parent gets no start time. And a
    /// release waits for a verification that holds the pin's lock.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_released_pin_stops_membership_from_proving_identity() {
        let own = i32::try_from(std::process::id()).expect("a pid");
        let Some(session) = sys::session_of(own).filter(|s| *s > 1 && *s != own) else {
            eprintln!("this process leads its own session or it cannot be read; skipped");
            return;
        };
        let lister: Lister = Arc::new(|| Box::pin(async { Ok(Vec::new()) }));
        let pin = Arc::new(GroupPin::default());
        let tree = ProcessTree::new(session, lister, true).with_pin(pin.clone());
        let row = ProcessRow {
            pid: own,
            parent: 1,
            started: "now".into(),
            group: own,
            zombie: false,
            stopped: false,
        };
        assert!(tree.verified_ticks(&row).is_some());
        // A release while a verification holds the lock waits for it.
        let held = pin.lock();
        let releasing = {
            let pin = pin.clone();
            std::thread::spawn(move || pin.release())
        };
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(*held, "the release went through while the lock was held");
        drop(held);
        releasing.join().expect("the release finished");
        assert!(tree.verified_ticks(&row).is_none());
    }

    #[test]
    fn an_empty_listing_is_a_failed_inspection_not_nothing_found() {
        for text in ["", "\n  \n"] {
            assert!(matches!(
                validate_listing(text, 10),
                Err(InspectError::Failed(_))
            ));
        }
    }

    #[test]
    fn a_listing_without_our_own_process_is_a_failed_inspection() {
        let text = format!("{}{}", row(1), row(2));
        match validate_listing(&text, 10) {
            Err(InspectError::Failed(message)) => assert!(message.contains("this process")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_listing_that_cannot_be_parsed_is_a_failed_inspection() {
        assert!(matches!(
            validate_listing("not a process table\n", 10),
            Err(InspectError::Failed(_))
        ));
        let mixed = format!("{}garbage line\n", row(10));
        match validate_listing(&mixed, 10) {
            Err(InspectError::Failed(message)) => assert!(message.contains("could not be parsed")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_listing_with_our_process_and_every_line_parsed_is_accepted() {
        let text = format!("{}{}", row(1), row(10));
        assert_eq!(validate_listing(&text, 10).map(|r| r.len()), Ok(2));
    }

    #[tokio::test]
    async fn observation_diagnostics_distinguish_failed_timeout_success_and_disabled() {
        let outcomes = Arc::new(Mutex::new(VecDeque::from([
            Err(InspectError::TimedOut),
            Err(InspectError::Failed("EPERM".to_string())),
            Ok(Vec::new()),
        ])));
        let lister: Lister = Arc::new(move || {
            let result = outcomes.lock().unwrap().pop_front().unwrap();
            Box::pin(async move { result })
        });
        let mut tree = ProcessTree::new(123, lister.clone(), true);
        assert_eq!(tree.capture().await, CaptureOutcome::Timeout);
        assert_eq!(tree.capture().await, CaptureOutcome::Failed);
        assert_eq!(tree.capture().await, CaptureOutcome::Success);
        {
            let obs = tree.observations.lock().unwrap();
            assert_eq!(obs.stats.attempts, 3);
            assert_eq!(obs.stats.successes, 1);
            assert_eq!(obs.stats.failures, 2);
            assert_eq!(obs.stats.timeouts, 1);
            assert_eq!(obs.stats.last_error.as_deref(), Some("EPERM"));
            assert!(obs.last_success.is_some());
        }
        let mut disabled = ProcessTree::new(123, lister, false);
        assert_eq!(disabled.capture().await, CaptureOutcome::TrackingDisabled);
        let obs = disabled.observations.lock().unwrap();
        assert_eq!(obs.stats.attempts, 0);
        assert_eq!(obs.stats.successes, 0);
        assert!(obs.last_success.is_none());
    }

    #[test]
    fn parses_ps_rows() {
        let rows = parse_rows(
            "  1     0 1 S Mon Nov 10 09:00:00 2026\n 42 1 42 Z Tue Nov 11 10:00:00 2026  \nbad\n",
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[1],
            ProcessRow {
                pid: 42,
                parent: 1,
                group: 42,
                zombie: true,
                stopped: false,
                started: "Tue Nov 11 10:00:00 2026".into()
            }
        );
    }

    fn scripted(frames: Vec<Vec<(i32, i32, &'static str)>>) -> Lister {
        let frames = Arc::new(Mutex::new(frames.into_iter()));
        Arc::new(move || {
            let next = frames.lock().unwrap().next().unwrap_or_default();
            Box::pin(async move {
                Ok(next
                    .into_iter()
                    .map(|(pid, parent, started)| ProcessRow {
                        pid,
                        parent,
                        // Not the child's group, so these count as tracked.
                        group: 99,
                        zombie: false,
                        stopped: false,
                        started: started.into(),
                    })
                    .collect())
            })
        })
    }

    #[tokio::test]
    async fn retains_reparented_descendants_and_rejects_recycled_pids() {
        let lister = scripted(vec![
            vec![(10, 1, "a"), (11, 10, "b"), (12, 11, "c"), (99, 1, "z")],
            // 11 reparented to 1; 12 recycled with a new start time.
            vec![(10, 1, "a"), (11, 1, "b"), (12, 1, "new")],
        ]);
        let mut tree = ProcessTree::new(10, lister, true);
        tree.capture().await;
        assert_eq!(tree.known_count(), 2);
        tree.capture().await;
        assert_eq!(tree.known.get(&11).map(String::as_str), Some("b"));
        assert_eq!(tree.known.get(&12).map(String::as_str), Some("c"));
    }

    #[tokio::test]
    async fn cleanup_adopts_new_ancestry_but_rejects_recycled_parent_identity() {
        let lister = scripted(vec![
            vec![(10, 1, "root"), (11, 10, "owned"), (12, 10, "old")],
            vec![
                (10, 1, "root"),
                (11, 1, "owned"),
                (13, 11, "new-child"),
                (12, 1, "reused"),
                (14, 12, "unowned"),
            ],
        ]);
        let mut tree = ProcessTree::new(10, lister, true);
        tree.capture().await;
        let view = tree.cleanup_state().await.unwrap();
        assert_eq!(view.tracked.len(), 2);
        assert_eq!(tree.known.get(&13).map(String::as_str), Some("new-child"));
        assert_eq!(tree.known.get(&12).map(String::as_str), Some("old"));
        assert!(!tree.known.contains_key(&14));
    }

    #[tokio::test]
    async fn cleanup_does_not_short_circuit_an_initially_empty_descendant_set() {
        let lister = scripted(vec![
            vec![(10, 1, "root")],
            vec![(10, 1, "root"), (11, 10, "late")],
        ]);
        let mut tree = ProcessTree::new(10, lister, true);
        tree.capture().await;
        assert_eq!(tree.known_count(), 0);
        assert_eq!(tree.alive_known().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn inspection_failure_is_a_warning() {
        let mut tree = ProcessTree::new(10, failing(InspectError::Failed("EPERM".into())), true);
        tree.capture().await;
        assert_eq!(
            tree.warnings.lock().unwrap().first().unwrap(),
            "Descendant tracking unavailable: EPERM"
        );
    }

    #[tokio::test]
    async fn a_panicking_inspector_is_a_failure_not_a_crash() {
        let lister: Lister = Arc::new(|| Box::pin(async { panic!("injected") }));
        let mut tree = ProcessTree::new(10, lister, true);
        assert_eq!(tree.capture().await, CaptureOutcome::Failed);
        assert_eq!(
            tree.observations
                .lock()
                .unwrap()
                .stats
                .last_error
                .as_deref(),
            Some("process inspector panicked")
        );
    }

    #[tokio::test]
    async fn zombies_are_exited_but_live_group_members_are_not() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let zombie = Arc::new(AtomicBool::new(false));
        let state = zombie.clone();
        let lister: Lister = Arc::new(move || {
            let dead = state.load(Ordering::SeqCst);
            Box::pin(async move {
                Ok(vec![
                    ProcessRow {
                        pid: 10,
                        parent: 1,
                        group: 10,
                        zombie: true,
                        stopped: false,
                        started: "leader".into(),
                    },
                    ProcessRow {
                        pid: 11,
                        parent: 10,
                        group: 10,
                        zombie: dead,
                        stopped: false,
                        started: "child".into(),
                    },
                ])
            })
        });
        let mut tree = ProcessTree::new(10, lister, true);
        tree.capture().await;
        assert!(tree.group_live().await.unwrap());
        assert_eq!(tree.cleanup_state().await.unwrap().all(), vec![11]);
        zombie.store(true, Ordering::SeqCst);
        assert!(!tree.group_live().await.unwrap());
        assert!(tree.cleanup_state().await.unwrap().is_empty());
        assert!(tree.sweep(Some(Signal::TERM)).await.unwrap().is_empty());
    }

    /// Rows as (pid, parent, group, started, zombie).
    fn table(rows: Vec<(i32, i32, i32, &'static str, bool)>) -> Vec<ProcessRow> {
        rows.into_iter()
            .map(|(pid, parent, group, started, zombie)| ProcessRow {
                pid,
                parent,
                group,
                zombie,
                stopped: false,
                started: started.into(),
            })
            .collect()
    }

    fn fixed(frames: Vec<Vec<ProcessRow>>) -> Lister {
        let frames = Arc::new(Mutex::new(frames.into_iter()));
        Arc::new(move || {
            let next = frames.lock().unwrap().next().unwrap_or_default();
            Box::pin(async move { Ok(next) })
        })
    }

    #[tokio::test]
    async fn children_of_a_group_member_are_adopted_after_the_leader_has_exited() {
        // The leader (10) is a zombie, so its own children were reparented. 11
        // is still in its group, and 12 is a child of 11 that left the group.
        // 13 is unrelated and so is its child 14; 15 is an orphan of init.
        let lister = fixed(vec![table(vec![
            (1, 0, 1, "init", false),
            (10, 1, 10, "root", true),
            (11, 1, 10, "member", false),
            (12, 11, 12, "grandchild", false),
            (13, 1, 13, "other", false),
            (14, 13, 13, "other-child", false),
            (15, 1, 15, "orphan", false),
        ])]);
        let mut tree = ProcessTree::new(10, lister, true);
        // The listing of the child while it ran gave it its identity.
        tree.root_started = Some("root".into());
        tree.capture().await;
        let mut known: Vec<_> = tree.known.keys().copied().collect();
        known.sort_unstable();
        assert_eq!(known, vec![11, 12]);
    }

    #[tokio::test]
    async fn nothing_is_adopted_from_a_listing_that_does_not_show_the_child() {
        // Without the child's own row its pid cannot be trusted to name its
        // group, so a row with that group is not a member.
        let lister = fixed(vec![table(vec![
            (1, 0, 1, "init", false),
            (11, 1, 10, "stranger", false),
            (12, 11, 12, "stranger-child", false),
        ])]);
        let mut tree = ProcessTree::new(10, lister, true);
        tree.root_started = Some("root".into());
        tree.capture().await;
        assert_eq!(tree.known_count(), 0);
    }

    #[tokio::test]
    async fn a_listing_that_shows_another_process_under_the_childs_pid_adopts_nothing() {
        // The pid of the child is held by a process with another start time.
        let lister = fixed(vec![table(vec![
            (1, 0, 1, "init", false),
            (10, 1, 10, "reused", false),
            (11, 10, 10, "its-child", false),
        ])]);
        let mut tree = ProcessTree::new(10, lister, true);
        tree.root_started = Some("root".into());
        tree.capture().await;
        assert_eq!(tree.known_count(), 0);
    }

    #[tokio::test]
    async fn a_recycled_pid_and_children_of_init_are_not_adopted() {
        let lister = fixed(vec![
            table(vec![
                (1, 0, 1, "init", false),
                (10, 1, 10, "root", false),
                (11, 10, 10, "member", false),
                (12, 11, 12, "grandchild", false),
            ]),
            // The member and the grandchild are gone. 11 and 12 are reused by
            // unrelated processes, one of them the parent of a new process.
            table(vec![
                (1, 0, 1, "init", false),
                (10, 1, 10, "root", true),
                (11, 1, 11, "reused-11", false),
                (12, 1, 12, "reused-12", false),
                (16, 12, 12, "child-of-reused", false),
                (17, 1, 17, "child-of-init", false),
            ]),
        ]);
        let mut tree = ProcessTree::new(10, lister, true);
        tree.capture().await;
        assert_eq!(tree.known_count(), 2);
        tree.capture().await;
        assert_eq!(tree.known.get(&11).map(String::as_str), Some("member"));
        assert_eq!(tree.known.get(&12).map(String::as_str), Some("grandchild"));
        assert!(!tree.known.contains_key(&16));
        assert!(!tree.known.contains_key(&17));
        assert!(!tree.known.contains_key(&1));
    }

    #[tokio::test]
    async fn the_process_that_runs_the_tracker_is_never_adopted() {
        let own = i32::try_from(std::process::id()).unwrap();
        let lister = fixed(vec![table(vec![
            (1, 0, 1, "init", false),
            (10, 1, 10, "root", false),
            (11, 10, 10, "member", false),
            (own, 11, 11, "ours", false),
        ])]);
        let mut tree = ProcessTree::new(10, lister, true);
        tree.capture().await;
        assert!(!tree.known.contains_key(&own));
    }

    #[tokio::test]
    async fn failed_inspection_is_not_an_empty_group() {
        let mut tree = ProcessTree::new(10, failing(InspectError::Failed("EPERM".into())), true);
        assert_eq!(
            tree.group_live().await.unwrap_err(),
            InspectError::Failed("EPERM".into())
        );
    }
}
