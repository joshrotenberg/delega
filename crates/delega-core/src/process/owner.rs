//! Process ownership, observation and cleanup.
//!
//! One task owns the child from spawn to the single reap. Other tasks only
//! inspect (the descendant scan) or only write (standard input). None of them
//! can signal, so none can signal a recycled pid. The leader is never reaped
//! before cleanup is settled, which pins its process group id.

use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::contract::result::Cleanup;

use super::limits::{
    CAPTURE_INTERVAL, CLEANUP_POLL_INTERVAL, KILL_REPEAT_INTERVAL, MAX_SIGNAL_RECORDS,
    PRE_STOP_CAPTURE_BUDGET, PRE_STOP_POLL_INTERVAL,
};
use super::report::{Diagnostics, LeaderExit, SignalRecord, SignalTarget};
use super::spec::ProcessSpec;
use super::sys::{self, Signal, SignalError};
use super::tree::{
    CleanupView, InspectionStats, ProcessTree, SharedObservations, Warnings, push_warning,
    system_lister,
};

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

#[derive(Default)]
struct StopWait {
    requests: u64,
    acknowledged_at: Option<Instant>,
}

/// State shared between the owner and its capture task.
pub(super) struct Shared {
    pub(super) pid: i32,
    tracking_enabled: bool,
    observations: SharedObservations,
    tree: tokio::sync::Mutex<ProcessTree>,
    warnings: Warnings,
    signals: Mutex<Vec<SignalRecord>>,
    signals_omitted: AtomicU64,
    signal_error: Mutex<Option<String>>,
    pending_eperm: AtomicBool,
    post_exit_group_signal: AtomicBool,
    signals_closed: AtomicBool,
    capture_requested: AtomicU64,
    capture_completed: AtomicU64,
    capture_notify: tokio::sync::Notify,
    stop_wait: Mutex<StopWait>,
    budget_exceeded: AtomicU64,
    pipes_closed_by_owner: AtomicBool,
    pub(super) leader_exited: Arc<AtomicBool>,
    last_view: Mutex<Option<Result<CleanupView, String>>>,
}

impl Shared {
    pub(super) fn warn(&self, warning: String) {
        push_warning(&self.warnings, warning);
    }

    pub(super) fn note_pipes_closed(&self) {
        self.pipes_closed_by_owner.store(true, Ordering::SeqCst);
    }

    /// Whether the leader has exited, by the watcher's flag or a direct check.
    pub(super) fn leader_has_exited(&self) -> bool {
        self.leader_exited.load(Ordering::SeqCst) || sys::leader_has_exited(self.pid)
    }

    fn record_signal(&self, signal: Signal, target: SignalTarget) {
        let mut signals = lock(&self.signals);
        if signals.len() < MAX_SIGNAL_RECORDS {
            signals.push(SignalRecord {
                signal: sys::signal_name(signal.as_raw()),
                at: SystemTime::now(),
                target,
            });
        } else {
            self.signals_omitted.fetch_add(1, Ordering::SeqCst);
        }
    }

    // Only the process owner calls this method, while its direct child is
    // unreaped. No detached timer or capture task may send group signals.
    pub(super) fn signal_group(&self, signal: Signal) {
        if self.signals_closed.load(Ordering::SeqCst) {
            return;
        }
        let already_exited = self.leader_has_exited();
        match sys::signal_group(self.pid, signal) {
            Ok(()) => {
                if already_exited {
                    self.post_exit_group_signal.store(true, Ordering::SeqCst);
                }
                self.record_signal(signal, SignalTarget::Group);
            }
            Err(SignalError::NoSuchProcess) => {}
            // Resolve only after a fresh table positively excludes live group
            // members. Leader exit alone is not sufficient.
            Err(SignalError::PermissionDenied) => {
                self.pending_eperm.store(true, Ordering::SeqCst);
            }
            Err(SignalError::Other(message)) => {
                lock(&self.signal_error).get_or_insert_with(|| format!("kill {message}"));
                self.warn(format!("Process-group signal failed: {message}"));
            }
        }
    }

