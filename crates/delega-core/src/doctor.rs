//! `delega doctor`: what would stop a run, found without starting a worker.
//!
//! [`diagnose`] checks both providers in the current directory
//! ([`provider::doctor`]), runs one process listing the way cleanup does, and
//! reads the limits from the constants the code uses. The result is a
//! [`Report`], which prints as text ([`Report::to_text`]) or as the JSON
//! document the committed schema `schemas/doctor.schema.json` describes.
//!
//! A report holds names, counts and paths. It never holds an environment
//! value, a deny rule or the content of a file.
//!
//! To regenerate the schema after changing a type here, run
//! `DELEGA_UPDATE_SCHEMAS=1 cargo test -p delega-core committed_schemas`
//! and commit the changed file. Embedding reads the file at compile time, so
//! rerun the tests once more afterwards.
#![cfg(unix)]

use std::fmt::Write as _;

use schemars::{JsonSchema, Schema};
use serde::Serialize;
use serde_json::{Value, json};

use crate::contract::limits::{DEFAULT_TIMEOUT_SECS, MAX_TIMEOUT_SECS, MIN_TIMEOUT_SECS};
use crate::contract::request::Provider;
use crate::provider::doctor::{self, Blocks, Problem, ProviderCheck};
use crate::provider::limits::{
    MAX_OUTPUT_BYTES, MAX_STDERR_BYTES, MIN_CLAUDE_HOOK_VERSION, VERSION_PROBE_TIMEOUT,
};
use crate::provider::{ProviderOptions, Refusal};
use crate::supervisor::limits as supervisor;

/// The version of the document `delega doctor --output json` prints.
pub const SCHEMA_VERSION: u32 = 1;

/// The committed schema of the document, embedded at build time.
pub const DOCTOR_SCHEMA: &str = include_str!("../../../schemas/doctor.schema.json");

/// Problem kind: process inspection cannot be run, so every cleanup would be
/// reported as not confirmed.
pub const PROCESS_INSPECTION_UNAVAILABLE: &str = "process_inspection_unavailable";

/// Whether the process listing that confirms cleanup works.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ProcessInspection {
    /// Whether one listing, run now as cleanup runs it, gave a usable table.
    pub available: bool,
    /// Why not, when it is not available. Free text; it names no process.
    pub detail: Option<String>,
}

/// The defaults and bounds a single run has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct RunLimits {
    /// The time limit when a request sets none, in seconds.
    pub default_timeout_secs: u64,
    /// The shortest time limit a request may set, in seconds.
    pub min_timeout_secs: u64,
    /// The longest time limit a request may set, in seconds.
    pub max_timeout_secs: u64,
    /// Most bytes of provider standard output kept.
    pub max_output_bytes: usize,
    /// Most bytes of provider standard error kept.
    pub max_stderr_bytes: usize,
    /// How long the `--version` probe may take, in seconds.
    pub version_probe_timeout_secs: u64,
}

/// The limits of the supervisor that serves the MCP tools.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SupervisorLimits {
    /// Live workers at once.
    pub max_live_workers: usize,
    /// Runs running at once.
    pub max_running_runs: usize,
    /// Runs waiting behind one worker's running run.
    pub max_queued_per_worker: usize,
    /// Finished results kept readable.
    pub retained_results: usize,
    /// Estimated bytes of finished results kept readable.
    pub retained_bytes: usize,
    /// How long a finished result stays readable, in seconds.
    pub retention_secs: u64,
    /// The timeout `wait` uses when it gets none, in seconds.
    pub default_wait_secs: u64,
    /// The longest one `wait` blocks, in seconds.
    pub wait_ceiling_secs: u64,
    /// Request checks that may be outstanding at once.
    pub max_outstanding_request_checks: usize,
}

/// The limits in force, from the constants the code uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Limits {
    /// For one run.
    pub run: RunLimits,
    /// For the supervisor.
    pub supervisor: SupervisorLimits,
}

impl Limits {
    /// The limits this build applies.
    pub fn current() -> Self {
        Self {
            run: RunLimits {
                default_timeout_secs: DEFAULT_TIMEOUT_SECS,
                min_timeout_secs: MIN_TIMEOUT_SECS,
                max_timeout_secs: MAX_TIMEOUT_SECS,
                max_output_bytes: MAX_OUTPUT_BYTES,
                max_stderr_bytes: MAX_STDERR_BYTES,
                version_probe_timeout_secs: VERSION_PROBE_TIMEOUT.as_secs(),
            },
            supervisor: SupervisorLimits {
                max_live_workers: supervisor::MAX_LIVE_WORKERS,
                max_running_runs: supervisor::MAX_RUNNING_RUNS,
                max_queued_per_worker: supervisor::MAX_QUEUED_PER_WORKER,
                retained_results: supervisor::RETAINED_RESULTS,
                retained_bytes: supervisor::RETAINED_BYTES,
                retention_secs: supervisor::RETENTION.as_secs(),
                default_wait_secs: supervisor::DEFAULT_WAIT.as_secs(),
                wait_ceiling_secs: supervisor::WAIT_CEILING.as_secs(),
                max_outstanding_request_checks: supervisor::MAX_PENDING_PREFLIGHTS,
            },
        }
    }
}

