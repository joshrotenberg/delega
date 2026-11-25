//! The state the supervisor's task owns: every worker and run, the queue, the
//! cursor and the registered waits.
//!
//! One task owns a [`Core`] and applies one command at a time, so none of this
//! is shared and none of it needs a lock. The task never waits on a provider:
//! each run has a task of its own that reports back by a command.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::contract::result::{Cleanup, RunResult};

use super::command::Command;
use super::config::SupervisorConfig;
use super::ids::Ids;
use super::preflight::Preflight;
use super::record::{Run, Worker};
use super::runner::Launch;

mod admit;
mod queue;
mod retention;
mod stop;
mod view;
mod waits;

/// Whether the supervisor takes new work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Active,
    /// Stopping every worker; some run has not settled yet.
    ShuttingDown,
    /// Every worker is closed and nothing is running.
    ShutDown,
}

pub(super) struct Core {
    config: SupervisorConfig,
    ids: Ids,
    /// One counter for finished runs and activity entries.
    cursor: Arc<AtomicU64>,
    seq: u64,
    workers: HashMap<String, Worker>,
    runs: HashMap<String, Run>,
    /// Runs that may start when a running slot is free, oldest first.
    ready: VecDeque<String>,
    /// Finished runs, oldest first, for retention.
    finished: VecDeque<String>,
    running: usize,
    waiters: HashMap<u64, waits::Waiter>,
    next_waiter: u64,
    #[cfg(any(test, feature = "test-support"))]
    wait_observer: Option<UnboundedSender<super::wait_test_support::RegisteredWait>>,
    mode: Mode,
    shutdown_waiters: Vec<oneshot::Sender<()>>,
    tx: UnboundedSender<Command>,
    launch: Launch,
    /// Prepares each dispatched run off the runtime, in the request checks'
    /// slots and within their bound.
    preflight: Preflight,
    /// Parent of every run's stop token.
    root: CancellationToken,
    /// Estimated bytes of the finished results kept.
    retained_bytes: usize,
    /// Every run that ended with cleanup not confirmed, in the order they
    /// settled. Kept apart from the retained results, which retention may drop,
    /// so an exit status that asks "did any run leave cleanup unconfirmed" does
    /// not depend on what is still readable. Capped at
    /// [`MAX_UNCONFIRMED`]; the oldest are dropped past it.
    unconfirmed: VecDeque<(String, Cleanup)>,
    /// Test only: how far ahead of the real clock retention reads time.
    #[cfg(test)]
    retention_ahead: std::time::Duration,
}

/// Most unconfirmed runs remembered. A run costs its id and a verdict, and a
/// connection that leaves this many unconfirmed has long since answered the
/// question the list exists for.
const MAX_UNCONFIRMED: usize = 256;

/// Stops every run still going when the task ends, whether it returns or
/// panics: the unwinding drop of the `Core` is what makes a panic in the
/// supervisor's own task stop every provider.
impl Drop for Core {
    fn drop(&mut self) {
        self.root.cancel();
        self.abort_all();
    }
}

impl Core {
    pub fn new(
        mut config: SupervisorConfig,
        tx: UnboundedSender<Command>,
        launch: Launch,
        preflight: Preflight,
        cursor: Arc<AtomicU64>,
        root: CancellationToken,
    ) -> Self {
        config.limits = config.limits.normalized();
        Self {
            config,
            ids: Ids::new(),
            cursor,
            seq: 0,
            workers: HashMap::new(),
            runs: HashMap::new(),
            ready: VecDeque::new(),
            finished: VecDeque::new(),
            running: 0,
            waiters: HashMap::new(),
            next_waiter: 0,
            #[cfg(any(test, feature = "test-support"))]
            wait_observer: None,
            mode: Mode::Active,
            shutdown_waiters: Vec::new(),
            tx,
            launch,
            preflight,
            root,
            retained_bytes: 0,
            unconfirmed: VecDeque::new(),
            #[cfg(test)]
            retention_ahead: std::time::Duration::ZERO,
        }
    }

