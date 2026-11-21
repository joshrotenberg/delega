//! From a finished provider process and its parsed stream to the one result.

use jsonschema::Validator;
use serde_json::Value;

use crate::contract::outcome::Outcome;
use crate::contract::request::Provider;
use crate::contract::result::{Cleanup, ErrorDetail, RunResult, Timing};
use crate::process::{Ending, ProcessReport};

use super::facts::Facts;
use super::limits::{
    PROVIDER_MESSAGE_CHARS, STDERR_EXCERPT_CHARS, STDERR_SCAN_BYTES, VALIDATION_MESSAGE_CHARS,
};
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

/// The requested and the first other reported session, when a resumed run's
/// stream reported any session other than the one it asked for.
fn mismatch<'a>(s: &Settle<'a>, parsed: &'a Parsed) -> Option<(&'a str, &'a str)> {
    if s.ephemeral {
        return None;
    }
    let asked = s.resume?;
    let reported = parsed.session_ids.iter().find(|id| *id != asked)?;
    Some((asked, reported.as_str()))
}

/// The text of a mismatch, with secrets removed from both ids.
fn mismatch_text(redactor: &Redactor, asked: &str, reported: &str) -> String {
    redactor.apply(&format!(
        "the run asked to resume session {asked} but the provider reported session {reported}"
    ))
}

/// Whether a resumed run failed because the provider could not find the
/// session it was asked to continue.
fn session_not_found(
    provider: Provider,
    code: i32,
    result: &RunResult,
    report: &ProcessReport,
    parsed: &Parsed,
) -> bool {
    let Some(kind) = result.error.as_ref().map(|e| e.kind.as_str()) else {
        return false;
    };
    match (provider, &parsed.terminal) {
        // The failed result may also carry status 404 and `not_found_error`,
        // which alone would make it `provider_model_unavailable`.
        (Provider::Claude, Terminal::Failure { message, .. }) => {
            (kind == "provider_error" || CLASSIFIED.contains(&kind))
                && message.starts_with(CLAUDE_UNKNOWN_SESSION)
        }
        // Codex 0.170.0 exits 1 before it starts a thread, so nothing is on
        // standard output. A JSON `error` event before the exit makes the kind
        // `provider_error`, or a kind the event's text names. All of the captured standard error is searched; the
        // capture is bounded. Standard output cut at the capture limit may hide
        // a thread or a terminal event, so it does not match.
        (Provider::Codex, Terminal::None)
            if code == 1 && parsed.session_ids.is_empty() && !report.stdout.truncated =>
        {
            (matches!(kind, "provider_exit" | "provider_error") || CLASSIFIED.contains(&kind))
                && String::from_utf8_lossy(&report.stderr.bytes).contains(CODEX_UNKNOWN_SESSION)
        }
        _ => false,
    }
}

/// The kinds a provider failure is given by what it says (see the provider
/// parsers). A Codex `error` event before the exit can carry one of them, and
/// `session_not_found` still outranks it.
const CLASSIFIED: [&str; 6] = [
    "provider_usage_limit",
    "provider_auth",
    "provider_billing",
    "provider_capacity",
    "provider_model_unavailable",
    "provider_context_exhausted",
];

/// A provider's message as it goes into a result: secrets redacted from the
/// whole text first, then clipped, so a credential across the clip point is
/// still removed whole.
fn provider_text(redactor: &Redactor, message: &str) -> String {
    clip(&redactor.apply(message), PROVIDER_MESSAGE_CHARS)
}

fn error(kind: &str, message: impl Into<String>, code: Option<i32>) -> ErrorDetail {
    ErrorDetail {
        kind: kind.to_string(),
        message: message.into(),
        provider_exit_code: code,
    }
}

/// The last part of the provider's standard error as one line, with secrets
/// removed. All of the captured standard error is decoded and redacted first;
/// only then is the excerpt cut from it: its last [`STDERR_SCAN_BYTES`],
/// joined to one line, and its last [`STDERR_EXCERPT_CHARS`] characters, so
/// these cuts cannot split a secret. A 300-character cut is marked by a
/// leading `... ` (with a space, so it never joins a word), and the finished
/// excerpt is redacted once more, in case a cut left a newly
/// credential-shaped string.
fn stderr_excerpt(report: &ProcessReport, redactor: &Redactor) -> Option<String> {
    let decoded = String::from_utf8_lossy(&report.stderr.bytes);
    let redacted = redactor.apply(&decoded);
    let mut start = redacted.len().saturating_sub(STDERR_SCAN_BYTES);
    while !redacted.is_char_boundary(start) {
        start += 1;
    }
    let text = &redacted[start..];
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.is_empty() {
        return None;
    }
    let count = one_line.chars().count();
    let cut = count > STDERR_EXCERPT_CHARS;
    let kept: String = if cut {
        one_line
            .chars()
            .skip(count - STDERR_EXCERPT_CHARS)
            .collect()
    } else {
        one_line
    };
    // The marker for the 300-character cut is followed by a space, so it
    // never joins a word. Either cut can turn the end of a word the first
    // pass did not match (such as `xsk-ant-...` losing its `x`) into a
    // credential shape, so the finished excerpt is redacted once more.
    let excerpt = if cut { format!("... {kept}") } else { kept };
    Some(redactor.apply(&excerpt))
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
            let not_found = s.resume.is_some()
                && session_not_found(s.facts.provider, *code, &result, report, parsed);
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
            notes.push(mismatch_text(s.redactor, asked, reported));
        }
    } else if s.resume.is_none() && !s.ephemeral && parsed.session_ids.len() > 1 {
        let mut note = format!(
            "the provider reported more than one session in this run: {}",
            parsed.session_ids.join(", ")
        );
        let more = parsed.session_ids_omitted.len();
        if more > 0 {
            note.push_str(&format!(", and {more} more that are not listed"));
        }
        notes.push(s.redactor.apply(&note));
    }
    result.provider_notes = notes;
    result.applied = s.facts.result_applied();
    // A resumed run's stream may give only session totals, which are not the
    // run's own; the supervisor works a follow-up's out from the worker's
    // previous totals.
    result.usage = if s.resume.is_some() {
        parsed.usage.clone()
    } else {
        parsed.fresh_usage.clone()
    };
    result.session_usage = parsed.session_usage.clone();
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
        // A run that would have completed but continued another session than
        // the one asked for did not do what was asked. Any other outcome of
        // completing (such as `answer_not_json`) stands, and `settle` adds a
        // note about the mismatch.
        Terminal::Success if code == 0 => {
            let done = completed(ctx.clone(), parsed, s.validator, notes);
            match mismatch(s, parsed) {
                Some((asked, reported)) if done.outcome == Outcome::Completed => RunResult::failed(
                    ctx,
                    error(
                        "session_mismatch",
                        mismatch_text(s.redactor, asked, reported),
                        Some(code),
                    ),
                    done.text,
                ),
                _ => done,
            }
        }
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
                provider_text(s.redactor, message)
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
                provider_text(s.redactor, message),
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
    if let Some(last) = &parsed.last_error {
        return error(
            parsed.error_code.unwrap_or("provider_error"),
            format!(
                "{} (the provider exited with code {code} without a final result)",
                provider_text(redactor, last)
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