    /// Give a fresh fast snapshot one bounded chance to capture newly
    /// detached children before TERM reparents them. This never cancels an
    /// inspector: slow captures continue under their owner while signalling
    /// resumes after at most the budget.
    pub(super) async fn before_stop(&self) {
        let requested_at = Instant::now();
        let requested = {
            let mut wait = lock(&self.stop_wait);
            let requested = self.capture_requested.fetch_add(1, Ordering::SeqCst) + 1;
            wait.requests = requested;
            wait.acknowledged_at = None;
            requested
        };
        self.capture_notify.notify_one();
        let deadline = requested_at + PRE_STOP_CAPTURE_BUDGET;
        while self.capture_completed.load(Ordering::SeqCst) < requested && Instant::now() < deadline
        {
            tokio::time::sleep(PRE_STOP_POLL_INTERVAL).await;
        }
        let acknowledged = self.capture_completed.load(Ordering::SeqCst) >= requested
            && lock(&self.stop_wait)
                .acknowledged_at
                .is_some_and(|at| at <= deadline);
        if !acknowledged {
            self.budget_exceeded.fetch_add(1, Ordering::SeqCst);
            self.warn(format!(
                "Pre-stop process capture exceeded {} ms; newly detached children may be unobserved",
                PRE_STOP_CAPTURE_BUDGET.as_millis()
            ));
        }
    }

    async fn signal_descendants(&self, signal: Signal) -> usize {
        let count = self.tree.lock().await.signal(signal).await;
        if count > 0 {
            self.record_signal(signal, SignalTarget::Descendants { count });
        }
        count
    }

    pub(super) fn diagnostics(&self) -> Diagnostics {
        let inspection: InspectionStats = lock(&self.observations).stats.clone();
        let capture_gap_observed = self.budget_exceeded.load(Ordering::SeqCst) > 0
            || inspection.failures > 0
            || !self.tracking_enabled
            || !inspection.root_identity_observed;
        Diagnostics {
            warnings: lock(&self.warnings).clone(),
            signals: lock(&self.signals).clone(),
            signals_omitted: self.signals_omitted.load(Ordering::SeqCst),
            tracking_enabled: self.tracking_enabled,
            inspection,
            capture_gap_observed,
            post_exit_group_signal: self.post_exit_group_signal.load(Ordering::SeqCst),
            pipes_closed_by_owner: self.pipes_closed_by_owner.load(Ordering::SeqCst),
            unobserved_descendants_possible: true,
        }
    }
}

/// What cleanup concluded.
pub(super) struct Settled {
    /// How the leader ended, or why that is unknown.
    pub exit: Result<LeaderExit, String>,
    /// The verdict.
    pub cleanup: Cleanup,
}

/// Owns the direct child until the final signal and reap.
pub(super) struct OwnedProcess {
    pub(super) child: Option<tokio::process::Child>,
    pub(super) shared: Arc<Shared>,
    capture_stop: CancellationToken,
    capture_task: Option<JoinHandle<()>>,
    pub(super) stdin_task: Option<JoinHandle<()>>,
    pub(super) exit_rx: Option<tokio::sync::oneshot::Receiver<Result<(), String>>>,
    /// Why the exit watcher could not be started, when it could not.
    pub(super) watch_error: Option<String>,
}

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        self.capture_stop.cancel();
        if let Some(task) = &self.stdin_task {
            task.abort();
        }
        if self.child.is_some() && !self.shared.signals_closed.load(Ordering::SeqCst) {
            // Runtime shutdown or a panic is best effort, not a settlement.
            // The child is still unreaped here, so the group id is still ours
            // and no other task can send a later group signal.
            let _ = sys::signal_group(self.shared.pid, Signal::KILL);
        }
    }
}

