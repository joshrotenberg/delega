//! Printing what a run produced: the answer, the result, a dry run, or a
//! refusal.
//!
//! Standard output carries the answer or the JSON document and nothing else.
//! Everything meant for a person while a command runs (progress, summaries,
//! problems in text mode) goes to standard error. The exit status follows the
//! outcome, whatever was printed.

use std::process::ExitCode;
use std::time::SystemTime;

use delega_core::VERSION;
use delega_core::contract::SCHEMA_VERSION;
use delega_core::contract::outcome::Outcome;
use delega_core::contract::permissions::Mode;
use delega_core::contract::request::Provider;
use delega_core::contract::result::{
    AppliedPermissions, Cleanup, Provenance, RefusalContext, RunResult, Timing,
};
use delega_core::process::rfc3339_utc;
use delega_core::provider::{DryRun, Refusal};
use serde_json::json;

use super::log::Log;
use super::request::{Facts, Failure};
use crate::commands::OutputFormat;
use crate::commands::output::{Written, write};

/// The outcome as the contract spells it.
pub fn outcome_name(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Completed => "completed",
        Outcome::Failed => "failed",
        Outcome::TimedOut => "timed_out",
        Outcome::Interrupted => "interrupted",
        Outcome::Refused => "refused",
    }
}

/// The provider as the contract spells it.
pub fn provider_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude => "claude",
        Provider::Codex => "codex",
    }
}

fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Read => "read",
        Mode::Write => "write",
        Mode::Full => "full",
    }
}

/// A run id for a result of a request that never reached the provider layer.
fn run_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("run-{nanos:x}-{:x}", std::process::id())
}

fn context(facts: &Facts) -> RefusalContext {
    let now = rfc3339_utc(SystemTime::now());
    RefusalContext {
        run: run_id(),
        worker: None,
        provider: facts.provider,
        permissions: AppliedPermissions {
            mode: facts.mode,
            denied: Vec::new(),
        },
        timing: Timing {
            queued_at: now.clone(),
            started_at: None,
            finished_at: now,
            duration_ms: 0,
            timeout_secs: facts.timeout_secs,
        },
        provenance: Provenance {
            delega_version: VERSION.to_string(),
            build: None,
            provider_executable: None,
            provider_version: None,
        },
    }
}

/// A `refused` result for something that went wrong before a run existed.
pub fn refused_result(facts: &Facts, kind: &str, message: &str) -> RunResult {
    RunResult::refused_before_start(context(facts), kind, message)
}

/// Write a document or the answer to standard output. A failure is reported
/// through the log and the exit status accounts for it; nothing here waits on
/// standard error.
fn emit(log: &Log, text: &str) -> bool {
    match write(text.as_bytes()) {
        Written::Done | Written::Closed => true,
        Written::Failed(message) => {
            log.line(format!("delega run: {message}"));
            false
        }
    }
}

/// The status to exit with: the outcome's, unless the result could not be
/// written to standard output and the outcome said success.
fn status(outcome: Outcome, written: bool) -> ExitCode {
    let code = outcome.exit_status();
    if !written && code == 0 {
        ExitCode::from(1)
    } else {
        ExitCode::from(code)
    }
}

fn json_document(value: &impl serde::Serialize) -> String {
    let mut text = serde_json::to_string_pretty(value)
        .unwrap_or_else(|err| format!("{{\"error\":\"cannot serialize: {err}\"}}"));
    text.push('\n');
    text
}

/// Print a refusal found before a run existed, and exit with the refused
/// status. In JSON mode the refused result is the document.
pub fn refuse(format: OutputFormat, failure: &Failure, log: &Log) -> ExitCode {
    match format {
        OutputFormat::Json => {
            let result = RunResult::refused_invalid(context(&failure.facts), &failure.problems)
                .unwrap_or_else(|_| {
                    refused_result(&failure.facts, "invalid_request", "the request is invalid")
                });
            let written = emit(log, &json_document(&result));
            status(Outcome::Refused, written)
        }
        OutputFormat::Text => {
            for problem in &failure.problems {
                log.line(format!("delega run: refused: {problem}"));
            }
            ExitCode::from(Outcome::Refused.exit_status())
        }
    }
}

/// Print a refusal that is not a list of request problems: a dry run refused, or
/// the configuration file that could not be used. In JSON mode the refused result
/// is the document.
pub fn refuse_dry_run(
    format: OutputFormat,
    facts: &Facts,
    refusal: &Refusal,
    log: &Log,
) -> ExitCode {
    match format {
        OutputFormat::Json => {
            let result = refused_result(facts, &refusal.kind, &refusal.message);
            let written = emit(log, &json_document(&result));
            status(Outcome::Refused, written)
        }
        OutputFormat::Text => {
            log.line(format!("delega run: refused: {refusal}"));
            ExitCode::from(Outcome::Refused.exit_status())
        }
    }
}