    /// Drop what has aged out, apply one command and then everything that
    /// follows from it: start what can start, wake what can wake, settle a
    /// shutdown. Pruning comes first so a read never sees a result past its
    /// retention time.
    pub fn handle(&mut self, command: Command) {
        self.prune_and_wake(self.retention_now());
        match command {
            #[cfg(any(test, feature = "test-support"))]
            Command::ObserveWaits(observer) => self.wait_observer = Some(observer),
            Command::Start {
                request,
                preflight,
                origin,
                reply,
            } => {
                let _ = reply.send(self.start(*request, preflight, *origin));
            }
            Command::FollowUp {
                worker,
                prompt,
                timeout_secs,
                reply,
            } => {
                let _ = reply.send(self.follow_up(&worker, prompt, timeout_secs));
            }
            Command::Wait {
                targets,
                after,
                applied,
                reply,
            } => {
                let _ = reply.send(self.wait(&targets, after, applied));
            }
            Command::WaitView {
                targets,
                after,
                waiter,
                timed_out,
                released,
                timeout_secs,
                reply,
            } => {
                let _ = reply.send(Ok(self.wait_view(
                    &targets,
                    after,
                    waiter,
                    timed_out,
                    released,
                    timeout_secs,
                )));
            }
            Command::Read {
                target,
                detail,
                after,
                text_offset,
                reply,
            } => {
                let _ = reply.send(self.read(&target, detail, after, text_offset));
            }
            Command::List { reply } => {
                let _ = reply.send(Ok(self.list()));
            }
            Command::Interrupt { target, reply } => {
                let _ = reply.send(self.interrupt(&target));
            }
            Command::Close { worker, reply } => {
                let _ = reply.send(self.close(&worker));
            }
            Command::Settled { run, reply } => {
                let _ = reply.send(self.settled(&run));
            }
            Command::Shutdown {
                connection_closed,
                reply,
            } => {
                let _ = reply.send(self.shutdown(connection_closed));
            }
            Command::Unconfirmed { reply } => {
                let _ = reply.send(self.unconfirmed());
            }
            Command::Unsettled { reply } => {
                let _ = reply.send(self.unsettled());
            }
            Command::InteractionChanged => self.wake_waiters(),
            Command::Respond {
                interaction,
                decision,
                reply,
            } => {
                let (status, answer) = interaction
                    .rsplit_once(':')
                    .and_then(|(run, _)| self.runs.get(run))
                    .map_or(("unknown_interaction", None), |run| {
                        run.respond(&interaction, decision)
                    });
                let _ = reply.send(super::RespondResponse {
                    interaction,
                    status,
                    answer,
                });
            }
            Command::Finished { run, result } => self.run_finished(&run, *result),
            Command::Unprepared { run, how } => self.run_unprepared(&run, how),
            Command::Crashed { run, reason } => self.run_crashed(&run, reason),
            Command::Quit => {
                let _ = self.shutdown(false);
            }
            #[cfg(test)]
            Command::AdvanceRetentionClock(by) => {
                self.retention_ahead += by;
                // Nothing follows from it here, so the next command is the one
                // that finds the results aged out, as when real time passes
                // between two commands.
                return;
            }
            #[cfg(test)]
            Command::PanicForTest => panic!("the supervisor's task was told to panic"),
        }
        self.settle_changes();
    }

    /// Runs that are running now.
    pub fn running(&self) -> usize {
        self.running
    }

    /// Stop following every run still running, for a supervisor that is going
    /// away. The provider layer stops the child of a dropped handle in the
    /// background.
    pub fn abort_all(&mut self) {
        for run in self.runs.values_mut() {
            if let Some(stop) = run.stop.take() {
                stop.cancel();
            }
            if let Some(abort) = run.abort.take() {
                abort.abort();
            }
        }
    }

    /// Remember a run whose result did not confirm cleanup. Every result is
    /// recorded through here when its run settles.
    fn note_cleanup(&mut self, id: &str, result: &RunResult) {
        if result.cleanup == Cleanup::Confirmed {
            return;
        }
        if self.unconfirmed.len() == MAX_UNCONFIRMED {
            self.unconfirmed.pop_front();
        }
        self.unconfirmed
            .push_back((id.to_string(), result.cleanup.clone()));
    }

    /// Every run that settled with cleanup not confirmed, oldest first.
    pub fn unconfirmed(&self) -> Vec<(String, Cleanup)> {
        self.unconfirmed.iter().cloned().collect()
    }

    fn next_cursor(&self) -> u64 {
        self.cursor.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn current_cursor(&self) -> u64 {
        self.cursor.load(Ordering::SeqCst)
    }

    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    fn live_workers(&self) -> usize {
        self.workers.values().filter(|w| !w.closed).count()
    }

    /// The work that follows any change.
    fn settle_changes(&mut self) {
        self.dispatch();
        self.check_shutdown();
        self.wake_waiters();
        self.prune_and_wake(self.retention_now());
    }

    /// The time retention compares a result's age with. The real clock; only
    /// a test moves it ahead.
    fn retention_now(&self) -> Instant {
        #[cfg(test)]
        return Instant::now() + self.retention_ahead;
        #[cfg(not(test))]
        Instant::now()
    }

    /// Prune, and if anything went, wake the waits that named it. A wait on a
    /// run or worker that is gone has nothing left to wait for, and it must
    /// not hold a slot against the command that follows the pruning.
    fn prune_and_wake(&mut self, now: Instant) {
        if self.prune(now) {
            self.wake_waiters();
        }
    }
}
