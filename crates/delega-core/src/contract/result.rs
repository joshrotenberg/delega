//! The result: what a run produced and how it ended.
//!
//! Unlike a request, a result accepts fields it does not know, so a newer
//! producer does not break an older reader within one schema version.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::SCHEMA_VERSION;
use super::outcome::Outcome;
use super::permissions::Mode;
use super::request::Provider;
use super::validate::Problem;

/// The worker a run belonged to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkerRef {
    /// Worker id.
    pub id: String,
    /// The worker's name, if the request gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Why a run did not complete. Present unless the outcome is `completed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorDetail {
    /// A short machine-readable label for the kind of error. Free text, so a
    /// reader must tolerate values it does not know. The values used so far:
    /// `invalid_request` (a refused run whose request failed validation),
    /// `timed_out` and `interrupted` (set by the matching constructors).
    pub kind: String,
    /// A human-readable description.
    pub message: String,
    /// The provider process exit code, when it exited with one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_exit_code: Option<i32>,
}

/// Whether the provider process and its descendants are gone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Cleanup {
    /// True when everything started for the run is confirmed stopped.
    pub confirmed: bool,
    /// Process ids that could not be confirmed stopped.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pids: Vec<u32>,
    /// Process group ids that could not be confirmed stopped.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub process_groups: Vec<u32>,
    /// Why stopping could not be confirmed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl Cleanup {
    /// Everything started for the run is confirmed stopped.
    pub fn confirmed() -> Self {
        Self {
            confirmed: true,
            pids: Vec::new(),
            process_groups: Vec::new(),
            reason: None,
        }
    }
}

/// A request the provider made that was answered no.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeniedRequest {
    /// What the provider asked to do.
    pub request: String,
}

/// The permissions that applied and what was refused under them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AppliedPermissions {
    /// The mode that was applied. `read` when the run never started.
    #[serde(default)]
    pub mode: Mode,
    /// Requests answered no.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub denied: Vec<DeniedRequest>,
}

/// Token counts and cost, where the provider reports them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Usage {
    /// Tokens sent to the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    /// Tokens the model produced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// Input tokens served from a cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u64>,
    /// Tokens spent on reasoning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
    /// Cost in US dollars, when the provider reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

/// When the run was queued, started and finished.
///
/// Times are RFC 3339 strings in UTC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Timing {
    /// When the run was queued.
    pub queued_at: String,
    /// When the provider was started. Absent if the run never started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// When the run finished.
    pub finished_at: String,
    /// Time from start to finish, in milliseconds. Zero if it never started.
    pub duration_ms: u64,
    /// The time limit that applied, in seconds. Same name and unit as the
    /// request's `timeout_secs`.
    pub timeout_secs: u64,
}

/// Which build and which provider executable produced the result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Provenance {
    /// The delega version.
    pub delega_version: String,
    /// The build identity, such as a source revision, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    /// Path of the provider executable that was used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_executable: Option<String>,
    /// Version the provider executable reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_version: Option<String>,
}

/// The result of one run. Every run produces exactly one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunResult {
    /// The contract version of this result.
    pub schema_version: u32,
    /// Run id.
    pub run: String,
    /// The worker the run belonged to. Absent when no worker was created, such
    /// as a refused request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<WorkerRef>,
    /// The provider that ran it. Absent when the request named none or an
    /// unknown one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<Provider>,
    /// How the run ended.
    pub outcome: Outcome,
    /// The final answer, or empty.
    pub text: String,
    /// The structured answer, when the request gave an `answer_schema`.
    ///
    /// A JSON `null` answer is not representable: it reads back as absent,
    /// because absent and null share one representation here. Report such an
    /// answer in `text` with `structured_valid` left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured: Option<Value>,
    /// Whether `structured` satisfied the schema. Present with `structured`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_valid: Option<bool>,
    /// Why the run did not complete. Present unless `completed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorDetail>,
    /// Whether the processes started for the run are gone.
    pub cleanup: Cleanup,
    /// The permissions that applied and what was denied.
    #[serde(default)]
    pub permissions: AppliedPermissions,
    /// Where the provider did not do what the request asked, or did it
    /// differently.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_notes: Vec<String>,
    /// The provider's own session id, for the provider's `resume`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Token counts and cost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// When the run was queued, started and finished.
    pub timing: Timing,
    /// The build and provider executable behind the result.
    pub provenance: Provenance,
}

/// What every constructor of [`RunResult`] needs, whatever the outcome.
#[derive(Debug, Clone, PartialEq)]
pub struct RunContext {
    /// Run id.
    pub run: String,
    /// The worker, if one exists.
    pub worker: Option<WorkerRef>,
    /// The provider, if the request named a known one.
    pub provider: Option<Provider>,
    /// The permissions that applied.
    pub permissions: AppliedPermissions,
    /// Whether cleanup is confirmed.
    pub cleanup: Cleanup,
    /// When the run was queued, started and finished.
    pub timing: Timing,
    /// The build and provider executable behind the result.
    pub provenance: Provenance,
}

impl RunResult {
    fn build(ctx: RunContext, outcome: Outcome, text: String, error: Option<ErrorDetail>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            run: ctx.run,
            worker: ctx.worker,
            provider: ctx.provider,
            outcome,
            text,
            structured: None,
            structured_valid: None,
            error,
            cleanup: ctx.cleanup,
            permissions: ctx.permissions,
            provider_notes: Vec::new(),
            session_id: None,
            usage: None,
            timing: ctx.timing,
            provenance: ctx.provenance,
        }
    }

    /// The provider finished and returned a final answer. Has no `error`.
    pub fn completed(ctx: RunContext, text: impl Into<String>) -> Self {
        Self::build(ctx, Outcome::Completed, text.into(), None)
    }

    /// The provider or delega hit an error. `text` is whatever partial answer
    /// exists, possibly empty.
    pub fn failed(ctx: RunContext, error: ErrorDetail, text: impl Into<String>) -> Self {
        Self::build(ctx, Outcome::Failed, text.into(), Some(error))
    }

    /// The time limit passed. The error kind is `timed_out`.
    pub fn timed_out(ctx: RunContext, message: impl Into<String>, text: impl Into<String>) -> Self {
        let error = ErrorDetail {
            kind: "timed_out".into(),
            message: message.into(),
            provider_exit_code: None,
        };
        Self::build(ctx, Outcome::TimedOut, text.into(), Some(error))
    }

    /// The caller stopped the run or went away. The error kind is
    /// `interrupted`.
    pub fn interrupted(
        ctx: RunContext,
        message: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        let error = ErrorDetail {
            kind: "interrupted".into(),
            message: message.into(),
            provider_exit_code: None,
        };
        Self::build(ctx, Outcome::Interrupted, text.into(), Some(error))
    }

    /// The run did not go ahead, for the stated reason. The provider never
    /// started, so `timing.started_at` is cleared and `text` is empty.
    pub fn refused(
        mut ctx: RunContext,
        kind: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        ctx.timing.started_at = None;
        let error = ErrorDetail {
            kind: kind.into(),
            message: message.into(),
            provider_exit_code: None,
        };
        Self::build(ctx, Outcome::Refused, String::new(), Some(error))
    }

    /// A refusal because the request failed validation. The error kind is
    /// `invalid_request` and the message lists every problem.
    pub fn refused_invalid(ctx: RunContext, problems: &[Problem]) -> Self {
        let message = problems
            .iter()
            .map(Problem::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        Self::refused(ctx, "invalid_request", message)
    }
}
