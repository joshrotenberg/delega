//! The arguments and responses of the supervisor's operations.
//!
//! Every response serializes as it is meant to be returned, so a surface can
//! hand it on unchanged. Compact is the default. [`Detail::Full`] adds the
//! activity history, the supervisor's own diagnostics and every field of the
//! result.

use serde::{Deserialize, Serialize};

use crate::contract::outcome::Outcome;
use crate::contract::permissions::Mode;
use crate::contract::request::Provider;
use crate::contract::result::{Cleanup, DeniedRequest, ErrorDetail, RunResult, Timing, WorkerRef};

use super::activity::ActivityEntry;
use super::error::Error;
use super::state::{RunState, WorkerState};

/// How much a read returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Detail {
    /// The answer and what went wrong; for a running run, the end of the
    /// partial text and the last few activity entries.
    #[default]
    Compact,
    /// Everything kept.
    Full,
}

/// A run or a worker, named by id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// One run.
    Run(String),
    /// One worker: its current run, or its latest if nothing is current.
    Worker(String),
}

impl Target {
    /// The target for a tool that takes `run` or `worker`. Exactly one must be
    /// given.
    pub fn from_args(run: Option<String>, worker: Option<String>) -> Result<Target, Error> {
        match (run, worker) {
            (Some(run), None) => Ok(Target::Run(run)),
            (None, Some(worker)) => Ok(Target::Worker(worker)),
            (Some(_), Some(_)) => Err(Error::InvalidArguments(
                "give either run or worker, not both".to_string(),
            )),
            (None, None) => Err(Error::InvalidArguments(
                "give a run or a worker".to_string(),
            )),
        }
    }
}

/// What `wait` waits for. At least one id is needed.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WaitTargets {
    /// Runs: the wait ends when any finishes.
    #[serde(default)]
    pub runs: Vec<String>,
    /// Workers: the wait ends when the worker's current run finishes.
    #[serde(default)]
    pub workers: Vec<String>,
}

/// The answer to `start` and to `follow_up`.
///
/// A run that could not go ahead is finished at once: `state` is `finished`,
/// `outcome` is `refused` and `error` says why. `read` returns its result. A
/// refused `start` creates no worker, so `worker` is absent.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StartResponse {
    /// The worker, absent when `start` was refused.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker: Option<String>,
    /// The run.
    pub run: String,
    /// `queued` or `running`, or `finished` for a refusal.
    pub state: RunState,
    /// Runs ahead of this one on its worker, counting the running one. Present
    /// while queued.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ahead: Option<usize>,
    /// Present only for a refusal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
    /// Present only for a refusal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorDetail>,
}

/// The answer to `follow_up`: the same as [`StartResponse`], with `worker`
/// always present.
pub type FollowUpResponse = StartResponse;

/// One run in a `wait` response.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunBrief {
    /// The run.
    pub run: String,
    /// Its worker, if it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker: Option<String>,
    /// Where it is.
    pub state: RunState,
    /// How it ended, once finished.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
    /// With `after_cursor`: whether it finished after that cursor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed: Option<bool>,
}

/// One worker in a `wait` or `list` response.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorkerBrief {
    /// The worker.
    pub worker: String,
    /// Where it is.
    pub state: WorkerState,
    /// Its running run, or the next queued one when nothing is running.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_run: Option<String>,
    /// How many runs wait behind the running one.
    pub queue_length: usize,
    /// Its latest finished run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run: Option<String>,
    /// How that run ended.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_outcome: Option<Outcome>,
    /// With `after_cursor`: whether a run of this worker finished after it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed: Option<bool>,
}

/// The answer to `wait`. A timeout is not an error: `timed_out` is true and
/// the states are current.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WaitResponse {
    /// Pass this as `after_cursor` on the next call to see only what finishes
    /// after this response.
    pub cursor: u64,
    /// True when the time passed and nothing named had finished.
    pub timed_out: bool,
    /// The wait that applied, in seconds, after the ceiling.
    pub timeout_secs: u64,
    /// The runs named, in the order given, minus the unknown ones.
    pub runs: Vec<RunBrief>,
    /// The workers named, in the order given, minus the unknown ones.
    pub workers: Vec<WorkerBrief>,
    /// Ids that name nothing: wrong, or their results have gone.
    pub unknown: Vec<String>,
}

/// A result, compact or whole.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ResultView {
    /// What a caller needs to know what happened.
    Compact(Box<CompactResult>),
    /// Every field of the result.
    Full(Box<RunResult>),
}

/// The compact form of a [`RunResult`]: the outcome, the answer, the error, the
/// cleanup verdict, the denials, the session and the timing. Usage and
/// provenance are left out.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CompactResult {
    /// Run id.
    pub run: String,
    /// The worker, absent for a refused start.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker: Option<WorkerRef>,
    /// The provider, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<Provider>,
    /// How the run ended.
    pub outcome: Outcome,
    /// The final answer, or whatever partial answer exists.
    pub text: String,
    /// The structured answer, when one was asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured: Option<serde_json::Value>,
    /// Whether it satisfied the schema.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_valid: Option<bool>,
    /// Why the run did not complete.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorDetail>,
    /// Whether everything started for the run is gone.
    pub cleanup: Cleanup,
    /// Requests answered no.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub denied: Vec<DeniedRequest>,
    /// Where the provider did not do what was asked.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub provider_notes: Vec<String>,
    /// The provider's session id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Queued, started and finished times, and the limit applied.
    pub timing: Timing,
}

