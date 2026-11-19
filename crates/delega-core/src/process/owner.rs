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
    CAPTURE_INTERVAL, CLEAN_SWEEPS_AFTER_ACTIVITY, CLEAN_SWEEPS_PLAIN, CLEANUP_POLL_INTERVAL,
    EXIT_POLL_INTERVAL, INSPECTION_FAILURE_LIMIT, KILL_REPEAT_INTERVAL, LEADER_EXIT_WAIT,
    MAX_SIGNAL_RECORDS, MAX_SWEPT_RECORDS, PRE_STOP_POLL_INTERVAL,
};
use super::report::{Diagnostics, LeaderExit, SignalRecord, SignalTarget};
use super::spec::ProcessSpec;
use super::sys::{self, Signal, SignalError};
use super::tree::{
    CaptureOutcome, InspectError, InspectionStats, ProcessTree, SharedObservations, SweepView,
    Warnings, push_warning, system_lister,
};

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// What the handle and the owner share so a report can be written even when
/// the owner is gone.
#[derive(Debug, Default)]
pub(super) struct Registry {
    /// The child's pid, 0 until it has started.
    pub pid: std::sync::atomic::AtomicU32,
    /// Tracked descendants alive at the latest scan.
    pub tracked: Arc<Mutex<Vec<i32>>>,
}

impl Registry {
    /// The tracked descendants as pids.
    pub(super) fn tracked_pids(&self) -> Vec<u32> {
        lock(&self.tracked)
            .iter()
            .filter_map(|p| u32::try_from(*p).ok())
            .collect()
    }
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
    /// The highest request number a successful scan has served.
    capture_succeeded: AtomicU64,
    /// A listing that had to precede the first signal did not complete.
    pre_signal_listing_missed: AtomicBool,
    capture_notify: tokio::sync::Notify,
    stop_wait: Mutex<StopWait>,
    budget_exceeded: AtomicU64,
    pipes_closed_by_owner: AtomicBool,
    pub(super) leader_exited: Arc<AtomicBool>,
    last_view: Mutex<Option<Result<SweepView, String>>>,
    swept: Mutex<Vec<u32>>,
    tracked_mirror: Arc<Mutex<Vec<i32>>>,
    #[cfg(test)]
    swallow_kills: std::sync::atomic::AtomicU32,
    descendant_signal_budget: Duration,
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
        #[cfg(test)]
        if signal == Signal::KILL {
            let left = self.swallow_kills.load(Ordering::SeqCst);
            if left > 0
                && self
                    .swallow_kills
                    .compare_exchange(left, left - 1, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                return;
            }
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
    pub(super) async fn before_stop(&self, budget: Duration, hurry: &CancellationToken) -> bool {
        let requested_at = Instant::now();
        let requested = {
            let mut wait = lock(&self.stop_wait);
            let requested = self.capture_requested.fetch_add(1, Ordering::SeqCst) + 1;
            wait.requests = requested;
            wait.acknowledged_at = None;
            requested
        };
        self.capture_notify.notify_one();
        let deadline = requested_at + budget;
        while self.capture_completed.load(Ordering::SeqCst) < requested
            && Instant::now() < deadline
            && !hurry.is_cancelled()
        {
            tokio::time::sleep(PRE_STOP_POLL_INTERVAL).await;
        }
        let acknowledged = self.capture_succeeded.load(Ordering::SeqCst) >= requested
            && lock(&self.stop_wait)
                .acknowledged_at
                .is_some_and(|at| at <= deadline);
        if !acknowledged {
            self.budget_exceeded.fetch_add(1, Ordering::SeqCst);
            self.warn(format!(
                "The process listing before the first signal did not complete within {} ms; \
                 newly detached children may be unobserved",
                budget.as_millis()
            ));
        }
        acknowledged
    }

    /// Record that the first signal goes out without the listing it needed.
    pub(super) fn note_listing_missed(&self) {
        self.pre_signal_listing_missed.store(true, Ordering::SeqCst);
    }

    /// The polite stop: TERM to the group, then CONT so a stopped process
    /// receives it, then TERM (and CONT) to everything a sweep finds outside
    /// the group. The group is signalled first and without waiting.
    pub(super) async fn terminate(&self) {
        self.signal_group(Signal::TERM);
        self.signal_group(Signal::CONT);
        self.signal_swept(Signal::TERM).await;
    }

    /// The hard stop: KILL to the group, then to everything a sweep finds.
    pub(super) async fn kill_all(&self) {
        self.signal_group(Signal::KILL);
        self.signal_swept(Signal::KILL).await;
    }

    /// Signal what a fresh sweep finds, within a budget so a slow or hung
    /// listing cannot hold the owner. A cleanup pass signals again later.
    async fn signal_swept(&self, signal: Signal) {
        let _ = tokio::time::timeout(self.descendant_signal_budget, async {
            let result = self.tree.lock().await.sweep(Some(signal)).await;
            self.note_sweep(signal, result);
        })
        .await;
    }

    /// Record a sweep that signalled: the signal log, the session members
    /// found, and the view for a later unconfirmed verdict.
    fn note_sweep(&self, signal: Signal, result: Result<SweepView, InspectError>) {
        match result {
            Ok(view) => {
                let outside = view.session.len() + view.tracked.len();
                if outside > 0 {
                    self.record_signal(signal, SignalTarget::Descendants { count: outside });
                }
                let mut swept = lock(&self.swept);
                for pid in &view.session {
                    let Ok(pid) = u32::try_from(*pid) else {
                        continue;
                    };
                    if swept.len() < MAX_SWEPT_RECORDS && !swept.contains(&pid) {
                        swept.push(pid);
                    }
                }
                *lock(&self.last_view) = Some(Ok(view));
            }
            Err(error) => {
                self.warn(format!("Process sweep unavailable: {error}"));
                *lock(&self.last_view) = Some(Err(error.to_string()));
            }
        }
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
            session_members_swept: lock(&self.swept).clone(),
        }
    }
}

