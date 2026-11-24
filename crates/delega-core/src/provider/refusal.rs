//! A reason a run cannot go ahead, found before the provider starts.

use std::fmt;

/// Why a request is refused before the provider starts. It becomes the error
/// of a `refused` result: `kind` is the error kind and `message` its text.
///
/// Kinds used by this layer: `invalid_request`, `invalid_answer_schema`,
/// `native_arg_rejected`, `cwd_unusable`, `directory_unusable`,
/// `unsupported_command`, `permissions_not_enforceable`, `provider_not_found`,
/// `unsupported_provider_version`, `invalid_env_name`, `forbidden_env_name`,
/// `settings_unreadable`, `deny_rules_over_limit` and `managed_mcp_exclusive`.
///
/// Error kinds of started runs, besides `timed_out` and `interrupted`:
/// `provider_error` (the provider reported failure), `provider_turn_limit` and
/// `provider_budget_limit` (Claude Code reported a turn or budget limit),
/// `provider_usage_limit`, `provider_auth`, `provider_billing`,
/// `provider_capacity`, `provider_model_unavailable`,
/// `provider_context_exhausted` and `provider_content_policy` (the provider's
/// failure names a usage limit, an account, billing, capacity or model problem,
/// a full context window, or a content-policy refusal),
/// `provider_exit`, `no_final_message`, `malformed_output`, `answer_not_json`,
/// `session_mismatch` (a resumed run reported another session),
/// `session_not_found` (the session to resume does not exist),
/// `provider_preparation_failed`, `permission_denied` (refused after start),
/// `spawn_failed`, `process_signaled`, `wait_failed` and `internal_error`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// A short machine-readable label.
    pub kind: String,
    /// A human-readable description. It never contains environment values.
    pub message: String,
}

impl Refusal {
    pub(super) fn new(kind: &str, message: impl Into<String>) -> Self {
        Self {
            kind: kind.to_string(),
            message: message.into(),
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

impl std::error::Error for Refusal {}

/// Cut `text` to at most `max` characters, marking a cut.
pub(super) fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let kept: String = text.chars().take(max).collect();
        format!("{kept}...")
    }
}
