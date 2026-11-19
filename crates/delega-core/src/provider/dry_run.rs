//! What a run would start, without starting it.

use serde::Serialize;

use crate::contract::request::{Provider, Request};

use super::codex::MCP_PLACEHOLDER;
use super::env::EnvReport;
use super::limits::MIN_CLAUDE_HOOK_VERSION;
use super::options::ProviderOptions;
use super::permissions::Mapping;
use super::plan;
use super::refusal::Refusal;

/// What a run would start. It holds environment variable names and never
/// values, and the assignment text is not in it: the prompt goes to the
/// provider on standard input and `stdin_bytes` gives its size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DryRun {
    /// The provider.
    pub provider: Provider,
    /// The executable that would run.
    pub program: String,
    /// The launch arguments. Instruction text is replaced by its size.
    pub args: Vec<String>,
    /// The names of the environment variables the provider would receive,
    /// sorted.
    pub env_names: Vec<String>,
    /// The same names by reason (base, network, provider, named by the
    /// caller, set), the names the caller asked for that are not set, and the
    /// count of the caller's variables that are withheld. Names of withheld
    /// variables are not reported.
    pub env: EnvReport,
    /// The working directory, absolute.
    pub cwd: String,
    /// The time limit in seconds.
    pub time_limit_secs: u64,
    /// The size of standard input in bytes.
    pub stdin_bytes: usize,
    /// How the permissions map onto the provider.
    pub mapping: Mapping,
    /// Where the provider would not do what the request asks. The result of
    /// a real run carries the same notes.
    pub notes: Vec<String>,
    /// What happens before the provider starts.
    pub before_start: Vec<String>,
}

/// Validate a request and report what a run would start.
///
/// Nothing is started, not even a version probe. A request that would be
/// refused before start returns the same [`Refusal`] a run would.
pub fn dry_run(request: &Request, options: &ProviderOptions) -> Result<DryRun, Refusal> {
    let plan = plan::build(request, options)?;
    let mut args = plan.args.clone();
    for elided in &plan.elide {
        let size = args[elided.index].len() - elided.prefix.len().min(args[elided.index].len());
        args[elided.index] = format!("{}<{size} bytes elided>", elided.prefix);
    }
    if let Some(index) = plan.mcp_index {
        args[index] = MCP_PLACEHOLDER.to_string();
    }
    let program = plan.program.display().to_string();
    let mut before_start = vec![format!(
        "{program} --version, to record the provider version"
    )];
    if plan.needs_hook_version {
        let (a, b, c) = MIN_CLAUDE_HOOK_VERSION;
        before_start.push(format!(
            "the run is refused unless that version is {a}.{b}.{c} or later, because the exact \
             command guard needs it"
        ));
    }
    if plan.mcp_index.is_some() {
        before_start.push(format!(
            "{program} mcp list --json, to disable every MCP server Codex lists"
        ));
    }
    Ok(DryRun {
        provider: plan.provider,
        program,
        args,
        env_names: plan.env.names(),
        env: plan.env.report().clone(),
        cwd: plan.scope.cwd.display().to_string(),
        time_limit_secs: plan.time_limit.as_secs(),
        stdin_bytes: plan.stdin.len(),
        mapping: plan.mapping,
        notes: plan.notes,
        before_start,
    })
}
