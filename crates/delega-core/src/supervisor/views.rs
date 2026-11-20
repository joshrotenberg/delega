//! The arguments and responses of the supervisor's operations.
//!
//! Every response serializes as it is meant to be returned, so a surface can
//! hand it on unchanged. Compact is the default. [`Detail::Full`] adds the
//! activity history, the supervisor's own diagnostics and every field of the
//! result.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::contract::applied::Applied;
use crate::contract::outcome::Outcome;
use crate::contract::permissions::Mode;
use crate::contract::request::Provider;
use crate::contract::result::{Cleanup, DeniedRequest, ErrorDetail, RunResult, Timing, WorkerRef};

use super::activity::ActivityEntry;
use super::error::Error;
use super::limits::{COMPACT_RESULT_TEXT_CHARS, COMPACT_STRUCTURED_BYTES};
use super::state::{RunState, WorkerState};

/// How much a read returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
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
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
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
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
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
/// the states are current. A wait that shutdown released says
/// `released_by_shutdown`.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct WaitResponse {
    /// Pass this as `after_cursor` on the next call to see only what finishes
    /// after this response.
    pub cursor: u64,
    /// True when the time passed and nothing named had finished.
    pub timed_out: bool,
    /// True when shutdown released the wait. Nothing named need have finished:
    /// a run can still be `running` while it is stopped, so `timed_out: false`
    /// does not mean something finished. Absent otherwise.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub released_by_shutdown: bool,
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
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum ResultView {
    /// What a caller needs to know what happened.
    Compact(Box<CompactResult>),
    /// Every field of the result.
    Full(Box<RunResult>),
}

/// The compact form of a [`RunResult`]: the outcome, the answer, the error, the
/// cleanup verdict, the denials, `applied`, the session and the timing. Usage
/// and provenance are left out.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
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
    /// The final answer, or whatever partial answer exists, cut to
    /// [`COMPACT_RESULT_TEXT_CHARS`] characters. See `text_truncated`.
    pub text: String,
    /// True when `text` is only the start of the answer. `detail: full` returns
    /// all of it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub text_truncated: bool,
    /// The full length of the answer in characters. Present when truncated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_chars: Option<usize>,
    /// The character offset `text` starts at, when the read gave one and the
    /// text is a window.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_offset: Option<usize>,
    /// The structured answer, when one was asked for, unless it is larger than
    /// [`COMPACT_STRUCTURED_BYTES`] serialized. See `structured_omitted`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured: Option<serde_json::Value>,
    /// True when the structured answer was left out for its size. `detail:
    /// full` returns it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub structured_omitted: bool,
    /// Its serialized size in bytes. Present when omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_bytes: Option<usize>,
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
    /// What delega applied to the run. Small, so a compact read carries it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applied: Option<Applied>,
    /// The provider's session id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Queued, started and finished times, and the limit applied.
    pub timing: Timing,
}

/// Where a window of the answer text sits in the whole answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct TextWindow {
    /// Characters before the window.
    pub offset: usize,
    /// Characters in the window.
    pub length: usize,
    /// Characters in the whole answer.
    pub total: usize,
    /// True when the answer goes on after the window. Read again with
    /// `offset + length` as the next offset.
    pub more: bool,
}

impl ResultView {
    /// The view of `result` at `detail`.
    pub fn of(result: &RunResult, detail: Detail) -> Self {
        Self::windowed(result, detail, None).0
    }

