//! Descendant tracking.
//!
//! Some programs create another process group, so signalling the child's own
//! group does not reach everything they start. Ancestry is observed with `ps`
//! while the child is alive. Start times are kept with each pid, so a recycled
//! pid is never signalled by number alone.
//!
//! A descendant that was never observed (started and detached between two
//! scans, then orphaned) is not known to this tracker. That is a known gap, see
//! the module documentation of [`crate::process`].

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
use super::sys::{self, Signal, SignalError};

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
            let zombie = parts.next()?.starts_with('Z');
            let started = parts.collect::<Vec<_>>().join(" ");
            (!started.is_empty()).then_some(ProcessRow {
                pid,
                parent,
                group,
                zombie,
                started,
            })
        })
        .collect()
}

/// Where `ps` is looked for, in order.
const PS_PROGRAMS: [&str; 2] = ["/bin/ps", "/usr/bin/ps"];

/// Run bounded process inspection with the first of `programs` that starts.
pub(crate) async fn list_processes_from(
    programs: &[PathBuf],
) -> Result<Vec<ProcessRow>, InspectError> {
    let failed = |e: std::io::Error| InspectError::Failed(format!("ps: {e}"));
    let mut spawned = None;
    let mut last_error = None;
    for program in programs {
        match tokio::process::Command::new(program)
            .args(["-axo", "pid=,ppid=,pgid=,stat=,lstart="])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(child) => {
                spawned = Some(child);
                break;
            }
            Err(error) => last_error = Some((program.clone(), error)),
        }
    }
    let Some(mut child) = spawned else {
        let detail = last_error.map_or_else(
            || "no ps program configured".to_string(),
            |(program, error)| format!("{}: {error}", program.display()),
        );
        return Err(InspectError::Unavailable(format!(
            "ps could not be started ({detail})"
        )));
    };
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
            if bytes.len() > PS_MAX_BYTES {
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
    let result = match tokio::time::timeout(PS_TIMEOUT, read).await {
        Ok(result) => result.map(|s| parse_rows(&s)),
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

/// Run bounded process inspection with the system `ps`.
#[cfg(test)]
pub(crate) async fn list_processes() -> Result<Vec<ProcessRow>, InspectError> {
    list_processes_from(&system_programs()).await
}

fn system_programs() -> Vec<PathBuf> {
    PS_PROGRAMS.iter().map(PathBuf::from).collect()
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
}

/// The shared receipts: the public numbers plus the instant of the latest
/// success, which stays internal.
#[derive(Debug, Default)]
pub(crate) struct Observations {
    pub stats: InspectionStats,
    pub last_success: Option<Instant>,
}

pub(crate) type SharedObservations = Arc<Mutex<Observations>>;
pub(crate) type Warnings = Arc<Mutex<Vec<String>>>;

/// Add a warning once.
pub(crate) fn push_warning(warnings: &Warnings, warning: String) {
    let mut guard = warnings.lock().unwrap_or_else(|e| e.into_inner());
    if !guard.contains(&warning) {
        guard.push(warning);
    }
}

fn clip(text: &str, chars: usize) -> String {
    text.chars().take(chars).collect()
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
    mirror: Arc<Mutex<Vec<i32>>>,
    lister: Lister,
    enabled: bool,
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
}

impl SweepView {
    /// Whether the sweep found nothing alive.
    pub fn is_empty(&self) -> bool {
        self.group.is_empty() && self.session.is_empty() && self.tracked.is_empty()
    }

    /// Every pid found, once.
    pub fn all(&self) -> Vec<i32> {
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
        }
    }

    /// Publish the live tracked descendants to `mirror` after each snapshot.
    #[must_use]
    pub fn with_mirror(mut self, mirror: Arc<Mutex<Vec<i32>>>) -> Self {
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

    /// Adopt ancestry from every inspection, including cleanup and signal
    /// snapshots. Known descendants may fork after periodic capture stops.
    fn adopt(&mut self, rows: &[ProcessRow]) {
        if !self.enabled {
            return;
        }
        let root = rows.iter().find(|row| row.pid == self.pid);
        if self.root_started.is_none() {
            self.root_started = root.map(|r| r.started.clone());
        }
        let mut owned: HashSet<i32> = HashSet::new();
        if let Some(root) = root
            && Some(&root.started) == self.root_started.as_ref()
        {
            owned.insert(self.pid);
        }
        for row in rows {
            if self.known.get(&row.pid) == Some(&row.started) {
                owned.insert(row.pid);
            }
        }
        for _ in 0..rows.len() {
            let before = owned.len();
            for row in rows {
                if row.pid != self.pid && owned.contains(&row.parent) && !owned.contains(&row.pid) {
                    owned.insert(row.pid);
                    self.known.insert(row.pid, row.started.clone());
                }
            }
            if before == owned.len() {
                break;
            }
        }
    }

    async fn snapshot(&mut self) -> Result<Vec<ProcessRow>, InspectError> {
        let rows = self.rows().await?;
        self.adopt(&rows);
        let tracked: Vec<i32> = rows
            .iter()
            .filter(|r| !r.zombie && self.known.get(&r.pid) == Some(&r.started))
            .map(|r| r.pid)
            .collect();
        *self.mirror.lock().unwrap_or_else(|e| e.into_inner()) = tracked;
        let mut obs = self.observations.lock().unwrap_or_else(|e| e.into_inner());
        obs.stats.root_identity_observed = self.root_started.is_some();
        obs.stats.retained_descendant_identities = self.known.len();
        drop(obs);
        Ok(rows)
    }

    /// Classify the live processes in one table. The child is the session
    /// leader, so its pid is the session id; a process outside its group that
    /// still reports that session id was started by the child and never left.
    /// The session id is read per pid with `getsid`, because `ps` keywords for
    /// it differ between systems (macOS reports 0 for every process).
    fn view(&self, rows: &[ProcessRow]) -> SweepView {
        let mut view = SweepView::default();
        for row in rows.iter().filter(|r| !r.zombie) {
            if row.group == self.pid {
                view.group.push(row.pid);
            } else if sys::session_of(row.pid) == Some(self.pid) {
                view.session.push(row.pid);
            } else if self.known.get(&row.pid) == Some(&row.started) {
                view.tracked.push(row.pid);
            }
        }
        view
    }

    /// One sweep from a fresh snapshot, adopting descendants still attached to
    /// an identified owned process. With `signal`, everything the sweep found
    /// is signalled from that same snapshot; a TERM is followed by CONT so a
    /// stopped process receives it.
    pub async fn sweep(&mut self, signal: Option<Signal>) -> Result<SweepView, InspectError> {
        let rows = self.snapshot().await?;
        let view = self.view(&rows);
        if let Some(signal) = signal {
            for pid in view.all() {
                // A session member is checked again just before it is signalled.
                if view.session.contains(&pid) && sys::session_of(pid) != Some(self.pid) {
                    continue;
                }
                self.deliver(pid, signal);
            }
        }
        Ok(view)
    }

    fn deliver(&self, pid: i32, signal: Signal) {
        for signal in [
            Some(signal),
            (signal == Signal::TERM).then_some(Signal::CONT),
        ]
        .into_iter()
        .flatten()
        {
            match sys::signal_process(pid, signal) {
                Ok(()) | Err(SignalError::NoSuchProcess) => {}
                Err(error) => push_warning(
                    &self.warnings,
                    format!("Descendant signal failed: {error:?}"),
                ),
            }
        }
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
        match self.snapshot().await {
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
                        started: "leader".into(),
                    },
                    ProcessRow {
                        pid: 11,
                        parent: 10,
                        group: 10,
                        zombie: dead,
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

    #[tokio::test]
    async fn failed_inspection_is_not_an_empty_group() {
        let mut tree = ProcessTree::new(10, failing(InspectError::Failed("EPERM".into())), true);
        assert_eq!(
            tree.group_live().await.unwrap_err(),
            InspectError::Failed("EPERM".into())
        );
    }
}
