//! What `delega doctor` learns about one provider without starting a worker.
//!
//! [`check`] resolves a read-only request for the provider in the current
//! directory with the planning code a run uses, so the executable, the
//! environment and the deny rules are the ones a run would get. The one
//! provider process it starts is the `--version` probe, with the run's timeout
//! and environment. For Claude Code it also looks on the worker's `PATH` for the
//! tools its sandbox needs, without starting them. It reports names and counts,
//! never an environment value, a deny rule or the content of a file.

use std::ffi::OsStr;

use schemars::JsonSchema;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::contract::request::{Provider, Request};
use crate::process::Ending;

use super::deny::{DenyFile, SkippedFile};
use super::limits::{MIN_CLAUDE_HOOK_VERSION, VERSION_PROBE_TIMEOUT};
use super::managed::ManagedSource;
use super::options::ProviderOptions;
use super::plan::{self, Plan};
use super::program;
use super::refusal::Refusal;
use super::run::probe;

/// Doctor's verdicts follow the refusals a run gives: a problem blocks `run`
/// only where a run is refused for it.
///
/// Problem kind: the version probe exited with an error or was ended by a
/// signal. A probe that could not be started is [`PROVIDER_NOT_STARTABLE`].
pub const VERSION_PROBE_FAILED: &str = "version_probe_failed";
/// Problem kind: the executable was found but cannot be started, so every run
/// with it would fail to start (`doctor` uses its own kind here; a run reports
/// `spawn_failed` for Claude and `provider_preparation_failed` for Codex).
pub const PROVIDER_NOT_STARTABLE: &str = "provider_not_startable";
/// Problem kind: the version probe did not answer within its time limit.
pub const VERSION_PROBE_TIMED_OUT: &str = "version_probe_timed_out";
/// Problem kind: Claude Code's sandbox, which `build` runs require, cannot start
/// on this system, because a tool it needs is not on the worker's `PATH` or the
/// system has no sandbox.
pub const SANDBOX_UNAVAILABLE: &str = "sandbox_unavailable";

/// The executables Claude Code's sandbox needs on Linux.
const LINUX_SANDBOX_TOOLS: [&str; 2] = ["bwrap", "socat"];

/// What a problem stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Blocks {
    /// Every run with this provider.
    Run,
    /// Only runs that grant exact commands (`permissions.commands`).
    CommandGrants,
    /// Only runs in the `build` permission mode.
    Build,
    /// Runs still start, but each would report its cleanup as not confirmed.
    Cleanup,
    /// No run is refused or changed. It is a warning: the provider did not
    /// answer `--version`, which a run records only as an absent version.
    Nothing,
}

/// Something that would stop a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Problem {
    /// A stable label: `provider_not_found`, `provider_not_startable`,
    /// `version_probe_failed`, `version_probe_timed_out`,
    /// `unsupported_provider_version`, `sandbox_unavailable`,
    /// `settings_unreadable`, `deny_rules_over_limit`, `managed_mcp_exclusive`,
    /// or another refusal a run would give.
    pub kind: String,
    /// A description. It names files and counts and never repeats an
    /// environment value or a rule.
    pub message: String,
    /// What the problem stops.
    pub blocks: Blocks,
}

impl Problem {
    fn run(kind: &str, message: impl Into<String>) -> Self {
        Self::new(Blocks::Run, kind, message)
    }

    fn new(blocks: Blocks, kind: &str, message: impl Into<String>) -> Self {
        Self {
            kind: kind.to_string(),
            message: message.into(),
            blocks,
        }
    }
}

/// Whether the Claude Code version has the blocking hook interface that exact
/// command grants need.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct CommandHook {
    /// The oldest release that has it.
    pub required_version: String,
    /// Whether the installed version is that release or later, as a run
    /// decides it.
    pub available: bool,
}

/// Whether Claude Code's sandbox, which `build` runs require, can start on this
/// system, as far as doctor can tell without starting it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SandboxCheck {
    /// The system, as Rust names it: `macos`, `linux` or another.
    pub system: String,
    /// Whether the sandbox is expected to start: on macOS always (it uses the
    /// built-in Seatbelt framework), on Linux when every tool in `requires` was
    /// found, elsewhere never.
    pub available: bool,
    /// The executables the sandbox needs on this system: `bwrap` and `socat` on
    /// Linux, none on macOS.
    pub requires: Vec<String>,
    /// Those of `requires` not found on the absolute entries of the worker's
    /// `PATH`.
    pub missing: Vec<String>,
}

