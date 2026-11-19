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

use super::command::Command;
use super::config::SupervisorConfig;
use super::ids::Ids;
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
    mode: Mode,
    shutdown_waiters: Vec<oneshot::Sender<()>>,
    tx: UnboundedSender<Command>,
    launch: Launch,
    /// Parent of every run's stop token.
    root: CancellationToken,
    /// Estimated bytes of the finished results kept.
    retained_bytes: usize,
}

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
            mode: Mode::Active,
            shutdown_waiters: Vec::new(),
            tx,
            launch,
            root,
            retained_bytes: 0,
        }
    }

    /// Drop what has aged out, apply one command and then everything that
    /// follows from it: start what can start, wake what can wake, settle a
    /// shutdown. Pruning comes first so a read never sees a result past its
    /// retention time.
    pub fn handle(&mut self, command: Command) {
        self.prune(Instant::now());
        match command {
            Command::Start {
                request,
                preflight,
                reply,
            } => {
                let _ = reply.send(self.start(*request, preflight));
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
                reply,
            } => {
                let _ = reply.send(self.wait(&targets, after));
            }
            Command::WaitView {
                targets,
                after,
                waiter,
                timed_out,
                timeout_secs,
                reply,
            } => {
                let _ = reply.send(Ok(self.wait_view(
                    &targets,
                    after,
                    waiter,
                    timed_out,
                    timeout_secs,
                )));
            }
            Command::Read {
                target,
                detail,
                after,
                reply,
            } => {
                let _ = reply.send(self.read(&target, detail, after));
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
            Command::Shutdown { reply } => {
                let _ = reply.send(self.shutdown());
            }
            Command::Unsettled { reply } => {
                let _ = reply.send(self.unsettled());
            }
            Command::Finished { run, result } => self.run_finished(&run, *result),
            Command::Crashed { run, reason } => self.run_crashed(&run, &reason),
            Command::Quit => {
                let _ = self.shutdown();
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
        self.prune(Instant::now());
    }
}