/// Print what a dry run found and exit 0.
pub fn dry_run(format: OutputFormat, plan: &DryRun, log: &Log) -> ExitCode {
    let text = match format {
        OutputFormat::Json => json_document(&delega_core::canonical::sorted(json!({
            "schema_version": SCHEMA_VERSION,
            "dry_run": plan,
        }))),
        OutputFormat::Text => dry_run_text(plan),
    };
    if emit(log, &text) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn dry_run_text(plan: &DryRun) -> String {
    let mut out = String::from("dry run: nothing was started\n");
    let mut line = |text: String| {
        out.push_str(&text);
        out.push('\n');
    };
    line(format!("provider: {}", provider_name(plan.provider)));
    line(format!("program: {}", plan.program));
    line("arguments:".to_string());
    for arg in &plan.args {
        line(format!("  {arg:?}"));
    }
    line(format!("working directory: {}", plan.cwd));
    line(format!("time limit: {} s", plan.time_limit_secs));
    line(format!("standard input: {} bytes", plan.stdin_bytes));
    line("environment variable names:".to_string());
    for (reason, names) in [
        ("base", &plan.env.base),
        ("network", &plan.env.network),
        ("provider", &plan.env.provider),
        ("named by the caller", &plan.env.named),
        ("set by the caller", &plan.env.set_by_caller),
        ("set by delega", &plan.env.set_by_delega),
    ] {
        line(format!("  {reason}: {}", names.join(", ")));
    }
    if !plan.env.named_not_set.is_empty() {
        line(format!(
            "  named but not set: {}",
            plan.env.named_not_set.join(", ")
        ));
    }
    line(format!(
        "  withheld from the caller's environment: {} (names not shown)",
        plan.env.withheld_count
    ));
    line(format!(
        "permissions: mode {}",
        mode_name(plan.mapping.mode)
    ));
    for command in &plan.mapping.commands {
        line(format!("  allowed command: {command:?}"));
    }
    for dir in &plan.mapping.directories {
        line(format!("  readable directory: {dir}"));
    }
    for setting in &plan.mapping.settings {
        line(format!("  setting: {setting}"));
    }
    line(format!("  on request: {}", plan.mapping.on_request));
    if !plan.deny_rules.files.is_empty() {
        line("Claude Code settings files read for deny rules:".to_string());
        for file in &plan.deny_rules.files {
            line(format!("  {}: {} deny rules", file.path, file.rules));
        }
    }
    for file in &plan.deny_rules.skipped {
        line(format!(
            "Claude Code settings file ignored: {}: {}",
            file.path, file.reason
        ));
    }
    if !plan.deny_rules.user_deny_rules.is_empty() {
        line(format!(
            "user deny rules ({} applied; permission patterns from those files, as they are \
             passed to the worker):",
            plan.deny_rules.applied
        ));
        for rule in &plan.deny_rules.user_deny_rules {
            line(format!("  {rule:?}"));
        }
        if plan.deny_rules.omitted > 0 {
            line(format!(
                "  ... and {} more not shown",
                plan.deny_rules.omitted
            ));
        }
    }
    for step in &plan.before_start {
        line(format!("before start: {step}"));
    }
    for note in &plan.notes {
        line(format!("note: {note}"));
    }
    out
}

/// Print a run's result as the format asks and return the exit status.
pub fn print_result(format: OutputFormat, result: &RunResult, log: &Log) -> ExitCode {
    let written = match format {
        OutputFormat::Json => emit(log, &json_document(result)),
        OutputFormat::Text => {
            summarize(result, log);
            emit(log, &answer_text(result))
        }
    };
    status(result.outcome, written)
}

/// The answer, ending in a newline, when the run completed. Any other outcome
/// prints nothing on standard output: whatever partial text exists is in the
/// result, which `--output json` prints.
fn answer_text(result: &RunResult) -> String {
    if result.outcome != Outcome::Completed || result.text.is_empty() {
        return String::new();
    }
    let mut text = result.text.clone();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// What a person needs beside the answer, on standard error: how a run that did
/// not complete ended, what was denied, and cleanup that could not be confirmed.
fn summarize(result: &RunResult, log: &Log) {
    if let Some(error) = &result.error {
        log.line(format!(
            "delega run: {}: {}: {}",
            outcome_name(result.outcome),
            error.kind,
            error.message
        ));
    }
    for denied in &result.permissions.denied {
        log.line(format!("delega run: denied: {}", denied.request));
    }
    if let Cleanup::Unconfirmed {
        pids,
        process_groups,
        reason,
    } = &result.cleanup
    {
        log.line(format!(
            "delega run: cleanup not confirmed: {reason} (pids {pids:?}, process groups {process_groups:?})"
        ));
    }
}