impl ResultView {
    /// The view of `result` at `detail`.
    pub fn of(result: &RunResult, detail: Detail) -> Self {
        match detail {
            Detail::Full => ResultView::Full(Box::new(result.clone())),
            Detail::Compact => ResultView::Compact(Box::new(CompactResult {
                run: result.run.clone(),
                worker: result.worker.clone(),
                provider: result.provider,
                outcome: result.outcome,
                text: result.text.clone(),
                structured: result.structured.clone(),
                structured_valid: result.structured_valid,
                error: result.error.clone(),
                cleanup: result.cleanup.clone(),
                denied: result.permissions.denied.clone(),
                provider_notes: result.provider_notes.clone(),
                session_id: result.session_id.clone(),
                timing: result.timing.clone(),
            })),
        }
    }
}

/// How an unfinished run is going.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProgressView {
    /// Time waiting in the queue so far, or in total once started.
    pub queued_ms: u64,
    /// Time since the provider started. Absent while queued.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running_ms: Option<u64>,
    /// The time limit that applies to the run, in seconds.
    pub timeout_secs: u64,
    /// What the agent has written so far. Compact keeps the end; see
    /// `partial_text_truncated`.
    pub partial_text: String,
    /// True when `partial_text` is not all the agent has written.
    pub partial_text_truncated: bool,
    /// Recent activity, oldest first. Compact returns the last few; full
    /// returns all that are kept.
    pub activity: Vec<ActivityEntry>,
    /// Entries reported over the whole run.
    pub activity_total: u64,
    /// Entries not in `activity`: dropped to stay in bounds, or older than the
    /// compact window, or at or before `after_cursor`.
    pub activity_omitted: u64,
    /// The supervisor's own facts about the run. Full detail only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<RunDiagnostics>,
}

/// What the supervisor knows about a run beyond its progress. The provider
/// layer does not expose the child's process diagnostics, so none appear here.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunDiagnostics {
    /// The provider the run uses.
    pub provider: Provider,
    /// The permission mode of the request.
    pub mode: Mode,
    /// When the run was admitted, RFC 3339 in UTC.
    pub queued_at: String,
    /// When the provider was started.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// Why a stop was requested: `interrupt`, `close` or `shutdown`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_requested: Option<&'static str>,
    /// Activity entries dropped from the front of the history.
    pub activity_dropped: u64,
}

/// The answer to `read`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReadResponse {
    /// The worker, when the run has one or the target was one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker: Option<String>,
    /// The worker's state, when the target was a worker.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker_state: Option<WorkerState>,
    /// The run read. Absent only for a worker with no run kept.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Where the run is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<RunState>,
    /// How it ended, once finished.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
    /// The latest cursor. Pass it as `after_cursor` to see only later activity.
    pub cursor: u64,
    /// The result, once the run has finished.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<ResultView>,
    /// How the run is going. Present while it is queued or running, and with
    /// full detail also once it has finished.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<ProgressView>,
    /// Anything the caller should know, such as that no run is kept.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// The limits in force and what is in use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LimitsView {
    /// Most live workers.
    pub max_live_workers: usize,
    /// Live workers now.
    pub live_workers: usize,
    /// Most running runs.
    pub max_running_runs: usize,
    /// Running runs now.
    pub running_runs: usize,
    /// Most queued runs behind one worker.
    pub max_queued_per_worker: usize,
    /// Most finished results kept.
    pub retained_results: usize,
    /// Finished results kept now.
    pub retained_now: usize,
    /// How long a finished result stays readable, in seconds.
    pub retention_secs: u64,
    /// The longest one wait blocks, in seconds.
    pub wait_ceiling_secs: u64,
}

/// One worker in `list`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorkerView {
    /// The state and the runs.
    #[serde(flatten)]
    pub brief: WorkerBrief,
    /// The worker's name, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The provider.
    pub provider: Provider,
    /// The provider's session id, once a run has reported one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Runs of this worker whose results are kept, plus unfinished ones.
    pub runs: usize,
}

/// The answer to `list`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ListResponse {
    /// The latest cursor.
    pub cursor: u64,
    /// Limits and use.
    pub limits: LimitsView,
    /// Live workers, then closed ones whose results are still kept, oldest
    /// first.
    pub workers: Vec<WorkerView>,
}

/// The answer to `interrupt`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InterruptResponse {
    /// The worker the run belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker: Option<String>,
    /// The run. Absent only for a worker with no run kept.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// `finished` once the run has stopped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<RunState>,
    /// True when the run is finished. False only when the stop did not settle
    /// within the bound; read the run later.
    pub settled: bool,
    /// True when the run had not started and was removed from the queue.
    pub was_queued: bool,
    /// The result: `interrupted` for a run that was stopped, or the result
    /// unchanged for a run that had already finished.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<ResultView>,
}

/// The answer to `close`. Closing twice answers the same way, with
/// `already_closed` true.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CloseResponse {
    /// The worker.
    pub worker: String,
    /// Always `closed`.
    pub state: WorkerState,
    /// True when the worker was closed before this call.
    pub already_closed: bool,
    /// Queued runs discarded, each now finished as `interrupted`.
    pub discarded: Vec<String>,
    /// The running run that was stopped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped_run: Option<String>,
    /// False when the running run did not settle within the bound.
    pub settled: bool,
    /// The stopped run's result, once settled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<ResultView>,
}

/// The answer to `shutdown`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShutdownReport {
    /// Workers that were live and are now closed.
    pub workers_closed: usize,
    /// Running runs that were stopped.
    pub runs_stopped: usize,
    /// Queued runs that were discarded.
    pub runs_discarded: usize,
    /// Runs that had not settled when the bound passed. Empty when everything
    /// stopped.
    pub unsettled: Vec<String>,
}
