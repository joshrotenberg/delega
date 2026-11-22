//! The messages the supervisor's task receives.
//!
//! A handler sends one command and awaits its reply. A command that has to
//! wait for a run, such as `wait` or `interrupt`, replies at once with
//! something to wait on, so the supervisor's task never waits for a provider.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use tokio::sync::oneshot;

use crate::contract::applied::Applied;
use crate::contract::request::Request;
use crate::contract::result::{Cleanup, RunResult};

use super::error::Error;
use super::views::{
    CloseResponse, Detail, InterruptResponse, ListResponse, ReadResponse, ResultView,
    StartResponse, Target, WaitResponse, WaitTargets,
};

pub(super) type Reply<T> = oneshot::Sender<Result<T, Error>>;

/// What `wait` gets back before it waits.
pub(super) enum WaitStart {
    /// Something named has finished already, or nothing named can.
    Ready,
    /// Shutdown has begun and the wait has a cursor, so it answers at once
    /// without anything named having finished after the cursor.
    Released,
    /// Nothing named has finished and the wait is not registered: either it
    /// was given no time, or it would block and as many waits as the limit
    /// allows are blocked already. This is its answer, built in the same
    /// command.
    Answered(Box<WaitResponse>),
    /// Registered. `rx` fires when something named finishes, or when shutdown
    /// releases the wait. The caller clears `blocked` as soon as it stops
    /// waiting for `rx`, so the registration no longer counts as a wait in
    /// flight, and keeps `rx` until it has its view.
    Pending {
        id: u64,
        rx: oneshot::Receiver<Wake>,
        blocked: Arc<AtomicBool>,
    },
}

/// Why a registered wait fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Wake {
    /// Something named finished.
    Finished,
    /// Shutdown began while nothing named had finished.
    Shutdown,
}

/// What `interrupt` gets back.
pub(super) enum Stopping {
    /// Nothing is left to wait for.
    Done(Box<InterruptResponse>),
    /// The run was asked to stop. `rx` fires when it has finished.
    Pending {
        base: Box<InterruptResponse>,
        run: String,
        rx: oneshot::Receiver<()>,
    },
}

/// What `close` gets back.
pub(super) enum Closing {
    /// Nothing is left to wait for.
    Done(Box<CloseResponse>),
    /// The running run was asked to stop. `rx` fires when it has finished.
    Pending {
        base: Box<CloseResponse>,
        run: String,
        rx: oneshot::Receiver<()>,
    },
}

/// What `shutdown` gets back.
pub(super) struct ShutdownStart {
    pub workers_closed: usize,
    pub runs_stopped: usize,
    pub runs_discarded: usize,
    /// Fires when no run is running. Absent when none was.
    pub rx: Option<oneshot::Receiver<()>>,
}

pub(super) enum Command {
    Start {
        request: Box<Request>,
        /// The refusal found by the filesystem checks done off this task, as
        /// error kind and message.
        preflight: Option<(String, String)>,
        /// What the caller knows of `applied`: the configuration file and
        /// which layer set each field.
        origin: Box<Applied>,
        reply: Reply<StartResponse>,
    },
    FollowUp {
        worker: String,
        prompt: String,
        timeout_secs: Option<u64>,
        reply: Reply<StartResponse>,
    },
    Wait {
        targets: WaitTargets,
        after: Option<u64>,
        /// The time the wait may block, after the default and the ceiling.
        applied: Duration,
        reply: Reply<WaitStart>,
    },
    WaitView {
        targets: WaitTargets,
        after: Option<u64>,
        /// The registration to remove, when the caller gave up waiting.
        waiter: Option<u64>,
        timed_out: bool,
        /// Shutdown released the wait, so nothing named need have finished.
        released: bool,
        timeout_secs: u64,
        reply: Reply<WaitResponse>,
    },
    Read {
        target: Target,
        detail: Detail,
        after: Option<u64>,
        text_offset: Option<usize>,
        reply: Reply<ReadResponse>,
    },
    List {
        reply: Reply<ListResponse>,
    },
    Interrupt {
        target: Target,
        reply: Reply<Stopping>,
    },
    Close {
        worker: String,
        reply: Reply<Closing>,
    },
    /// The compact result of a run, if it has finished.
    Settled {
        run: String,
        reply: oneshot::Sender<Option<ResultView>>,
    },
    Shutdown {
        reply: oneshot::Sender<ShutdownStart>,
    },
    /// The runs that are still running.
    Unsettled {
        reply: oneshot::Sender<Vec<String>>,
    },
    /// Every run that settled with cleanup not confirmed.
    Unconfirmed {
        reply: oneshot::Sender<Vec<(String, Cleanup)>>,
    },
    /// A run's task reported its result.
    Finished {
        run: String,
        result: Box<RunResult>,
    },
    /// A run's task ended without a result.
    Crashed {
        run: String,
        reason: String,
    },
    /// Stop everything and end the task.
    Quit,
    /// Move the time retention reads forward by this much, and do nothing
    /// else: what has aged out is pruned by the command that follows.
    #[cfg(test)]
    AdvanceRetentionClock(Duration),
    /// Make the task panic.
    #[cfg(test)]
    PanicForTest,
}
