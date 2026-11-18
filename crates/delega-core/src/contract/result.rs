//! The result: what a run produced and how it ended.
//!
//! Unlike a request, a result accepts fields it does not know, so a newer
//! producer does not break an older reader within one schema version.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::outcome::Outcome;
use super::permissions::Mode;
use super::request::Provider;

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
    /// A short machine-readable label for the kind of error.
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

/// A request the provider made that was answered no.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeniedRequest {
    /// What the provider asked to do.
    pub request: String,
}

/// The permissions that applied and what was refused under them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AppliedPermissions {
    /// The mode that was applied.
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
    /// The time limit that applied, in seconds.
    pub time_limit_secs: u64,
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
    /// The worker the run belonged to.
    pub worker: WorkerRef,
    /// The provider that ran it.
    pub provider: Provider,
    /// How the run ended.
    pub outcome: Outcome,
    /// The final answer, or empty.
    pub text: String,
    /// The structured answer, when the request gave an `answer_schema`.
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