/// A problem, with the provider it is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ReportedProblem {
    /// The provider it is about, or null when it is about the whole system.
    pub provider: Option<Provider>,
    /// A stable label. See [`Problem::kind`] and
    /// [`PROCESS_INSPECTION_UNAVAILABLE`].
    pub kind: String,
    /// A description.
    pub message: String,
    /// What the problem stops: `run` (every run with the provider),
    /// `command_grants` (only runs that grant exact commands) or `cleanup`
    /// (runs start, but cleanup cannot be confirmed) or `nothing` (a warning: no
    /// run is refused).
    pub blocks: Blocks,
}

/// A packaged skill and its digest, as `delega version --output json` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SkillDigest {
    /// The skill's name.
    pub name: String,
    /// Where an MCP client reads it.
    pub uri: String,
    /// The SHA-256 digest of its text.
    pub digest: String,
}

/// The whole report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Report {
    /// The version of this document: 1.
    pub schema_version: u32,
    /// Whether a run can go ahead: at least one provider is ready and no
    /// problem is about the whole system. The exit status is 0 when it is true
    /// and 1 when it is false.
    pub ready: bool,
    /// The delega version.
    pub version: String,
    /// The working directory the checks used.
    pub cwd: String,
    /// One entry per provider, `claude` then `codex`.
    pub providers: Vec<ProviderCheck>,
    /// Whether cleanup can be confirmed.
    pub process_inspection: ProcessInspection,
    /// The limits in force.
    pub limits: Limits,
    /// Everything that would stop a run, per provider where it is about one.
    pub problems: Vec<ReportedProblem>,
    /// The skills built into this executable.
    pub skills: Vec<SkillDigest>,
}

/// Check both providers and the process layer in the current directory.
///
/// The only provider process started is the `--version` probe of each provider,
/// as a run starts it. An error means `doctor` itself could not run: the
/// current directory is unusable.
pub async fn diagnose(options: &ProviderOptions) -> Result<Report, Refusal> {
    let (claude, codex) = tokio::join!(
        doctor::check(Provider::Claude, options),
        doctor::check(Provider::Codex, options)
    );
    let inspection = crate::process::check_inspection().await;
    let mut providers = Vec::new();
    let mut problems = Vec::new();
    for checked in [claude?, codex?] {
        let (found, found_problems) = checked;
        let provider = found.provider;
        problems.extend(
            found_problems
                .into_iter()
                .map(|p: Problem| ReportedProblem {
                    provider: Some(provider),
                    kind: p.kind,
                    message: p.message,
                    blocks: p.blocks,
                }),
        );
        providers.push(found);
    }
    let process_inspection = match &inspection {
        Ok(()) => ProcessInspection {
            available: true,
            detail: None,
        },
        Err(detail) => ProcessInspection {
            available: false,
            detail: Some(detail.clone()),
        },
    };
    if let Err(detail) = &inspection {
        problems.push(ReportedProblem {
            provider: None,
            kind: PROCESS_INSPECTION_UNAVAILABLE.to_string(),
            message: format!(
                "process inspection failed ({detail}), so every cleanup would be reported as not \
                 confirmed"
            ),
            blocks: Blocks::Cleanup,
        });
    }
    let ready = providers.iter().any(|p| p.ready) && inspection.is_ok();
    Ok(Report {
        schema_version: SCHEMA_VERSION,
        ready,
        version: crate::VERSION.to_string(),
        cwd: std::env::current_dir()
            .map(|d| d.display().to_string())
            .unwrap_or_default(),
        providers,
        process_inspection,
        limits: Limits::current(),
        problems,
        skills: crate::skills::catalog()
            .iter()
            .map(|skill| SkillDigest {
                name: skill.name().to_string(),
                uri: skill.uri(),
                digest: skill.digest().to_string(),
            })
            .collect(),
    })
}

impl Report {
    /// The exit status for this report: 0 when a run can go ahead, 1 when not.
    pub fn exit_status(&self) -> u8 {
        u8::from(!self.ready)
    }

    /// The report as JSON, with every object's keys in alphabetical order
    /// whether or not `serde_json` keeps insertion order in this build.
    pub fn to_json(&self) -> String {
        let value = crate::canonical::to_sorted_value(self).unwrap_or(Value::Null);
        let mut text = serde_json::to_string_pretty(&value).unwrap_or_default();
        text.push('\n');
        text
    }

    /// The report for people.
    pub fn to_text(&self) -> String {
        sanitize(&self.text())
    }