/// The reason of an `Unconfirmed` verdict whose only cause is that the listing
/// taken before the first signal did not complete in time. Exported so that
/// tests of the binary can recognise exactly this verdict.
///
/// Public only so that the tests of the binary and of the acceptance harness
/// can name the verdict; it is not part of the supported API.
#[doc(hidden)]
pub const LATE_LISTING_REASON: &str = "the process listing taken before the first signal did not \
     complete, so a descendant that left the session when its parent was stopped could not be \
     ruled out; the group and session sweeps afterwards found nothing";

/// Whether a verdict is `Unconfirmed` for the late listing and for nothing
/// else: no pids and no process groups, and a reason that is
/// [`LATE_LISTING_REASON`] once, or that text repeated any number of times
/// joined by `"; "` (each report of a provider run that missed its listing,
/// which `merge_cleanup` joins that way: the version probe, MCP discovery and
/// the run itself). Any other text, such as a failed stop, or any pid, is not
/// this verdict.
///
/// Public only so that the tests of the binary and of the acceptance harness
/// can recognise the verdict; it is not part of the supported API.
#[doc(hidden)]
pub fn is_late_listing_only(cleanup: &Cleanup) -> bool {
    let Cleanup::Unconfirmed {
        pids,
        process_groups,
        reason,
    } = cleanup
    else {
        return false;
    };
    pids.is_empty() && process_groups.is_empty() && only_late_listing_reasons(reason)
}

/// Whether `reason` is [`LATE_LISTING_REASON`] one or more times, joined by
/// `"; "`. The reason itself contains `"; "`, so it is matched by prefix.
fn only_late_listing_reasons(reason: &str) -> bool {
    let mut rest = reason;
    loop {
        let Some(after) = rest.strip_prefix(LATE_LISTING_REASON) else {
            return false;
        };
        if after.is_empty() {
            return true;
        }
        match after.strip_prefix("; ") {
            Some(next) => rest = next,
            None => return false,
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
            // and no other task can send a later group signal. Descendants
            // that were alive at the latest scan are signalled too; a scan
            // cannot run here, so a pid that was reused since is a risk this
            // accepts.
            let _ = sys::signal_group(self.shared.pid, Signal::KILL);
            for pid in lock(&self.shared.tracked_mirror).iter() {
                let _ = sys::signal_process(*pid, Signal::KILL);
            }
        }
    }
}