impl SandboxCheck {
    /// What doctor reports for `system` (a value of `std::env::consts::OS`)
    /// with `path` as the worker's `PATH`. Looks for files only and starts
    /// nothing.
    pub(crate) fn of(system: &str, path: Option<&OsStr>) -> Self {
        let requires: Vec<String> = match system {
            "linux" => LINUX_SANDBOX_TOOLS
                .iter()
                .map(|t| (*t).to_string())
                .collect(),
            _ => Vec::new(),
        };
        let missing: Vec<String> = requires
            .iter()
            .filter(|tool| program::find_on_path(tool, path).is_none())
            .cloned()
            .collect();
        let available = match system {
            "macos" => true,
            "linux" => missing.is_empty(),
            _ => false,
        };
        Self {
            system: system.to_string(),
            available,
            requires,
            missing,
        }
    }

    /// The problem to report when the sandbox cannot start.
    pub(crate) fn problem(&self) -> Option<Problem> {
        if self.available {
            return None;
        }
        let message = if self.missing.is_empty() {
            format!(
                "Claude Code has no sandbox on this system ({}), so every build run with Claude \
                 would fail at startup",
                self.system
            )
        } else {
            format!(
                "Claude Code's sandbox needs {} on PATH and {} not found, so every build run \
                 with Claude would fail at startup; install {}",
                self.requires.join(" and "),
                if self.missing.len() == 1 {
                    format!("{} was", self.missing[0])
                } else {
                    format!("{} were", self.missing.join(" and "))
                },
                self.missing
                    .iter()
                    .map(|tool| if tool == "bwrap" { "bubblewrap" } else { tool })
                    .collect::<Vec<_>>()
                    .join(" and ")
            )
        };
        Some(Problem::new(Blocks::Build, SANDBOX_UNAVAILABLE, message))
    }
}

/// How many environment variables a worker would receive, by reason. No names
/// and no values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct EnvCounts {
    /// Variables the worker would receive in all.
    pub passed: usize,
    /// Of the caller's: base variables (user, shell, locale and the like).
    pub base: usize,
    /// Of the caller's: proxy and certificate settings.
    pub network: usize,
    /// Of the caller's: variables that belong to the provider.
    pub provider: usize,
    /// Variables delega sets itself for this provider.
    pub set_by_delega: usize,
    /// Variables of the caller's environment that would not be passed.
    pub withheld: usize,
}

/// The deny rules a Claude run would apply, as a run's notes give them: files
/// and counts, never a rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct DenyInfo {
    /// The settings files that exist and were read, with their rule counts.
    pub files: Vec<DenyFile>,
    /// Existing local settings files that would be ignored, with the reason.
    pub skipped: Vec<SkippedFile>,
    /// Rules a worker would receive, after deduplication.
    pub applied: usize,
}

/// What was found for one provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ProviderCheck {
    /// The provider.
    pub provider: Provider,
    /// The executable a run would use, or null when none was found.
    pub executable: Option<String>,
    /// Where the lookup found it: the override variable's name, `config` (the
    /// configuration file), `PATH`, or `option` when a library caller set
    /// `ProviderOptions::executable`.
    pub executable_source: Option<String>,
    /// The first line of `--version` output, or null when there was none.
    pub version: Option<String>,
    /// For Claude Code, whether the command hook interface is available. Null
    /// for Codex, which has no minimum version.
    pub command_hook: Option<CommandHook>,
    /// For Claude Code, whether the sandbox `build` runs require can start.
    /// Null for Codex.
    pub sandbox: Option<SandboxCheck>,
    /// The environment a worker would get, or null when the launch could not be
    /// built.
    pub env: Option<EnvCounts>,
    /// The deny rules a Claude run would apply. Null for Codex, which reads
    /// none, and when the launch could not be built.
    pub deny_rules: Option<DenyInfo>,
    /// The managed (administrator) settings sources found on this system for
    /// the provider, with the names of the keys in them that can change how a
    /// delega launch is honoured. Empty when none was found. They are a
    /// notice: they are not problems and do not change `ready`.
    pub managed_settings: Vec<ManagedSource>,
    /// Whether no problem stops every run with this provider.
    pub ready: bool,
}

fn source(provider: Provider, options: &ProviderOptions) -> String {
    if options.executable.is_some() {
        return "option".to_string();
    }
    let variable = program::override_variable(provider);
    let set = options
        .parent_env()
        .iter()
        .rev()
        .find(|(n, _)| n.to_str() == Some(variable))
        .is_some_and(|(_, v)| !v.is_empty());
    if set {
        variable
    } else if options.configured_executable(provider).is_some() {
        "config"
    } else {
        "PATH"
    }
    .to_string()
}

