//! From a finished provider process and its parsed stream to the one result.

use jsonschema::Validator;
use serde_json::Value;

use crate::contract::request::Provider;
use crate::contract::result::{Cleanup, ErrorDetail, RunResult, Timing};
use crate::process::{Ending, ProcessReport};

use super::facts::Facts;
use super::limits::{STDERR_EXCERPT_CHARS, STDERR_SCAN_BYTES, VALIDATION_MESSAGE_CHARS};
use super::redact::Redactor;
use super::refusal::clip;
use super::stream::{Parsed, Terminal};
use super::structured;

/// What settling needs besides the report and the parsed stream.
pub(super) struct Settle<'a> {
    pub facts: &'a Facts,
    pub validator: Option<&'a Validator>,
    pub ephemeral: bool,
    /// The session the request asked to continue, if any.
    pub resume: Option<&'a str>,
    pub notes: Vec<String>,
    pub max_output_bytes: usize,
    pub redactor: &'a Redactor,
}

/// The start of Claude Code's error text when the session to resume does not
/// exist (seen with Claude Code 2.1.329).
const CLAUDE_UNKNOWN_SESSION: &str = "No conversation found with session ID";

/// What Codex writes to standard error when the thread to resume does not exist
/// (seen with Codex 0.170.0).
const CODEX_UNKNOWN_SESSION: &str = "no rollout found for thread id";

/// The requested and the reported session, when a resumed run reported a
/// session other than the one it asked for.
fn mismatch<'a>(s: &Settle<'a>, parsed: &'a Parsed) -> Option<(&'a str, &'a str)> {
    if s.ephemeral {
        return None;
    }
    match (s.resume, parsed.session_id.as_deref()) {
        (Some(asked), Some(reported)) if asked != reported => Some((asked, reported)),
        _ => None,
    }
}

/// Whether a resumed run failed because the provider could not find the
/// session it was asked to continue.
fn session_not_found(
    provider: Provider,
    result: &RunResult,
    report: &ProcessReport,
    parsed: &Parsed,
) -> bool {
    let Some(kind) = result.error.as_ref().map(|e| e.kind.as_str()) else {
        return false;
    };
    match (provider, &parsed.terminal) {
        (Provider::Claude, Terminal::Failure { message, .. }) => {
            kind == "provider_error" && message.starts_with(CLAUDE_UNKNOWN_SESSION)
        }
        (Provider::Codex, Terminal::None) => {
            let bytes = &report.stderr.bytes;
            let tail = &bytes[bytes.len().saturating_sub(STDERR_SCAN_BYTES)..];
            kind == "provider_exit" && String::from_utf8_lossy(tail).contains(CODEX_UNKNOWN_SESSION)
        }
        _ => false,
    }
}

fn error(kind: &str, message: impl Into<String>, code: Option<i32>) -> ErrorDetail {
    ErrorDetail {
        kind: kind.to_string(),
        message: message.into(),
        provider_exit_code: code,
    }
}

/// The last part of the provider's standard error as one line, with secrets
/// removed. Only the final [`STDERR_SCAN_BYTES`] are read.
fn stderr_excerpt(report: &ProcessReport, redactor: &Redactor) -> Option<String> {
    let bytes = &report.stderr.bytes;
    let tail = &bytes[bytes.len().saturating_sub(STDERR_SCAN_BYTES)..];
    let text = redactor.apply(&String::from_utf8_lossy(tail));
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.is_empty() {
        return None;
    }
    let count = one_line.chars().count();
    Some(if count > STDERR_EXCERPT_CHARS {
        let tail: String = one_line
            .chars()
            .skip(count - STDERR_EXCERPT_CHARS)
            .collect();
        format!("...{tail}")
    } else {
        one_line
    })
}