impl OwnedProcess {
    /// Start the child in its own process group, then its exit watcher and
    /// its periodic descendant scan.
    pub(super) fn spawn(
        command: &mut tokio::process::Command,
        spec: &ProcessSpec,
    ) -> Result<Self, std::io::Error> {
        command.kill_on_drop(true);
        // A process group of its own, with the child as leader: its pid is the
        // group id. Safe, unlike `setsid` in `pre_exec`; see the module docs.
        command.process_group(0);
        let child = command.spawn()?;
        let pid = child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .ok_or_else(|| {
                std::io::Error::other("the child exited before its pid could be read")
            })?;
        let lister = spec.lister.clone().unwrap_or_else(system_lister);
        let tree = ProcessTree::new(pid, lister, spec.track_descendants);
        let shared = Arc::new(Shared {
            pid,
            tracking_enabled: spec.track_descendants,
            observations: tree.observations.clone(),
            warnings: tree.warnings.clone(),
            tree: tokio::sync::Mutex::new(tree),
            signals: Default::default(),
            signals_omitted: AtomicU64::new(0),
            signal_error: Default::default(),
            pending_eperm: AtomicBool::new(false),
            post_exit_group_signal: AtomicBool::new(false),
            signals_closed: AtomicBool::new(false),
            capture_requested: AtomicU64::new(0),
            capture_completed: AtomicU64::new(0),
            capture_notify: tokio::sync::Notify::new(),
            stop_wait: Default::default(),
            budget_exceeded: AtomicU64::new(0),
            pipes_closed_by_owner: AtomicBool::new(false),
            leader_exited: Arc::new(AtomicBool::new(false)),
            last_view: Mutex::new(None),
        });
        let mut owner = OwnedProcess {
            child: Some(child),
            shared: shared.clone(),
            capture_stop: CancellationToken::new(),
            capture_task: None,
            stdin_task: None,
            exit_rx: None,
            watch_error: None,
        };
        match watch_exit(pid, shared.leader_exited.clone()) {
            Ok(rx) => owner.exit_rx = Some(rx),
            Err(error) => owner.watch_error = Some(error.to_string()),
        }
        let stop = owner.capture_stop.clone();
        owner.capture_task = Some(tokio::spawn(async move {
            loop {
                let requested = shared.capture_requested.load(Ordering::SeqCst);
                shared.tree.lock().await.capture().await;
                {
                    let mut wait = lock(&shared.stop_wait);
                    if requested > 0 && wait.requests == requested && wait.acknowledged_at.is_none()
                    {
                        wait.acknowledged_at = Some(Instant::now());
                    }
                }
                shared.capture_completed.store(requested, Ordering::SeqCst);
                tokio::select! {
                    _ = stop.cancelled() => break,
                    _ = shared.capture_notify.notified() => {},
                    _ = tokio::time::sleep(CAPTURE_INTERVAL) => {},
                }
            }
        }));
        Ok(owner)
    }

    /// Stop everything and settle. With `hard_stop` the group is killed at
    /// once; otherwise TERM goes first (unless `term_at` says it already did)
    /// and KILL follows after `grace`. KILL is repeated on a timer that does
    /// not depend on process inspection. The leader is reaped last. When
    /// `deadline` passes first, what remains is reported, not waited for.
    pub(super) async fn cleanup(
        &mut self,
        grace: Duration,
        deadline: Instant,
        hard_stop: bool,
        term_at: Option<Instant>,
    ) -> Settled {
        let shared = self.shared.clone();
        if hard_stop {
            shared.before_stop().await;
        }
        let first_term = term_at.unwrap_or_else(Instant::now);
        if term_at.is_none() && !hard_stop {
            shared.signal_group(Signal::TERM);
        }
        let mut next_kill = if hard_stop {
            Instant::now()
        } else {
            first_term + grace
        };
        if next_kill <= Instant::now() {
            shared.signal_group(Signal::KILL);
            next_kill = Instant::now() + KILL_REPEAT_INTERVAL;
        }
        self.capture_stop.cancel();
        if let Some(task) = self.stdin_task.take() {
            task.abort();
            let _ = task.await;
        }
        let capture_task = self.capture_task.take();
        let loop_shared = shared.clone();
        let settle = async move {
            let shared = loop_shared;
            if let Some(task) = capture_task
                && let Err(error) = task.await
            {
                shared.warn(format!("Process capture task failed: {error}"));
            }
            shared.tree.lock().await.capture().await;
            loop {
                let snapshot = shared.tree.lock().await.cleanup_state().await;
                *lock(&shared.last_view) = Some(
                    snapshot
                        .clone()
                        .map_err(|error: super::tree::InspectError| error.to_string()),
                );
                let leader_exited = shared.leader_has_exited();
                match &snapshot {
                    Ok(view) if !view.group_live && view.live.is_empty() && leader_exited => {
                        shared.pending_eperm.store(false, Ordering::SeqCst);
                        break;
                    }
                    Err(error) => {
                        shared.warn(format!("Process cleanup inspection unavailable: {error}"))
                    }
                    _ => {}
                }
                if shared.pending_eperm.load(Ordering::SeqCst)
                    && matches!(&snapshot, Ok(view) if view.group_live)
                {
                    lock(&shared.signal_error).get_or_insert_with(|| "kill EPERM".into());
                    shared
                        .warn("Process-group signal failed: EPERM with live group members".into());
                }
                let signal = if hard_stop || first_term.elapsed() >= grace {
                    Signal::KILL
                } else {
                    Signal::TERM
                };
                shared.signal_descendants(signal).await;
                tokio::time::sleep(CLEANUP_POLL_INTERVAL).await;
            }
        };
        tokio::pin!(settle);
        let confirmed = loop {
            tokio::select! {
                _ = &mut settle => break true,
                _ = tokio::time::sleep_until(next_kill) => {
                    shared.signal_group(Signal::KILL);
                    next_kill = Instant::now() + KILL_REPEAT_INTERVAL;
                },
                _ = tokio::time::sleep_until(deadline) => break false,
            }
        };
        if confirmed {
            // All signal-capable work is above. No task can signal this group
            // after this single reap.
            shared.signals_closed.store(true, Ordering::SeqCst);
            let exit = self.reap().await;
            return Settled {
                exit,
                cleanup: Cleanup::Confirmed,
            };
        }
        let leader_gone = shared.leader_has_exited();
        let exit = if leader_gone {
            shared.signals_closed.store(true, Ordering::SeqCst);
            self.reap().await
        } else {
            // The leader is still running and unreaped. Dropping the owner
            // sends one last group SIGKILL while the group id is still ours.
            Err("the child had not exited when cleanup gave up".into())
        };
        let cleanup = shared.unconfirmed(leader_gone, deadline_text(first_term, deadline));
        Settled { exit, cleanup }
    }

