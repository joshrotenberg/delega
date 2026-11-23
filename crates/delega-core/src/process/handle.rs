//! The caller's side of a running child: wait, interrupt, drop.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime};

use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::contract::result::Cleanup;

use super::capture::Captured;
use super::owner::Registry;
use super::report::{Diagnostics, Ending, ProcessReport, ProcessTiming};
use super::run::run_owned;
use super::spec::ProcessSpec;

/// A cloneable way to stop a run from another task or thread.
///
/// Interrupting is a request: the owner stops the child and its descendants,
/// settles cleanup, and the report's ending is [`Ending::Interrupted`], unless
/// the child had already exited, in which case it reports how it exited.
#[derive(Debug, Clone)]
pub struct Interrupter(Stop);

impl Interrupter {
    /// Ask the owner to stop the child. Returns at once. Safe to call from any
    /// thread. The owner first waits briefly for a process listing (see
    /// [`STOP_LISTING_BUDGET`](super::limits::STOP_LISTING_BUDGET)); a second
    /// call while it waits ends the wait and stops the child at once.
    pub fn interrupt(&self) {
        self.0.interrupt();
    }
}

/// The two ways to ask for a stop: `stop` is the first request, and `hurry`
/// ends the owner's wait for a process listing. A second interrupt and a drop
/// both set `hurry`.
#[derive(Debug, Clone, Default)]
pub(super) struct Stop {
    pub(super) stop: CancellationToken,
    pub(super) hurry: CancellationToken,
}

impl Stop {
    fn interrupt(&self) {
        if self.stop.is_cancelled() {
            self.hurry.cancel();
        } else {
            self.stop.cancel();
        }
    }

    /// A drop never waits for a listing.
    fn stop_at_once(&self) {
        self.hurry.cancel();
        self.stop.cancel();
    }
}

/// A running child and the way to its terminal report.
///
/// Dropping the handle before [`wait`](Self::wait) has returned interrupts the
/// run: the owner stops the child and its descendants in the background. If
/// the runtime is shut down first, the owner's drop sends SIGKILL to the
/// child's process group. Either way the report is discarded.
#[derive(Debug)]
pub struct ProcessHandle {
    stop: Stop,
    registry: Arc<Registry>,
    join: Option<JoinHandle<ProcessReport>>,
    started_wall: SystemTime,
    started: Instant,
    time_limit: Duration,
    tracking: bool,
    kept: bool,
}

impl ProcessHandle {
    /// Start the child and return at once. The child is started by the owner
    /// task, so a failure to start is the report's ending, not an error here.
    ///
    /// # Panics
    ///
    /// Panics if called outside a Tokio runtime.
    pub fn start(spec: ProcessSpec) -> Self {
        #[cfg(test)]
        crate::test_spawn::rooms::enter(if spec.keeper.is_some() {
            crate::test_spawn::rooms::Room::Kept
        } else {
            crate::test_spawn::rooms::Room::Unkept
        });
        let stop = Stop::default();
        let registry = Arc::new(Registry::default());
        let started_wall = SystemTime::now();
        let started = Instant::now();
        let time_limit = spec.time_limit;
        let tracking = spec.track_descendants;
        let kept = spec.keeper.is_some();
        let join = tokio::spawn(run_owned(spec, stop.clone(), registry.clone()));
        Self {
            stop,
            registry,
            join: Some(join),
            started_wall,
            started,
            time_limit,
            tracking,
            kept,
        }
    }

    /// The child's pid once it has started.
    pub fn pid(&self) -> Option<u32> {
        match self.registry.pid.load(Ordering::SeqCst) {
            0 => None,
            pid => Some(pid),
        }
    }

    /// The tracked descendants seen at the latest scan.
    #[cfg(test)]
    pub(crate) fn tracked_pids(&self) -> Vec<u32> {
        self.registry.tracked_pids()
    }

    /// Ask the owner to stop the child. See [`Interrupter`].
    pub fn interrupt(&self) {
        self.stop.interrupt();
    }

    /// A cloneable interrupter for another task or thread.
    pub fn interrupter(&self) -> Interrupter {
        Interrupter(self.stop.clone())
    }

    /// Whether the report is ready.
    pub fn is_finished(&self) -> bool {
        self.join.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Wait for the terminal report. Cancelling this future drops the handle,
    /// which interrupts the run.
    pub async fn wait(mut self) -> ProcessReport {
        let joined = match self.join.as_mut() {
            Some(join) => join.await,
            None => return self.lost("the report was already taken"),
        };
        self.join = None;
        match joined {
            Ok(report) => report,
            Err(error) => {
                let why = if error.is_panic() {
                    "the owner task panicked"
                } else {
                    "the runtime shut down before the run finished"
                };
                self.lost(why)
            }
        }
    }

    /// The report for an owner that ended without one. The owner's drop sent a
    /// best-effort SIGKILL to the group; nothing confirms it worked.
    fn lost(&self, why: &str) -> ProcessReport {
        let pid = self.pid();
        ProcessReport {
            pid,
            ending: Ending::WaitFailed {
                message: why.to_string(),
            },
            exit: None,
            stdout: Captured::default(),
            stderr: Captured::default(),
            timing: ProcessTiming {
                started_at: pid.map(|_| self.started_wall),
                finished_at: SystemTime::now(),
                duration: self.started.elapsed(),
                time_limit: self.time_limit,
            },
            cleanup: Cleanup::Unconfirmed {
                pids: pid
                    .into_iter()
                    .chain(self.registry.tracked_pids())
                    .collect(),
                process_groups: pid.into_iter().collect(),
                reason: format!(
                    "{why}; a best-effort SIGKILL was sent to the process group but \
                     no check confirmed it{}",
                    if self.kept {
                        "; the keeper stops what is left when the owner's end of its socket \
                         closes"
                    } else {
                        ""
                    }
                ),
            },
            diagnostics: Diagnostics {
                warnings: vec![why.to_string()],
                signals: Vec::new(),
                signals_omitted: 0,
                tracking_enabled: self.tracking,
                inspection: Default::default(),
                capture_gap_observed: true,
                post_exit_group_signal: false,
                pipes_closed_by_owner: false,
                session_members_swept: Vec::new(),
                keeper: self.kept.then(super::report::KeeperDiagnostics::unstarted),
            },
        }
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        if self.join.is_some() {
            self.stop.stop_at_once();
        }
    }
}

/// Start `spec` and wait for its terminal report.
///
/// Dropping the returned future interrupts the run, as dropping a
/// [`ProcessHandle`] does.
pub async fn run(spec: ProcessSpec) -> ProcessReport {
    ProcessHandle::start(spec).wait().await
}
