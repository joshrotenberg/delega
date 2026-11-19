//! What a run would start, without starting it.

use serde::Serialize;

use crate::contract::request::{Provider, Request};

use super::codex::MCP_PLACEHOLDER;
use super::deny::DenyFile;
use super::env::EnvReport;
use super::limits::{DRY_RUN_DENY_RULE_CHARS, DRY_RUN_DENY_RULES_SHOWN, MIN_CLAUDE_HOOK_VERSION};
use super::options::ProviderOptions;
use super::permissions::Mapping;
use super::plan;
use super::refusal::{Refusal, clip};

/// The deny rules a Claude run would take from the user's and the project's
/// settings files. Nothing else from those files is in a dry run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DenyReport {
    /// The settings files that exist and were read, with their rule counts.
    pub files: Vec<DenyFile>,
    /// How many rules would be passed after deduplication and adding absolute forms.
    pub applied: usize,
    /// The rules themselves: the user's own permission patterns, as they would
    /// be passed, at most [`DRY_RUN_DENY_RULES_SHOWN`] of them and each at most
    /// [`DRY_RUN_DENY_RULE_CHARS`] characters. They are the only settings text
    /// in a dry run; the launch arguments show a count in their place.
    pub user_deny_rules: Vec<String>,
    /// How many applied rules are not listed in `user_deny_rules`.
    pub omitted: usize,
}

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
    /// The user's deny rules a Claude run would apply. Empty for Codex, which
    /// reads none.
    pub deny_rules: DenyReport,
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
    // What is reported is read back from the argument that would be passed, so
    // the report cannot differ from the launch.
    let passed = take_deny_rules(&mut args);
    let deny_rules = DenyReport {
        files: plan.deny.files.clone(),
        applied: passed.len(),
        user_deny_rules: passed
            .iter()
            .take(DRY_RUN_DENY_RULES_SHOWN)
            .map(|rule| clip(rule, DRY_RUN_DENY_RULE_CHARS))
            .collect(),
        omitted: passed.len().saturating_sub(DRY_RUN_DENY_RULES_SHOWN),
    };
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
        deny_rules,
        notes: plan.notes,
        before_start,
    })
}

/// Take the deny rules out of the `--settings` argument and leave their count in
/// their place, so the rules appear in a dry run only under their own heading.
fn take_deny_rules(args: &mut [String]) -> Vec<String> {
    let Some(at) = args.iter().position(|a| a == "--settings") else {
        return Vec::new();
    };
    let Some(arg) = args.get_mut(at + 1) else {
        return Vec::new();
    };
    let Ok(mut settings) = serde_json::from_str::<serde_json::Value>(arg) else {
        return Vec::new();
    };
    let rules: Vec<String> = settings
        .pointer("/permissions/deny")
        .and_then(serde_json::Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if !rules.is_empty() {
        settings["permissions"] = serde_json::json!({
            "deny": [format!("<{} deny rules, listed under deny_rules>", rules.len())]
        });
        *arg = settings.to_string();
    }
    rules
}
