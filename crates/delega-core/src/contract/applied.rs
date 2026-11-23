//! What delega applied to a run: the `applied` object of a result and of a
//! dry run.
//!
//! Every value here is a name, a count, a path or a digest. An environment
//! variable's value and the text of a deny rule are never part of it.
//! `docs/api.md` ("`applied`") describes the parts for a caller.
//!
//! # When each part is present
//!
//! `env`, `deny_rules` and `context` are filled in together once the run has
//! been prepared: the request validated, the working directory and executable
//! found, the settings files read, the permissions mapped and the environment
//! resolved. A refusal before that point (an invalid request, an unusable
//! working directory, a rejected native argument, a missing provider
//! executable, unusable settings files, a permission `strict` cannot enforce,
//! an invalid or forbidden `--env` name) has `config` and `sources` only. So
//! does a refusal from the supervisor (a name in use, the worker limit) and a
//! run stopped in the queue before it started. A run that was prepared and
//! then ended without finishing (a failure, a time limit, an interrupt, or a
//! refusal at start such as an unsupported provider version) has every part
//! that applies. A refusal for a configuration file that cannot be used has no
//! `applied`, because no usable file was read and no request was built. Two
//! internal-error endings carry `config` and `sources` only, even for a
//! prepared run: a run whose task in the supervisor panicked, and a result the
//! supervisor replaced because it failed validation. `applied` is absent when
//! nothing is known.
//!
//! `sources` is recorded where the layers are merged (`crate::profile`, called
//! by `delega run` and MCP `start`), not worked out afterwards from the
//! values. A field a merge clears loses the layer that set it: an explicit
//! `ephemeral` removes a lower layer's `resume`, so `session.resume` is
//! absent; an explicit `resume` removes a lower layer's `ephemeral`, so
//! `session.ephemeral` is `default`. On a refusal after the merge, `sources`
//! holds the fields known so far. `config.sha256` is 64 lowercase hex digits;
//! a refused configuration file is not reported.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The layer that set a request field. From lowest to highest precedence:
/// built-in defaults, the user-level configuration file, a profile file, then
/// the explicit values of the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    /// The value is the contract's built-in default.
    Default,
    /// `[defaults]` in the configuration file.
    Config,
    /// The profile file.
    Profile,
    /// A command line flag of `delega run`.
    Flag,
    /// An argument of the MCP `start` tool (or `follow_up`).
    Argument,
}

impl Layer {
    /// The name the layer has in JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            Layer::Default => "default",
            Layer::Config => "config",
            Layer::Profile => "profile",
            Layer::Flag => "flag",
            Layer::Argument => "argument",
        }
    }
}

/// The configuration file that was read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AppliedConfig {
    /// The file's absolute path.
    pub path: String,
    /// The SHA-256 of the bytes that were read, as 64 lowercase hex digits.
    pub sha256: String,
}

/// How many variables a worker received, by reason.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EnvPassed {
    /// All variables the worker received. The counts below add up to it.
    pub total: usize,
    /// Passed because they are in the base set (user, shell, locale, temporary
    /// and base directories).
    pub base: usize,
    /// Passed because they are proxy or certificate settings.
    pub network: usize,
    /// Passed because they belong to the provider being run.
    pub provider: usize,
    /// Passed because the caller named them (`--env NAME`, `[env] pass`) and the
    /// environment holds them.
    pub named_by_caller: usize,
    /// Set to a value the caller gave (`--env NAME=VALUE`, `[env] set`).
    pub set_by_caller: usize,
    /// Set by delega for the provider.
    pub set_by_delega: usize,
}

/// How many variables of the caller's environment a worker did not receive, by
/// reason. Their names are not reported.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EnvWithheld {
    /// All variables of the caller's environment that were not passed.
    pub total: usize,
    /// Never passed, even when named: variables that describe the calling
    /// session, and every `DELEGA_*` variable.
    pub never_passed: usize,
    /// Not on the allowlist and not named by the caller.
    pub not_allowlisted: usize,
}

/// The environment a worker received.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AppliedEnv {
    /// What the worker received.
    pub passed: EnvPassed,
    /// What it did not.
    pub withheld: EnvWithheld,
    /// The names the caller gave with `--env` or in the configuration file's
    /// `[env]`, sorted and without duplicates, whether or not the variable was
    /// set. Names only, never values.
    pub named: Vec<String>,
}

/// A settings file that was read, and how many deny rules it held.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DenyFile {
    /// The file's path.
    pub path: String,
    /// The rules in the file, before deduplication.
    pub rules: usize,
}

/// A local settings file that was not read, and why.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SkippedFile {
    /// The file's path.
    pub path: String,
    /// Why it was ignored.
    pub reason: String,
}

/// The deny rules a Claude Code worker ran under: the files, never the rules.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AppliedDenyRules {
    /// The settings files that exist and were read, with their rule counts.
    pub files: Vec<DenyFile>,
    /// Existing local settings files that were ignored, with the reason.
    pub skipped: Vec<SkippedFile>,
    /// How many rules were passed, after deduplication and adding absolute
    /// forms.
    pub applied: usize,
}

/// What of the user's Claude Code context the worker saw.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AppliedContext {
    /// How the user's and the project's Claude Code settings reached the
    /// worker. `deny_rules` is the only mode today: the deny rules and nothing
    /// else from those files. Free text, so a reader must tolerate values it
    /// does not know.
    pub settings: String,
    /// Whether the worker loaded instruction files (such as `CLAUDE.md`).
    pub instructions: bool,
}

/// What delega applied to a run.
///
/// `config` and `sources` come from the caller that built the request; the
/// rest comes from preparing the run. Each part is absent when it is not
/// known or does not apply, which `docs/api.md` spells out part by part.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Applied {
    /// The configuration file that was read. Absent when none was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<AppliedConfig>,
    /// For each request field that ended up set, the layer that set it. The
    /// keys are the top-level request fields and `permissions.*` and
    /// `session.*` as dotted names. Absent when the caller did not say.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sources: BTreeMap<String, Layer>,
    /// The environment the worker received. Absent when the run was refused
    /// before the environment was resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<AppliedEnv>,
    /// The deny rules applied (Claude Code only). Absent for Codex and when
    /// the run was refused before the settings files were read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deny_rules: Option<AppliedDenyRules>,
    /// The Claude Code context mode. Absent for Codex and when the run was
    /// refused before it was decided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<AppliedContext>,
}

impl Applied {
    /// Whether nothing is known, so a result carries no `applied`.
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    /// `self` as the value of a result's `applied`: absent when empty.
    pub fn or_absent(self) -> Option<Self> {
        (!self.is_empty()).then_some(self)
    }
}