    async fn reap(&mut self) -> Result<LeaderExit, String> {
        let status = match self.child.as_mut() {
            Some(child) => child.wait().await,
            None => return Err("the child was already reaped".into()),
        };
        self.child.take();
        if let Some(rx) = self.exit_rx.take() {
            let _ = rx.await;
        }
        status
            .map(leader_exit)
            .map_err(|error| format!("wait failed: {error}"))
    }
}

fn deadline_text(first_term: Instant, deadline: Instant) -> String {
    format!(
        "{:.1} s",
        deadline.saturating_duration_since(first_term).as_secs_f64()
    )
}

impl Shared {
    /// The verdict when cleanup could not be confirmed, from the last view.
    fn unconfirmed(&self, leader_gone: bool, waited: String) -> Cleanup {
        let view = lock(&self.last_view).clone();
        let mut pids: Vec<u32> = Vec::new();
        let mut groups: Vec<u32> = Vec::new();
        let pid = u32::try_from(self.pid).unwrap_or(0);
        let detail = match view {
            Some(Ok(view)) => {
                pids.extend(view.live.iter().filter_map(|p| u32::try_from(*p).ok()));
                if view.group_live {
                    groups.push(pid);
                }
                if view.group_live {
                    "the child's process group still had live members".to_string()
                } else if !view.live.is_empty() {
                    "tracked descendants were still running".to_string()
                } else {
                    "the child had not exited".to_string()
                }
            }
            Some(Err(error)) => {
                // A failed listing proves nothing, so the group stays listed.
                groups.push(pid);
                format!("process inspection failed: {error}")
            }
            None => {
                groups.push(pid);
                "no process inspection completed".to_string()
            }
        };
        if !leader_gone {
            pids.push(pid);
            if !groups.contains(&pid) {
                groups.push(pid);
            }
        }
        pids.sort_unstable();
        pids.dedup();
        Cleanup::Unconfirmed {
            pids,
            process_groups: groups,
            reason: format!("stopping was not confirmed within {waited}: {detail}"),
        }
    }
}

fn leader_exit(status: ExitStatus) -> LeaderExit {
    use std::os::unix::process::ExitStatusExt;
    LeaderExit {
        code: status.code(),
        signal: status.signal(),
        signal_name: status.signal().map(sys::signal_name),
    }
}

/// A dedicated observer does not reap and does not wait behind process
/// inspection. The owner receives errors rather than assuming an exit.
fn watch_exit(
    pid: i32,
    flag: Arc<AtomicBool>,
) -> std::io::Result<tokio::sync::oneshot::Receiver<Result<(), String>>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name(format!("exit-watch-{pid}"))
        .spawn(move || {
            let result = sys::wait_for_exit(pid);
            if result.is_ok() {
                flag.store(true, Ordering::SeqCst);
            }
            let _ = tx.send(result);
        })?;
    Ok(rx)
}