    fn text(&self) -> String {
        let mut out = String::new();
        let verdict = if self.ready {
            "a run can go ahead"
        } else {
            "a run would be stopped"
        };
        let _ = writeln!(out, "delega {}: {verdict}", self.version);
        let _ = writeln!(out, "working directory: {}", self.cwd);
        for found in &self.providers {
            out.push('\n');
            provider_text(&mut out, found);
        }
        out.push('\n');
        match &self.process_inspection.detail {
            None => out.push_str("process inspection: available\n"),
            Some(detail) => {
                let _ = writeln!(out, "process inspection: unavailable ({detail})");
            }
        }
        out.push('\n');
        limits_text(&mut out, &self.limits);
        out.push('\n');
        if self.problems.is_empty() {
            out.push_str("problems: none\n");
        } else {
            out.push_str("problems:\n");
            for problem in &self.problems {
                let who = problem.provider.map_or("system", provider_name);
                let _ = writeln!(
                    out,
                    "  {who}: {} ({}): {}",
                    problem.kind,
                    blocks_text(problem.blocks),
                    problem.message
                );
            }
        }
        out.push_str("\nskills:\n");
        for skill in &self.skills {
            let _ = writeln!(out, "  {} {}", skill.name, skill.digest);
        }
        out
    }
}

/// Show every control character except the line break as `?`. Paths, version
/// lines and messages carry text from the environment and from providers, and an
/// escape sequence must not reach a terminal.
pub fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| if c != '\n' && c.is_control() { '?' } else { c })
        .collect()
}

fn provider_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude => "claude",
        Provider::Codex => "codex",
    }
}

fn blocks_text(blocks: Blocks) -> &'static str {
    match blocks {
        Blocks::Run => "stops every run",
        Blocks::CommandGrants => "stops runs that grant exact commands",
        Blocks::Cleanup => "cleanup would not be confirmed",
        Blocks::Nothing => "warning, no run is refused",
    }
}

fn provider_text(out: &mut String, found: &ProviderCheck) {
    let state = if found.ready { "ready" } else { "not ready" };
    let _ = writeln!(out, "{}: {state}", provider_name(found.provider));
    match (&found.executable, &found.executable_source) {
        (Some(path), Some(source)) => {
            let _ = writeln!(out, "  executable: {path} (from {source})");
        }
        _ => out.push_str("  executable: not found\n"),
    }
    let version = found.version.as_deref().unwrap_or("not reported");
    let _ = writeln!(out, "  version: {version}");
    if let Some(hook) = &found.command_hook {
        let _ = writeln!(
            out,
            "  command hook: {} (needs {} or later)",
            if hook.available {
                "available"
            } else {
                "not available"
            },
            hook.required_version
        );
    }
    if let Some(env) = &found.env {
        let _ = writeln!(
            out,
            "  environment: {} variables would be passed (base {}, network {}, provider {}, set \
             by delega {}); {} of the caller's would be withheld",
            env.passed, env.base, env.network, env.provider, env.set_by_delega, env.withheld
        );
    }
    if let Some(deny) = &found.deny_rules {
        let _ = writeln!(
            out,
            "  deny rules: {} would be applied from {} settings files",
            deny.applied,
            deny.files.len()
        );
        for file in &deny.files {
            let _ = writeln!(out, "    {} ({} rules)", file.path, file.rules);
        }
        for file in &deny.skipped {
            let _ = writeln!(out, "    {} ignored: {}", file.path, file.reason);
        }
    }
}

fn limits_text(out: &mut String, limits: &Limits) {
    let run = &limits.run;
    let sup = &limits.supervisor;
    out.push_str("limits:\n");
    let _ = writeln!(
        out,
        "  run: time limit {} s by default, from {} s to {} s; output kept {} bytes, \
         standard error {} bytes; version probe {} s",
        run.default_timeout_secs,
        run.min_timeout_secs,
        run.max_timeout_secs,
        run.max_output_bytes,
        run.max_stderr_bytes,
        run.version_probe_timeout_secs
    );
    let _ = writeln!(
        out,
        "  supervisor: {} live workers, {} running runs, {} queued per worker; {} results or \
         {} bytes kept for {} s; wait {} s by default, {} s at most; {} outstanding request checks",
        sup.max_live_workers,
        sup.max_running_runs,
        sup.max_queued_per_worker,
        sup.retained_results,
        sup.retained_bytes,
        sup.retention_secs,
        sup.default_wait_secs,
        sup.wait_ceiling_secs,
        sup.max_outstanding_request_checks
    );
    let (a, b, c) = MIN_CLAUDE_HOOK_VERSION;
    let _ = writeln!(
        out,
        "  Claude Code needs {a}.{b}.{c} or later for exact command grants"
    );
}

/// Generate the schema of the document from the types, as pretty JSON ending in
/// a newline.
pub fn generate_schema() -> String {
    let mut schema = schemars::schema_for!(Report);
    match schema.pointer_mut("/properties/schema_version") {
        Some(Value::Object(target)) => {
            target.insert("const".to_string(), json!(SCHEMA_VERSION));
        }
        _ => panic!("schema refinement: no object at /properties/schema_version"),
    }
    let canonical = Schema::try_from(crate::canonical::sorted(schema.clone().to_value()))
        .unwrap_or_else(|_| schema.clone());
    let mut text = serde_json::to_string_pretty(&canonical).unwrap_or_default();
    text.push('\n');
    text
}

#[cfg(test)]
#[path = "doctor/tests.rs"]
mod tests;
