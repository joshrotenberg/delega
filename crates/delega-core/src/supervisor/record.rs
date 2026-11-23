//! What the supervisor keeps about each worker and each run.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use tokio::sync::oneshot;
use tokio::task::AbortHandle;
use tokio_util::sync::CancellationToken;

use crate::contract::applied::Applied;
use crate::contract::permissions::Mode;
use crate::contract::request::{Provider, Request};
use crate::contract::result::{RunResult, WorkerRef};

use super::activity::Activity;
use super::state::{RunState, WorkerState};

/// How a run came to exist, which decides how its request is finished at
/// dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// The worker's first run: its request is complete.
    First,
    /// A follow-up: the session to resume is the worker's latest one at the
    /// moment the run starts, not when it was queued.
    FollowUp,
}

/// Why a running run was asked to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StopReason {
    Interrupt,
    Close,
    Shutdown,
    ConnectionClosed,
}

impl StopReason {
    pub fn word(self) -> &'static str {
        match self {
            StopReason::Interrupt => "interrupt",
            StopReason::Close => "close",
            StopReason::Shutdown => "shutdown",
            StopReason::ConnectionClosed => "connection_closed",
        }
    }
}

/// One run: queued, running or finished.
pub(super) struct Run {
    pub id: String,
    pub asks: Option<Arc<crate::provider::ask::Asks>>,
    pub ask_seq: Arc<std::sync::atomic::AtomicU64>,
    pub worker: Option<String>,
    pub kind: Kind,
    pub state: RunState,
    /// The request to run. Taken when the run is dispatched.
    pub request: Option<Request>,
    pub provider: Provider,
    pub mode: Mode,
    pub timeout_secs: u64,
    /// What the caller said of `applied` for this run: the configuration file
    /// and the layer of each request field.
    pub origin: Applied,
    pub queued_at: SystemTime,
    pub queued: Instant,
    pub started_at: Option<SystemTime>,
    pub started: Option<Instant>,
    pub activity: Arc<Activity>,
    /// Cancelled to stop the running run.
    pub stop: Option<CancellationToken>,
    /// Aborts the task that follows the provider, when it must be left behind.
    pub abort: Option<AbortHandle>,
    pub stop_reason: Option<StopReason>,
    /// Whether the run is in the dispatch queue.
    pub in_ready: bool,
    /// Told when the run finishes.
    pub settle: Vec<oneshot::Sender<()>>,
    pub result: Option<RunResult>,
    /// Estimated bytes this run's kept result takes.
    pub bytes: usize,
    pub finished_seq: Option<u64>,
    pub finished_at: Option<Instant>,
}

impl Run {
    pub fn respond(
        &self,
        id: &str,
        decision: crate::contract::result::PermissionDecision,
    ) -> (
        &'static str,
        Option<crate::contract::result::AskedPermission>,
    ) {
        if let Some(asks) = &self.asks {
            return asks.respond(id, decision);
        }
        let answer = self.result.as_ref().and_then(|result| {
            result
                .permissions
                .asked
                .iter()
                .find(|answer| answer.interaction.id == id)
        });
        answer.map_or(("unknown_interaction", None), |answer| {
            (
                crate::provider::ask::resolved_status(answer),
                Some(answer.clone()),
            )
        })
    }

    pub fn pending(&self) -> Vec<crate::contract::result::PermissionInteraction> {
        self.asks
            .as_ref()
            .map_or_else(Vec::new, |asks| asks.pending())
    }

    pub fn ask_changed(&self, after: u64) -> bool {
        self.ask_seq.load(std::sync::atomic::Ordering::SeqCst) > after
    }

    pub fn new(id: String, kind: Kind, request: &Request) -> Self {
        Self {
            id,
            asks: None,
            ask_seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            worker: None,
            kind,
            state: RunState::Queued,
            request: None,
            provider: request.provider,
            mode: request.permissions.mode,
            timeout_secs: request.timeout_secs,
            origin: Applied::default(),
            queued_at: SystemTime::now(),
            queued: Instant::now(),
            started_at: None,
            started: None,
            activity: Arc::new(Activity::default()),
            stop: None,
            abort: None,
            stop_reason: None,
            in_ready: false,
            settle: Vec::new(),
            result: None,
            bytes: 0,
            finished_seq: None,
            finished_at: None,
        }
    }
}

/// One worker: a provider conversation and the runs given to it.
pub(super) struct Worker {
    pub id: String,
    pub seq: u64,
    pub name: Option<String>,
    /// The first request with an empty prompt: the settings a follow-up keeps.
    pub template: Request,
    /// The `origin` the worker was started with, which its follow-ups keep.
    pub origin: Applied,
    pub provider: Provider,
    pub state: WorkerState,
    /// The worker's session: the one it was started to resume, or else the
    /// first one a run reported. It does not change once set.
    pub session_id: Option<String>,
    /// The run that is running.
    pub current: Option<String>,
    /// Runs waiting, next first.
    pub queue: VecDeque<String>,
    /// Every run of this worker still kept, oldest first.
    pub runs: Vec<String>,
    /// The latest run that was queued or running and has finished.
    pub last_run: Option<String>,
    /// The cursor at which any run of this worker last finished.
    pub last_finish_seq: u64,
    pub closed: bool,
}

impl Worker {
    pub fn worker_ref(&self) -> WorkerRef {
        WorkerRef {
            id: self.id.clone(),
            name: self.name.clone(),
        }
    }
}