fn timing(facts: &Facts, report: &ProcessReport) -> Timing {
    Timing {
        queued_at: crate::process::rfc3339_utc(facts.queued_at),
        started_at: report.timing.started_at_rfc3339(),
        finished_at: report.timing.finished_at_rfc3339(),
        duration_ms: report.timing.duration_ms(),
        timeout_secs: facts.timeout_secs,
    }
}

/// Build the result for a finished provider process.
pub(super) fn settle(
    mut s: Settle<'_>,
    report: &ProcessReport,
    parsed: &Parsed,
    cleanup: Cleanup,
) -> RunResult {
    let mut notes = std::mem::take(&mut s.notes);
    if parsed.stats.unparseable > 0 {
        notes.push(format!(
            "{} provider output lines were not JSON objects and were ignored",
            parsed.stats.unparseable
        ));
    }
    if parsed.stats.oversize > 0 {
        notes.push(format!(
            "{} provider output lines were over the line limit and were skipped",
            parsed.stats.oversize
        ));
    }
    if parsed.stats.replaced > 0 {
        notes.push(format!(
            "{} provider output lines held invalid UTF-8 and were decoded with replacement \
             characters",
            parsed.stats.replaced
        ));
    }
    notes.extend(parsed.notes.iter().cloned());
    if parsed.denied_omitted > 0 {
        notes.push(format!(
            "{} further denied requests were not listed",
            parsed.denied_omitted
        ));
    }
    if report.stdout.truncated {
        notes.push(format!(
            "provider output went over {} bytes and the end was discarded",
            s.max_output_bytes
        ));
    }
    let ctx = s
        .facts
        .run_context(cleanup, timing(s.facts, report), parsed.denied.clone());
    let partial = parsed.partial_text();
    let had_result = matches!(parsed.terminal, Terminal::Success);

    let mut result = match &report.ending {
        Ending::TimedOut { .. } => {
            if had_result {
                notes.push("a final result had been received before the time limit".to_string());
            }
            RunResult::timed_out(
                ctx,
                format!(
                    "the provider was still running after the {} s time limit and was stopped",
                    s.facts.timeout_secs
                ),
                partial,
            )
        }
        Ending::Interrupted => {
            if had_result {
                notes.push("a final result had been received before the stop".to_string());
            }
            RunResult::interrupted(
                ctx,
                "the provider was stopped at the caller's request",
                partial,
            )
        }
        Ending::Exited { code } => {
            let mut result = exited(ctx, *code, report, parsed, &s, &mut notes);
            let not_found =
                s.resume.is_some() && session_not_found(s.facts.provider, &result, report, parsed);
            if let Some(error) = result.error.as_mut() {
                error.provider_exit_code.get_or_insert(*code);
                if not_found {
                    error.kind = "session_not_found".to_string();
                }
            }
            result
        }
        other => {
            let detail = other
                .error_detail()
                .unwrap_or_else(|| error("process_ended", "the process ended", None));
            RunResult::failed(ctx, detail, partial)
        }
    };
    if let Some((asked, reported)) = mismatch(&s, parsed) {
        let kind = result.error.as_ref().map(|e| e.kind.as_str());
        if kind != Some("session_mismatch") {
            notes.push(format!(
                "the run asked to resume session {asked} but the provider reported session \
                 {reported}"
            ));
        }
    }
    result.provider_notes = notes;
    result.applied = s.facts.result_applied();
    result.usage = parsed.usage.clone();
    if !s.ephemeral {
        result.session_id = parsed.session_id.clone();
    }
    result
}

