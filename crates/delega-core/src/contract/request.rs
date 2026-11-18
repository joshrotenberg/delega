//! The request: what to run, with what permissions, and for how long.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::SCHEMA_VERSION;
use super::limits::DEFAULT_TIMEOUT_SECS;
use super::permissions::Permissions;

/// The provider CLI that runs the assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    /// Claude Code.
    Claude,
    /// Codex CLI.
    Codex,
}

/// How much reasoning effort to ask the provider for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    /// Least effort.
    Low,
    /// Moderate effort.
    Medium,
    /// Most effort.
    High,
}

/// How the provider session is kept or continued.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Session {
    /// Do not keep provider history for this worker.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ephemeral: bool,
    /// Continue saved history, by the provider's own session id, in a new
    /// worker. Cannot be combined with `ephemeral`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<String>,
}

impl Session {
    /// Whether this value says nothing, so it can be left out of the JSON.
    pub fn is_default(&self) -> bool {
        !self.ephemeral && self.resume.is_none()
    }
}

/// One request: starts a worker and its first run.
///
/// Unknown fields are rejected, so a misspelled field is an error and not a
/// silently ignored setting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// The contract version this request was written against. Required.
    pub schema_version: u32,
    /// Which provider runs the assignment.
    pub provider: Provider,
    /// The assignment, sent to the provider on stdin.
    pub prompt: String,
    /// Working directory of the worker. Absent means the directory of the
    /// process that receives the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// A label for the worker, unique among live workers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Extra system instructions for the worker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// Provider model name or alias. Absent keeps the provider default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Reasoning effort. Absent keeps the provider default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    /// Limit for the run, in whole seconds. Defaults to 600 (10 minutes).
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    /// What the worker may do.
    #[serde(default)]
    pub permissions: Permissions,
    /// A JSON Schema the final answer must satisfy; asks for a structured
    /// answer and validates it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer_schema: Option<Value>,
    /// Whether to keep or continue provider history.
    #[serde(default, skip_serializing_if = "Session::is_default")]
    pub session: Session,
    /// Provider options passed through, restricted by an allowlist.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub native_args: Vec<String>,
}

fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_SECS
}

impl Request {
    /// A request with the required fields and every default applied.
    pub fn new(provider: Provider, prompt: impl Into<String>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            provider,
            prompt: prompt.into(),
            cwd: None,
            name: None,
            instructions: None,
            model: None,
            effort: None,
            timeout: DEFAULT_TIMEOUT_SECS,
            permissions: Permissions::default(),
            answer_schema: None,
            session: Session::default(),
            native_args: Vec::new(),
        }
    }
}
