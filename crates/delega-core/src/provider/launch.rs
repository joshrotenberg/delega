//! The shapes the provider modules share when they build a launch.

use crate::contract::request::Request;

use super::deny::DenyRules;
use super::hook::HookCommand;
use super::permissions::Scope;

/// What a provider module reads to build its launch.
pub(super) struct Inputs<'a> {
    pub request: &'a Request,
    pub scope: &'a Scope,
    pub hook: Option<&'a HookCommand>,
    /// The system-instruction text.
    pub system: &'a str,
    /// Allowlisted `native_args`, as `flag value` pairs.
    pub native: &'a [String],
    /// The deny rules read from the user's and the project's Claude Code
    /// settings. Empty for Codex.
    pub deny: &'a DenyRules,
}

/// An argument whose text is long and is shown elided in a dry run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Elide {
    /// Index in the argument list.
    pub index: usize,
    /// Text kept in front of the elision, such as `developer_instructions=`.
    pub prefix: &'static str,
}

/// What a provider module produces.
#[derive(Debug, Clone, Default)]
pub(super) struct Built {
    /// Launch arguments, after the program.
    pub args: Vec<String>,
    /// Environment variables the adapter owns.
    pub env: Vec<(String, String)>,
    /// Notes for the result: where the provider cannot do what was asked.
    pub notes: Vec<String>,
    /// Parts of the requested permissions this provider cannot enforce as
    /// stated, each a short phrase. They are the same cases that produce a
    /// "wider than requested" or "not granted" note. With `permissions.strict`
    /// a non-empty list refuses the run.
    pub unenforceable: Vec<String>,
    /// Provider settings carrying the permission mapping.
    pub settings: Vec<String>,
    /// The commands the provider is set up to allow.
    pub commands: Vec<String>,
    /// Arguments to elide in a dry run.
    pub elide: Vec<Elide>,
    /// Whether the exact command guard is installed, which needs a Claude Code
    /// release with the blocking hook interface.
    pub needs_hook_version: bool,
    /// Index of the argument that carries the MCP override, filled in after
    /// discovery. Codex only.
    pub mcp_index: Option<usize>,
}