    /// The view of `result` at `detail`, with the answer text read from the
    /// character `offset`. A compact view always returns a window of at most
    /// [`COMPACT_RESULT_TEXT_CHARS`] characters. A full view returns all of the
    /// text unless an offset is given, and then the same window. The window is
    /// returned when an offset was given.
    pub fn windowed(
        result: &RunResult,
        detail: Detail,
        offset: Option<usize>,
    ) -> (Self, Option<TextWindow>) {
        // Plain ASCII, the common case, is sliced by byte; anything else is
        // walked by character, so a window never cuts a character in two.
        let ascii = result.text.is_ascii();
        let total = if ascii {
            result.text.len()
        } else {
            result.text.chars().count()
        };
        let start = offset.unwrap_or(0).min(total);
        let paged = offset.is_some();
        let whole = detail == Detail::Full && !paged;
        let text: String = if whole {
            result.text.clone()
        } else if ascii {
            let end = (start + COMPACT_RESULT_TEXT_CHARS).min(total);
            result.text[start..end].to_string()
        } else {
            result
                .text
                .chars()
                .skip(start)
                .take(COMPACT_RESULT_TEXT_CHARS)
                .collect()
        };
        let length = if whole {
            total
        } else if ascii {
            text.len()
        } else {
            text.chars().count()
        };
        let more = start + length < total;
        let window = paged.then_some(TextWindow {
            offset: start,
            length,
            total,
            more,
        });
        let view = match detail {
            Detail::Full if !paged => ResultView::Full(Box::new(result.clone())),
            Detail::Full => {
                let mut cut = result.clone();
                cut.text = text;
                ResultView::Full(Box::new(cut))
            }
            Detail::Compact => {
                let truncated = start > 0 || more;
                let structured_bytes = result.structured.as_ref().map(|v| v.to_string().len());
                let omit = structured_bytes.is_some_and(|n| n > COMPACT_STRUCTURED_BYTES);
                ResultView::Compact(Box::new(CompactResult {
                    run: result.run.clone(),
                    worker: result.worker.clone(),
                    provider: result.provider,
                    outcome: result.outcome,
                    text,
                    text_truncated: truncated,
                    text_chars: truncated.then_some(total),
                    text_offset: (paged && truncated).then_some(start),
                    structured: if omit {
                        None
                    } else {
                        result.structured.clone()
                    },
                    structured_omitted: omit,
                    structured_bytes: if omit { structured_bytes } else { None },
                    structured_valid: result.structured_valid,
                    error: result.error.clone(),
                    cleanup: result.cleanup.clone(),
                    denied: result.permissions.denied.clone(),
                    provider_notes: result.provider_notes.clone(),
                    applied: result.applied.clone(),
                    session_id: result.session_id.clone(),
                    timing: result.timing.clone(),
                }))
            }
        };
        (view, window)
    }
}

/// How an unfinished run is going.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
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
    /// Events reported over the whole run. A run of identical `system`
    /// events shown as one entry counts each event.
    pub activity_total: u64,
    /// Events not in `activity` as their own entry: dropped to stay in bounds,
    /// older than the compact window, at or before `after_cursor`, or folded
    /// into a later entry of a run of identical `system` events.
    pub activity_omitted: u64,
    /// The supervisor's own facts about the run. Full detail only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<RunDiagnostics>,
}

/// What the supervisor knows about a run beyond its progress. The provider
/// layer does not expose the child's process diagnostics, so none appear here.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
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
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
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
    /// Where the answer text of `result` sits in the whole answer. Present when
    /// the read gave a `text_offset`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_window: Option<TextWindow>,
    /// How the run is going. Present while it is queued or running, and with
    /// full detail also once it has finished.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<ProgressView>,
    /// Anything the caller should know, such as that no run is kept.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// The limits in force and what is in use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
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
    /// Most bytes of finished results kept, estimated.
    pub retained_bytes: usize,
    /// Estimated bytes of the results kept now.
    pub retained_bytes_now: usize,
    /// How long a finished result stays readable, in seconds.
    pub retention_secs: u64,
    /// The longest one wait blocks, in seconds.
    pub wait_ceiling_secs: u64,
    /// The timeout a wait uses when it is given none, in seconds.
    pub default_wait_secs: u64,
}

/// One worker in `list`.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
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
    /// Whether `follow_up` can continue this worker: it is not closed, was not
    /// started `ephemeral`, and has a session or a run that may still report
    /// one. A worker whose runs ended without reporting a session cannot, and a
    /// follow-up to it is refused.
    pub can_follow_up: bool,
    /// Runs of this worker whose results are kept, plus unfinished ones.
    pub runs: usize,
}

/// The answer to `list`.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
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
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
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
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
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
    /// Present when the supervisor's own task had failed: what happened to the
    /// providers it was running. A report with this set is not a clean stop.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed: Option<String>,
}