fn exited(
    ctx: crate::contract::result::RunContext,
    code: i32,
    report: &ProcessReport,
    parsed: &Parsed,
    s: &Settle<'_>,
    notes: &mut Vec<String>,
) -> RunResult {
    let partial = parsed.partial_text();
    match &parsed.terminal {
        // A run that continued another session than the one asked for did not
        // do what was asked, whatever it answered.
        Terminal::Success if code == 0 => match mismatch(s, parsed) {
            Some((asked, reported)) => RunResult::failed(
                ctx,
                error(
                    "session_mismatch",
                    format!(
                        "the run asked to resume session {asked} but the provider reported \
                         session {reported}"
                    ),
                    Some(code),
                ),
                partial,
            ),
            None => completed(ctx, parsed, s.validator, notes),
        },
        Terminal::Success => RunResult::failed(
            ctx,
            error(
                "provider_exit",
                format!("the provider reported success but exited with code {code}"),
                Some(code),
            ),
            partial,
        ),
        // The run stopped because of a denial only when the provider reported
        // a failure that is not a named limit and nothing else went on or went
        // wrong after the denial (see `Parsed::stopped_by_denial`).
        Terminal::Failure {
            message,
            code: None,
        } if parsed.stopped_by_denial => RunResult::refused_after_start(
            ctx,
            "permission_denied",
            format!(
                "the provider stopped after {} denied request(s) it was not granted: {}",
                parsed.denied.len(),
                s.redactor.apply(message)
            ),
            partial,
        ),
        Terminal::Failure {
            message,
            code: kind,
        } => RunResult::failed(
            ctx,
            error(
                kind.unwrap_or("provider_error"),
                s.redactor.apply(message),
                Some(code),
            ),
            partial,
        ),
        Terminal::None => {
            let detail = no_terminal(code, report, parsed, s.redactor);
            RunResult::failed(ctx, detail, partial)
        }
    }
}

fn no_terminal(
    code: i32,
    report: &ProcessReport,
    parsed: &Parsed,
    redactor: &Redactor,
) -> ErrorDetail {
    if let Some(last) = parsed.errors.last() {
        return error(
            "provider_error",
            format!(
                "{} (the provider exited with code {code} without a final result)",
                redactor.apply(last)
            ),
            Some(code),
        );
    }
    if code != 0 {
        let mut message = format!("the provider exited with code {code} without a final result");
        if let Some(excerpt) = stderr_excerpt(report, redactor) {
            message.push_str(&format!("; its standard error ended with: {excerpt}"));
        }
        return error("provider_exit", message, Some(code));
    }
    if parsed.stats.unparseable > 0 || parsed.stats.oversize > 0 {
        return error(
            "malformed_output",
            "the provider output could not be read as events and held no final result",
            Some(code),
        );
    }
    error(
        "no_final_message",
        "the provider exited without a final result",
        Some(code),
    )
}

fn completed(
    ctx: crate::contract::result::RunContext,
    parsed: &Parsed,
    validator: Option<&Validator>,
    notes: &mut Vec<String>,
) -> RunResult {
    let text = match (&parsed.final_text, &parsed.last_text) {
        (Some(text), _) => text.clone(),
        (None, Some(last)) => {
            notes.push(
                "the provider's final result carried no text, so the last assistant message is \
                 used as the answer"
                    .to_string(),
            );
            last.clone()
        }
        (None, None) => {
            return RunResult::failed(
                ctx,
                error(
                    "no_final_message",
                    "the provider reported success but gave no answer text",
                    Some(0),
                ),
                String::new(),
            );
        }
    };
    let Some(validator) = validator else {
        return RunResult::completed(ctx, text);
    };
    match structured::extract(&text) {
        Err(message) => RunResult::failed(ctx, error("answer_not_json", message, Some(0)), text),
        Ok(Value::Null) => {
            notes.push(
                "the answer is JSON null, which the result cannot carry in structured; it is in \
                 text only"
                    .to_string(),
            );
            RunResult::completed(ctx, text)
        }
        Ok(value) => {
            let problem = validator
                .validate(&value)
                .err()
                .map(|why| clip(&why.to_string(), VALIDATION_MESSAGE_CHARS));
            if let Some(why) = &problem {
                notes.push(format!(
                    "the structured answer does not satisfy answer_schema: {why}"
                ));
            }
            let mut result = RunResult::completed(ctx, text);
            result.structured = Some(crate::canonical::sorted(value));
            result.structured_valid = Some(problem.is_none());
            result
        }
    }
}