/// Check one provider in the current directory. Returns what was found and the
/// problems that would stop a run, in the order found. `managed_settings` is
/// left empty: `doctor::diagnose_at` fills it, since it knows where the system
/// locations are looked for.
///
/// Returns the refusal when the current directory cannot be used at all, which
/// is `doctor` itself failing; every other finding is in the returned values.
pub async fn check(
    provider: Provider,
    options: &ProviderOptions,
) -> Result<(ProviderCheck, Vec<Problem>), Refusal> {
    let request = Request::new(provider, "delega doctor");
    let mut refusals = Vec::new();
    let planned = match plan::build_lenient(&request, options, &mut refusals) {
        Ok(plan) => Some(plan),
        Err(refusal) if refusal.kind == "cwd_unusable" => return Err(refusal),
        Err(refusal) => {
            refusals.push(refusal);
            None
        }
    };
    let mut problems: Vec<Problem> = refusals
        .into_iter()
        .map(|r| Problem::run(&r.kind, r.message))
        .collect();
    let mut found = ProviderCheck {
        provider,
        executable: None,
        executable_source: None,
        version: None,
        command_hook: None,
        sandbox: None,
        env: None,
        deny_rules: None,
        managed_settings: Vec::new(),
        ready: false,
    };
    if let Some(plan) = &planned {
        describe(plan, &mut found);
        if !plan.program.as_os_str().is_empty() {
            found.executable = Some(plan.program.display().to_string());
            found.executable_source = Some(source(provider, options));
            probe_version(plan, &mut found, &mut problems).await;
        }
    }
    if provider == Provider::Claude {
        // The tools are looked for on the PATH the worker would get, which is
        // where Claude Code looks for them.
        let parent = options.parent_env();
        let path = match &planned {
            Some(plan) => plan.env.value("PATH"),
            None => parent
                .iter()
                .rev()
                .find(|(n, _)| n.to_str() == Some("PATH"))
                .map(|(_, v)| v.as_os_str()),
        };
        let sandbox = SandboxCheck::of(std::env::consts::OS, path);
        problems.extend(sandbox.problem());
        found.sandbox = Some(sandbox);
    }
    found.ready = !problems.iter().any(|p| p.blocks == Blocks::Run);
    Ok((found, problems))
}

/// The environment and deny rules of the plan, as counts.
fn describe(plan: &Plan, found: &mut ProviderCheck) {
    let report = plan.env.report();
    found.env = Some(EnvCounts {
        passed: plan.env.names().len(),
        base: report.base.len(),
        network: report.network.len(),
        provider: report.provider.len(),
        set_by_delega: report.set_by_delega.len(),
        withheld: report.withheld_count,
    });
    if plan.provider == Provider::Claude {
        found.deny_rules = Some(DenyInfo {
            files: plan.deny.files.clone(),
            skipped: plan.deny.skipped.clone(),
            applied: plan.deny.rules.len(),
        });
    }
}

/// Run `--version` as a run does and judge what it says.
async fn probe_version(plan: &Plan, found: &mut ProviderCheck, problems: &mut Vec<Problem>) {
    let report = probe(
        plan,
        vec!["--version".to_string()],
        VERSION_PROBE_TIMEOUT,
        &CancellationToken::new(),
    )
    .await;
    // A run records an absent version and goes on; only a Claude run that
    // grants exact commands is refused for it.
    let blocks = if plan.provider == Provider::Claude {
        Blocks::CommandGrants
    } else {
        Blocks::Nothing
    };
    let failure = match &report.ending {
        Ending::Exited { code: 0 } => None,
        Ending::Exited { code } => Some(Problem::new(
            blocks,
            VERSION_PROBE_FAILED,
            format!("`--version` exited with code {code}"),
        )),
        Ending::TimedOut { limit } => Some(Problem::new(
            blocks,
            VERSION_PROBE_TIMED_OUT,
            format!(
                "`--version` did not answer within {} s",
                limit.as_secs().max(1)
            ),
        )),
        // The executable passed the lookup but cannot be exec'd, such as a
        // script whose interpreter is missing. A run fails to start it too.
        Ending::FailedToStart { .. } => Some(Problem::run(
            PROVIDER_NOT_STARTABLE,
            "the executable was found but could not be started",
        )),
        _ => Some(Problem::new(
            blocks,
            VERSION_PROBE_FAILED,
            "`--version` ended abnormally",
        )),
    };
    let probed = failure.is_none();
    if let Some(problem) = failure {
        problems.push(problem);
    } else {
        found.version = program::version_text(&report.stdout.text());
    }
    if plan.provider != Provider::Claude {
        return;
    }
    let (a, b, c) = MIN_CLAUDE_HOOK_VERSION;
    let available = found
        .version
        .as_deref()
        .and_then(program::parse_version)
        .is_some_and(|v| v >= MIN_CLAUDE_HOOK_VERSION);
    found.command_hook = Some(CommandHook {
        required_version: format!("{a}.{b}.{c}"),
        available,
    });
    // A failed probe is already reported, with the same effect; a version that
    // answered but is too old or unreadable stops only exact command grants.
    if probed && !available {
        problems.push(Problem {
            kind: "unsupported_provider_version".to_string(),
            message: format!(
                "exact command grants need Claude Code {a}.{b}.{c} or later for the blocking hook \
                 interface, and the installed version could not be confirmed to be that"
            ),
            blocks: Blocks::CommandGrants,
        });
    }
}