impl OwnedProcess {
    /// Start the child in its own process group, then its exit watcher and
    /// its periodic descendant scan.
    pub(super) fn spawn(
        command: &mut tokio::process::Command,
        spec: &ProcessSpec,
        registry: &Registry,
    ) -> Result<Self, std::io::Error> {
        command.kill_on_drop(true);
        // A session of its own, with the child as leader: its pid is the
        // session id and the process group id, and it has no controlling
        // terminal.
        sys::new_session(command);
        let child = command.spawn()?;
        let pid = child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .ok_or_else(|| {
                std::io::Error::other("the child exited before its pid could be read")
            })?;
        let lister = spec.lister.clone().unwrap_or_else(system_lister);
        let tree = ProcessTree::new(pid, lister, spec.track_descendants)
            .with_mirror(registry.tracked.clone());
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
            capture_succeeded: AtomicU64::new(0),
            pre_signal_listing_missed: AtomicBool::new(false),
            capture_notify: tokio::sync::Notify::new(),
            stop_wait: Default::default(),
            budget_exceeded: AtomicU64::new(0),
            pipes_closed_by_owner: AtomicBool::new(false),
            leader_exited: Arc::new(AtomicBool::new(false)),
            last_view: Mutex::new(None),
            swept: Mutex::new(Vec::new()),
            tracked_mirror: registry.tracked.clone(),
            #[cfg(test)]
            swallow_kills: std::sync::atomic::AtomicU32::new(spec.swallow_group_kills),
            descendant_signal_budget: spec.timings.descendant_signal,
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
        let interval = CAPTURE_INTERVAL;
        #[cfg(test)]
        let interval = spec.capture_interval.unwrap_or(interval);
        owner.capture_task = Some(tokio::spawn(async move {
            loop {
                let requested = shared.capture_requested.load(Ordering::SeqCst);
                let outcome = shared.tree.lock().await.capture().await;
                if requested > 0
                    && matches!(
                        outcome,
                        CaptureOutcome::Success | CaptureOutcome::TrackingDisabled
                    )
                {
                    shared
                        .capture_succeeded
                        .fetch_max(requested, Ordering::SeqCst);
                }
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
                    _ = tokio::time::sleep(interval) => {},
                }
            }
        }));
        Ok(owner)
    }

    /// Stop everything and settle. With `hard_stop` the group is killed at
    /// once; otherwise TERM goes first (unless `term_at` says it already did)
    /// and KILL follows after `grace`. KILL to the group is repeated on a timer
    /// that does not depend on process inspection. Each pass of the sweep
    /// signals whatever it finds in the group, in the child's session, or among
    /// tracked descendants, until a pass finds nothing and the leader has
    /// exited. The leader is reaped last. When `deadline` passes first, or
    /// inspection is unavailable, what could not be checked is reported, not
    /// waited for.
    pub(super) async fn cleanup(
        &mut self,
        grace: Duration,
        deadline: Instant,
        hard_stop: bool,
        term_at: Option<Instant>,
        stop_budget: Duration,
    ) -> Settled {
        let shared = self.shared.clone();
        if hard_stop {
            // The deadline is already fixed, so the wait leaves it half.
            let left = deadline.saturating_duration_since(Instant::now());
            if !shared
                .before_stop(stop_budget.min(left / 2), &CancellationToken::new())
                .await
            {
                shared.note_listing_missed();
            }
        }
        let first_term = term_at.unwrap_or_else(Instant::now);
        if term_at.is_none() && !hard_stop {
            shared.terminate().await;
        }
        let mut next_kill = if hard_stop {
            Instant::now()
        } else {
            first_term + grace
        };
        if next_kill <= Instant::now() {
            shared.kill_all().await;
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
            let mut failures = 0u32;
            let mut clean_sweeps = 0u32;
            let mut active = false;
            loop {
                let signal = if hard_stop || first_term.elapsed() >= grace {
                    Signal::KILL
                } else {
                    Signal::TERM
                };
                let pass = shared.tree.lock().await.sweep(Some(signal)).await;
                match pass {
                    Ok(view) => {
                        failures = 0;
                        let leader_exited = shared.leader_has_exited();
                        // Only a sweep that found nothing and could check every
                        // row counts, and it takes several in a row: a
                        // descendant that forks successors faster than a
                        // listing is taken can slip through one.
                        if !view.is_empty() {
                            active = true;
                        }
                        if view.is_empty() && leader_exited {
                            clean_sweeps += 1;
                        } else {
                            clean_sweeps = 0;
                        }
                        // One sweep confirms a plain exit; once anything was
                        // observed or signalled, two in a row must agree.
                        let tracked_ever = lock(&shared.observations)
                            .stats
                            .retained_descendant_identities
                            > 0;
                        let needed = if active || tracked_ever {
                            CLEAN_SWEEPS_AFTER_ACTIVITY
                        } else {
                            CLEAN_SWEEPS_PLAIN
                        };
                        let clean = clean_sweeps >= needed;
                        if shared.pending_eperm.load(Ordering::SeqCst) && !view.group.is_empty() {
                            lock(&shared.signal_error).get_or_insert_with(|| "kill EPERM".into());
                            shared.warn(
                                "Process-group signal failed: EPERM with live group members".into(),
                            );
                        }
                        if !view.is_empty() {
                            shared.note_sweep(signal, Ok(view));
                        } else {
                            *lock(&shared.last_view) = Some(Ok(view));
                        }
                        if clean {
                            shared.pending_eperm.store(false, Ordering::SeqCst);
                            break Ended::Clean;
                        }
                    }
                    Err(error) => {
                        failures += 1;
                        *lock(&shared.last_view) = Some(Err(error.to_string()));
                        shared.warn(format!("Process cleanup inspection unavailable: {error}"));
                        if matches!(error, InspectError::Unavailable(_))
                            || failures >= INSPECTION_FAILURE_LIMIT
                        {
                            break Ended::InspectionUnavailable(error.to_string());
                        }
                    }
                }
                tokio::time::sleep(CLEANUP_POLL_INTERVAL).await;
            }
        };
        tokio::pin!(settle);
        let ended = loop {
            tokio::select! {
                ended = &mut settle => break ended,
                _ = tokio::time::sleep_until(next_kill) => {
                    shared.signal_group(Signal::KILL);
                    next_kill = Instant::now() + KILL_REPEAT_INTERVAL;
                },
                _ = tokio::time::sleep_until(deadline) => break Ended::DeadlinePassed,
            }
        };
        match ended {
            Ended::Clean => {
                let missed = shared.pre_signal_listing_missed.load(Ordering::SeqCst);
                // All signal-capable work is above. No task can signal this
                // group after this single reap.
                shared.signals_closed.store(true, Ordering::SeqCst);
                let exit = self.reap().await;
                // Later sweeps found nothing, but none of them can rule out a
                // descendant that left the session when its parent was
                // stopped: that parent was only visible to the listing that
                // did not complete.
                let cleanup = if missed {
                    Cleanup::Unconfirmed {
                        pids: Vec::new(),
                        process_groups: Vec::new(),
                        reason: LATE_LISTING_REASON.to_string(),
                    }
                } else {
                    Cleanup::Confirmed
                };
                Settled { exit, cleanup }
            }
            Ended::DeadlinePassed => {
                let leader_gone = shared.leader_has_exited();
                let exit = if leader_gone {
                    shared.signals_closed.store(true, Ordering::SeqCst);
                    self.reap().await
                } else {
                    // The leader is still running and unreaped. Dropping the
                    // owner sends one last group SIGKILL while the group id is
                    // still ours.
                    Err("the child had not exited when cleanup gave up".into())
                };
                let cleanup = shared.unconfirmed(leader_gone, deadline_text(first_term, deadline));
                Settled { exit, cleanup }
            }
            Ended::InspectionUnavailable(error) => {
                self.finish_without_inspection(error, next_kill, deadline)
                    .await
            }
        }
    }

    /// Process listing does not work, so the sweep cannot run. Kill the group,
    /// reap the leader, and check the one thing that needs no listing: whether
    /// any process still has the child's group id. The verdict is always
    /// unconfirmed, and says what was checked.
    async fn finish_without_inspection(
        &mut self,
        error: String,
        next_kill: Instant,
        deadline: Instant,
    ) -> Settled {
        let shared = self.shared.clone();
        // A leader that has exited needs no grace: the group got TERM when it
        // exited, and nothing can be inspected to tell whether waiting helps.
        if !shared.leader_has_exited() && next_kill > Instant::now() && next_kill < deadline {
            tokio::time::sleep_until(next_kill).await;
        }
        shared.signal_group(Signal::KILL);
        let wait_until = (Instant::now() + LEADER_EXIT_WAIT).min(deadline.max(Instant::now()));
        while !shared.leader_has_exited() && Instant::now() < wait_until {
            tokio::time::sleep(EXIT_POLL_INTERVAL).await;
        }
        let leader_gone = shared.leader_has_exited();
        let pid = u32::try_from(shared.pid).unwrap_or(0);
        let mut pids: Vec<u32> = lock(&shared.tracked_mirror)
            .iter()
            .filter_map(|p| u32::try_from(*p).ok())
            .collect();
        let mut groups = Vec::new();
        let (exit, checked) = if leader_gone {
            shared.signals_closed.store(true, Ordering::SeqCst);
            let exit = self.reap().await;
            // After the reap the group id is no longer pinned; the probe only
            // asks, it never signals.
            // Members that just received the KILL need a moment to go.
            let until = Instant::now() + LEADER_EXIT_WAIT;
            while sys::probe_group(shared.pid) == sys::GroupProbe::Present && Instant::now() < until
            {
                tokio::time::sleep(EXIT_POLL_INTERVAL).await;
            }
            let checked = match sys::probe_group(shared.pid) {
                sys::GroupProbe::Empty => "no process has the child's group id".to_string(),
                sys::GroupProbe::Present => {
                    groups.push(pid);
                    "a process may still have the child's group id".to_string()
                }
            };
            (exit, checked)
        } else {
            pids.push(pid);
            groups.push(pid);
            (
                Err("the child had not exited after the final kill".into()),
                "the child had not exited".to_string(),
            )
        };
        pids.sort_unstable();
        pids.dedup();
        let tracked_note = if pids.is_empty() {
            String::new()
        } else {
            " Descendants seen at the latest scan are listed because they cannot be checked."
                .to_string()
        };
        Settled {
            exit,
            cleanup: Cleanup::Unconfirmed {
                pids,
                process_groups: groups,
                reason: format!(
                    "process inspection was unavailable ({error}); only the child's process \
                     group was checked with kill(-pgid, 0): {checked}. Descendants outside \
                     the group and session could not be checked.{tracked_note}"
                ),
            },
        }
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

/// How the settle loop ended.
enum Ended {
    Clean,
    DeadlinePassed,
    InspectionUnavailable(String),
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
                pids.extend(view.all().iter().filter_map(|p| u32::try_from(*p).ok()));
                if !view.group.is_empty() {
                    groups.push(pid);
                }
                if !view.group.is_empty() {
                    "the child's process group still had live members".to_string()
                } else if !view.session.is_empty() {
                    "processes in the child's session were still running".to_string()
                } else if !view.tracked.is_empty() {
                    "tracked descendants were still running".to_string()
                } else {
                    "the child had not exited or the sweeps were not yet clean".to_string()
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
