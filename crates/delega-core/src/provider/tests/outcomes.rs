//! Every outcome, for each provider, against the fakes.

use std::time::Duration;

use serde_json::json;

use super::{Rig, both, run_checked};
use crate::contract::outcome::Outcome;
use crate::contract::request::Provider;
use crate::contract::result::Cleanup;
use crate::provider::start;

fn schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": { "answer": { "type": "integer" } },
        "required": ["answer"]
    })
}

async fn completed(provider: Provider) {
    let rig = Rig::new();
    let request = rig.request(provider, "success\nPlease summarize.");
    let result = run_checked(request, rig.options(provider)).await;

    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    assert_eq!(result.text, "fake final answer");
    assert_eq!(result.error, None);
    assert_eq!(result.cleanup, Cleanup::Confirmed);
    assert_eq!(result.provider, Some(provider));
    assert!(result.session_id.is_some());
    let usage = result.usage.expect("usage is reported");
    assert_eq!(
        usage.input_tokens,
        Some(if provider == Provider::Claude {
            12
        } else {
            100
        })
    );
    assert_eq!(usage.output_tokens, Some(7));
    assert_eq!(
        usage.cached_tokens,
        Some(if provider == Provider::Claude { 4 } else { 10 })
    );
    assert_eq!(usage.cost_usd.is_some(), provider == Provider::Claude);
    assert_eq!(result.timing.timeout_secs, 600);
    assert!(result.timing.started_at.is_some());
    assert!(
        result
            .provenance
            .provider_executable
            .as_deref()
            .is_some_and(|p| p.ends_with(".sh"))
    );
    assert!(result.provenance.provider_version.is_some());
    assert!(!result.provider_notes.is_empty());
    assert_eq!(
        result.permissions.mode,
        crate::contract::permissions::Mode::Read
    );
    assert!(result.permissions.denied.is_empty());

    // The prompt reached the provider on standard input, and the worker ran in
    // the requested directory.
    assert!(rig.stdin().starts_with("success\nPlease summarize."));
    assert_eq!(rig.cwd_seen(), rig.work.to_string_lossy());
    rig.assert_nothing_left().await;
}
both!(completes_with_the_final_answer, completed);

async fn structured(provider: Provider) {
    for mode in ["structured", "structured-fenced"] {
        let rig = Rig::new();
        let mut request = rig.request(provider, mode);
        request.answer_schema = Some(schema());
        let result = run_checked(request, rig.options(provider)).await;
        assert_eq!(result.outcome, Outcome::Completed, "{mode}: {result:#?}");
        assert_eq!(result.structured, Some(json!({"answer": 42})), "{mode}");
        assert_eq!(result.structured_valid, Some(true), "{mode}");
        // The schema was put in front of the model.
        assert!(rig.stdin().contains("JSON Schema:"), "{mode}");
        rig.assert_nothing_left().await;
    }
}
both!(reports_a_structured_answer, structured);

async fn structured_invalid(provider: Provider) {
    let rig = Rig::new();
    let mut request = rig.request(provider, "structured-invalid");
    request.answer_schema = Some(schema());
    let result = run_checked(request, rig.options(provider)).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    assert_eq!(result.structured, Some(json!({"answer": "no"})));
    assert_eq!(result.structured_valid, Some(false));
    assert!(
        result
            .provider_notes
            .iter()
            .any(|n| n.contains("does not satisfy answer_schema")),
        "{:?}",
        result.provider_notes
    );
    rig.assert_nothing_left().await;
}
both!(
    marks_a_schema_invalid_answer_without_failing,
    structured_invalid
);

async fn structured_not_json(provider: Provider) {
    let rig = Rig::new();
    let mut request = rig.request(provider, "structured-text");
    request.answer_schema = Some(schema());
    let result = run_checked(request, rig.options(provider)).await;
    assert_eq!(result.outcome, Outcome::Failed, "{result:#?}");
    assert_eq!(result.error.expect("error").kind, "answer_not_json");
    assert_eq!(result.text, "this is prose, not JSON");
    assert_eq!(result.structured, None);
    rig.assert_nothing_left().await;
}
both!(
    fails_when_a_structured_answer_is_not_json,
    structured_not_json
);

async fn provider_error(provider: Provider) {
    let rig = Rig::new();
    let result = run_checked(
        rig.request(provider, "provider-error"),
        rig.options(provider),
    )
    .await;
    assert_eq!(result.outcome, Outcome::Failed, "{result:#?}");
    let error = result.error.expect("error");
    assert_eq!(error.kind, "provider_error");
    assert!(
        error.message.contains("Provider rejected the turn"),
        "{}",
        error.message
    );
    assert_eq!(result.cleanup, Cleanup::Confirmed);
    rig.assert_nothing_left().await;
}
both!(fails_on_a_provider_error_event, provider_error);

async fn exit_nonzero(provider: Provider) {
    let rig = Rig::new();
    let result = run_checked(rig.request(provider, "exit-nonzero"), rig.options(provider)).await;
    assert_eq!(result.outcome, Outcome::Failed, "{result:#?}");
    let error = result.error.expect("error");
    assert_eq!(error.kind, "provider_exit");
    assert_eq!(error.provider_exit_code, Some(3));
    assert!(
        error.message.contains("fake provider crashed"),
        "{}",
        error.message
    );
    rig.assert_nothing_left().await;
}
both!(fails_on_a_nonzero_exit_and_quotes_stderr, exit_nonzero);

async fn success_then_nonzero(provider: Provider) {
    let rig = Rig::new();
    let request = rig.request(provider, "success-then-exit-nonzero");
    let result = run_checked(request, rig.options(provider)).await;
    assert_eq!(result.outcome, Outcome::Failed, "{result:#?}");
    let error = result.error.expect("error");
    assert_eq!(error.kind, "provider_exit");
    assert_eq!(error.provider_exit_code, Some(5));
    assert_eq!(result.text, "fake final answer");
    rig.assert_nothing_left().await;
}
both!(
    does_not_complete_when_success_is_followed_by_a_failing_exit,
    success_then_nonzero
);

async fn malformed(provider: Provider) {
    let rig = Rig::new();
    let result = run_checked(rig.request(provider, "malformed"), rig.options(provider)).await;
    assert_eq!(result.outcome, Outcome::Failed, "{result:#?}");
    assert_eq!(result.error.expect("error").kind, "malformed_output");
    assert!(
        result
            .provider_notes
            .iter()
            .any(|n| n.contains("not JSON objects")),
        "{:?}",
        result.provider_notes
    );
    rig.assert_nothing_left().await;
}
both!(fails_on_a_malformed_stream, malformed);

async fn no_final(provider: Provider) {
    let rig = Rig::new();
    let result = run_checked(rig.request(provider, "no-final"), rig.options(provider)).await;
    assert_eq!(result.outcome, Outcome::Failed, "{result:#?}");
    assert_eq!(result.error.expect("error").kind, "no_final_message");
    rig.assert_nothing_left().await;
}
both!(fails_when_the_stream_has_no_final_message, no_final);

async fn denied_but_completed(provider: Provider) {
    let rig = Rig::new();
    let request = rig.request(provider, "denied-completed");
    let result = run_checked(request, rig.options(provider)).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    assert_eq!(result.permissions.denied.len(), 1);
    assert!(
        result.permissions.denied[0]
            .request
            .contains("rm -rf /tmp/never"),
        "{:?}",
        result.permissions.denied
    );
    rig.assert_nothing_left().await;
}
both!(
    lists_a_denial_in_a_run_that_still_completed,
    denied_but_completed
);

async fn denied_and_stopped(provider: Provider) {
    let rig = Rig::new();
    let request = rig.request(provider, "denied-refused");
    let result = run_checked(request, rig.options(provider)).await;
    assert_eq!(result.outcome, Outcome::Refused, "{result:#?}");
    assert_eq!(result.error.expect("error").kind, "permission_denied");
    assert_eq!(result.permissions.denied.len(), 1);
    assert!(
        result.timing.started_at.is_some(),
        "the provider had started"
    );
    assert_eq!(result.cleanup, Cleanup::Confirmed);
    rig.assert_nothing_left().await;
}
both!(
    refuses_after_start_when_a_denial_stops_the_run,
    denied_and_stopped
);

async fn timeout(provider: Provider) {
    let rig = Rig::new();
    let mut request = rig.request(provider, "hang");
    request.timeout_secs = 1;
    let result = run_checked(request, rig.options(provider)).await;
    assert_eq!(result.outcome, Outcome::TimedOut, "{result:#?}");
    assert_eq!(result.error.expect("error").kind, "timed_out");
    assert_eq!(result.timing.timeout_secs, 1);
    assert_eq!(result.cleanup, Cleanup::Confirmed);
    rig.assert_nothing_left().await;
}
both!(times_out_a_hung_provider_and_stops_it, timeout);

async fn interrupt(provider: Provider) {
    let rig = Rig::new();
    let handle = start(rig.request(provider, "hang"), rig.options(provider));
    rig.wait_started().await;
    assert!(!handle.is_finished());
    handle.interrupt();
    let result = handle.wait().await;
    assert_eq!(result.validate(), Ok(()), "{result:#?}");
    assert_eq!(result.outcome, Outcome::Interrupted, "{result:#?}");
    assert_eq!(result.error.expect("error").kind, "interrupted");
    assert_eq!(result.cleanup, Cleanup::Confirmed);
    rig.assert_nothing_left().await;
}
both!(interrupts_a_hung_provider_and_stops_it, interrupt);

async fn interrupter_from_another_task(provider: Provider) {
    let rig = Rig::new();
    let handle = start(rig.request(provider, "hang"), rig.options(provider));
    let interrupter = handle.interrupter();
    rig.wait_started().await;
    tokio::spawn(async move { interrupter.interrupt() });
    let result = handle.wait().await;
    assert_eq!(result.outcome, Outcome::Interrupted, "{result:#?}");
    rig.assert_nothing_left().await;
}
both!(
    interrupts_through_a_cloned_interrupter,
    interrupter_from_another_task
);

async fn dropped_handle(provider: Provider) {
    let rig = Rig::new();
    let handle = start(rig.request(provider, "hang"), rig.options(provider));
    rig.wait_started().await;
    drop(handle);
    // The stop runs in the background; give it time, then check.
    tokio::time::sleep(Duration::from_millis(50)).await;
    rig.assert_nothing_left().await;
}
both!(
    stops_the_provider_when_the_handle_is_dropped,
    dropped_handle
);

async fn large(provider: Provider) {
    let rig = Rig::new();
    let result = run_checked(rig.request(provider, "large"), rig.options(provider)).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    assert_eq!(result.text.len(), 3_000_000);
    rig.assert_nothing_left().await;
}
both!(returns_a_large_answer_whole, large);

async fn over_the_output_bound(provider: Provider) {
    let rig = Rig::new();
    let mut options = rig.options(provider);
    options.max_output_bytes = Some(64 * 1024);
    let result = run_checked(rig.request(provider, "large"), options).await;
    assert_eq!(result.outcome, Outcome::Failed, "{result:#?}");
    assert!(
        result
            .provider_notes
            .iter()
            .any(|n| n.contains("went over 65536 bytes")),
        "{:?}",
        result.provider_notes
    );
    rig.assert_nothing_left().await;
}
both!(
    fails_and_says_so_when_output_exceeds_the_bound,
    over_the_output_bound
);

async fn resume(provider: Provider) {
    let rig = Rig::new();
    let mut request = rig.request(provider, "success");
    request.session.resume = Some("saved-session-9".into());
    let result = run_checked(request, rig.options(provider)).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    assert_eq!(result.session_id.as_deref(), Some("saved-session-9"));
    let argv = rig.argv();
    let at = argv
        .iter()
        .position(|a| {
            a == if provider == Provider::Claude {
                "--resume"
            } else {
                "resume"
            }
        })
        .expect("resume argument");
    assert_eq!(argv[at + 1], "saved-session-9");
    rig.assert_nothing_left().await;
}
both!(resumes_the_given_session, resume);

async fn session_id_reported(provider: Provider) {
    let rig = Rig::new();
    let result = run_checked(rig.request(provider, "success"), rig.options(provider)).await;
    let expected = match provider {
        Provider::Claude => "fake-claude-session-1",
        Provider::Codex => "fake-codex-thread-1",
    };
    assert_eq!(result.session_id.as_deref(), Some(expected));
    rig.assert_nothing_left().await;
}
both!(reports_the_provider_session_id, session_id_reported);

/// Options whose fake reports `reported` as the session of a resumed run.
fn resumed_as(rig: &Rig, provider: Provider, reported: &str) -> crate::provider::ProviderOptions {
    let mut options = rig.options(provider);
    options
        .env_set
        .push(("FAKE_RESUMED_SESSION_ID".into(), reported.into()));
    options
}

async fn session_mismatch(provider: Provider) {
    let rig = Rig::new();
    let mut request = rig.request(provider, "success");
    request.session.resume = Some("asked-session".into());
    let result = run_checked(request, resumed_as(&rig, provider, "other-session")).await;
    assert_eq!(result.outcome, Outcome::Failed, "{result:#?}");
    let error = result.error.expect("error");
    assert_eq!(error.kind, "session_mismatch");
    assert!(
        error.message.contains("asked-session") && error.message.contains("other-session"),
        "{}",
        error.message
    );
    assert_eq!(
        result.text, "fake final answer",
        "the answer is kept as partial text"
    );
    assert_eq!(result.session_id.as_deref(), Some("other-session"));
    rig.assert_nothing_left().await;
}
both!(
    fails_a_resumed_run_that_reports_another_session,
    session_mismatch
);

async fn mismatch_after_timeout(provider: Provider) {
    let rig = Rig::new();
    let mut request = rig.request(provider, "hang");
    request.session.resume = Some("asked-session".into());
    request.timeout_secs = 1;
    let result = run_checked(request, resumed_as(&rig, provider, "other-session")).await;
    assert_eq!(result.outcome, Outcome::TimedOut, "{result:#?}");
    assert_eq!(result.error.expect("error").kind, "timed_out");
    assert!(
        result
            .provider_notes
            .iter()
            .any(|n| n.contains("asked-session") && n.contains("other-session")),
        "{:?}",
        result.provider_notes
    );
    rig.assert_nothing_left().await;
}
both!(
    keeps_a_timeout_and_notes_the_mismatch_of_a_resumed_run,
    mismatch_after_timeout
);

async fn no_reported_session(provider: Provider) {
    let rig = Rig::new();
    let mut request = rig.request(provider, "no-session-id");
    request.session.resume = Some("asked-session".into());
    let result = run_checked(request, rig.options(provider)).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    assert_eq!(result.session_id, None);
    assert!(
        !result
            .provider_notes
            .iter()
            .any(|n| n.contains("asked-session")),
        "{:?}",
        result.provider_notes
    );
    rig.assert_nothing_left().await;
}
both!(
    completes_a_resumed_run_that_reports_no_session,
    no_reported_session
);

async fn start_unaffected(provider: Provider) {
    let rig = Rig::new();
    let request = rig.request(provider, "success");
    let result = run_checked(request, resumed_as(&rig, provider, "other-session")).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    let expected = match provider {
        Provider::Claude => "fake-claude-session-1",
        Provider::Codex => "fake-codex-thread-1",
    };
    assert_eq!(result.session_id.as_deref(), Some(expected));
    rig.assert_nothing_left().await;
}
both!(
    does_not_check_the_session_of_a_run_that_resumes_none,
    start_unaffected
);

async fn mismatch_under_schema(provider: Provider) {
    let rig = Rig::new();
    let mut request = rig.request(provider, "structured-text");
    request.answer_schema = Some(schema());
    request.session.resume = Some("asked-session".into());
    let result = run_checked(request, resumed_as(&rig, provider, "other-session")).await;
    assert_eq!(result.outcome, Outcome::Failed, "{result:#?}");
    assert_eq!(result.error.expect("error").kind, "answer_not_json");
    assert!(
        result
            .provider_notes
            .iter()
            .any(|n| n.contains("asked-session") && n.contains("other-session")),
        "{:?}",
        result.provider_notes
    );
    rig.assert_nothing_left().await;
}
both!(
    keeps_answer_not_json_and_notes_the_mismatch_of_a_resumed_run,
    mismatch_under_schema
);

async fn mismatch_redacted(provider: Provider) {
    let secret = "sk-ant-abcdefghijklmnopqrstuvwx";
    // The error of a run that would have completed, and the note of one that
    // failed another way.
    for mode in ["success", "provider-error"] {
        let rig = Rig::new();
        let mut request = rig.request(provider, mode);
        request.session.resume = Some("asked-session".into());
        let result = run_checked(request, resumed_as(&rig, provider, secret)).await;
        let error = result.error.expect("error");
        let expected = if mode == "success" {
            "session_mismatch"
        } else {
            "provider_error"
        };
        assert_eq!(error.kind, expected, "{mode}");
        assert!(!error.message.contains(secret), "{mode}: {}", error.message);
        assert!(
            result.provider_notes.iter().all(|n| !n.contains(secret)),
            "{mode}: {:?}",
            result.provider_notes
        );
        if mode == "provider-error" {
            assert!(
                result
                    .provider_notes
                    .iter()
                    .any(|n| n.contains("asked-session")),
                "{:?}",
                result.provider_notes
            );
        }
        rig.assert_nothing_left().await;
    }
}
both!(
    redacts_a_credential_shaped_session_id_in_mismatch_text,
    mismatch_redacted
);

#[tokio::test]
async fn codex_keeps_provider_exit_for_a_missing_rollout_with_another_exit_code_or_a_thread() {
    for mode in ["unknown-session-exit-3", "unknown-session-after-thread"] {
        let rig = Rig::new();
        let mut request = rig.request(Provider::Codex, mode);
        request.session.resume = Some("missing-session".into());
        let result = run_checked(request, rig.options(Provider::Codex)).await;
        assert_eq!(result.outcome, Outcome::Failed, "{mode}: {result:#?}");
        assert_eq!(result.error.expect("error").kind, "provider_exit", "{mode}");
        rig.assert_nothing_left().await;
    }
}

#[tokio::test]
async fn claude_fails_a_resumed_run_whose_stream_also_reports_another_session() {
    let rig = Rig::new();
    let mut request = rig.request(Provider::Claude, "two-sessions");
    request.session.resume = Some("asked-session".into());
    let result = run_checked(request, rig.options(Provider::Claude)).await;
    assert_eq!(result.outcome, Outcome::Failed, "{result:#?}");
    let error = result.error.expect("error");
    assert_eq!(error.kind, "session_mismatch");
    assert!(
        error.message.contains("fake-claude-session-2"),
        "{}",
        error.message
    );
    rig.assert_nothing_left().await;
}

async fn stray_after_terminal(provider: Provider) {
    let rig = Rig::new();
    let (mode, stray) = match provider {
        Provider::Claude => ("stray-after-result", "fake-claude-stray"),
        Provider::Codex => ("stray-after-completed", "fake-codex-stray"),
    };
    let mut request = rig.request(provider, mode);
    request.session.resume = Some("asked-session".into());
    let result = run_checked(request, rig.options(provider)).await;
    assert_eq!(result.outcome, Outcome::Failed, "{result:#?}");
    let error = result.error.expect("error");
    assert_eq!(error.kind, "session_mismatch");
    assert!(error.message.contains(stray), "{}", error.message);
    assert_eq!(result.text, "fake final answer");
    rig.assert_nothing_left().await;
}
both!(
    fails_a_resumed_run_that_reports_another_session_after_its_result,
    stray_after_terminal
);

#[tokio::test]
async fn codex_finds_a_missing_rollout_behind_long_stderr_or_after_an_error_event() {
    for mode in ["unknown-session-long-stderr", "unknown-session-error-event"] {
        let rig = Rig::new();
        let mut request = rig.request(Provider::Codex, mode);
        request.session.resume = Some("missing-session".into());
        let result = run_checked(request, rig.options(Provider::Codex)).await;
        assert_eq!(result.outcome, Outcome::Failed, "{mode}: {result:#?}");
        let error = result.error.expect("error");
        assert_eq!(error.kind, "session_not_found", "{mode}: {}", error.message);
        assert_eq!(error.provider_exit_code, Some(1), "{mode}");
        rig.assert_nothing_left().await;
    }
}

#[tokio::test]
async fn codex_keeps_provider_exit_for_a_missing_rollout_when_stdout_was_cut() {
    let rig = Rig::new();
    let mut request = rig.request(Provider::Codex, "unknown-session-large-stdout");
    request.session.resume = Some("missing-session".into());
    let mut options = rig.options(Provider::Codex);
    options.max_output_bytes = Some(64 * 1024);
    let result = run_checked(request, options).await;
    assert_eq!(result.outcome, Outcome::Failed, "{result:#?}");
    assert_eq!(result.error.expect("error").kind, "provider_exit");
    assert!(
        result
            .provider_notes
            .iter()
            .any(|n| n.contains("went over 65536 bytes")),
        "{:?}",
        result.provider_notes
    );
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn claude_says_how_many_session_ids_past_the_bound_were_not_listed() {
    let rig = Rig::new();
    let result = run_checked(
        rig.request(Provider::Claude, "many-sessions"),
        rig.options(Provider::Claude),
    )
    .await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    let note = result
        .provider_notes
        .iter()
        .find(|n| n.contains("more than one session"))
        .expect("a note about the sessions");
    assert!(note.contains("fake-claude-many-8"), "{note}");
    assert!(!note.contains("fake-claude-many-9"), "{note}");
    // Two more of the ten, and the usual id.
    assert!(note.contains("and 3 more that are not listed"), "{note}");
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn claude_takes_no_session_id_from_a_sub_agent_message() {
    let rig = Rig::new();
    let mut request = rig.request(Provider::Claude, "subagent-session");
    request.session.resume = Some("asked-session".into());
    let resumed = run_checked(request, rig.options(Provider::Claude)).await;
    assert_eq!(resumed.outcome, Outcome::Completed, "{resumed:#?}");
    assert_eq!(resumed.session_id.as_deref(), Some("asked-session"));
    rig.assert_nothing_left().await;

    let rig = Rig::new();
    let fresh = run_checked(
        rig.request(Provider::Claude, "subagent-session"),
        rig.options(Provider::Claude),
    )
    .await;
    assert_eq!(fresh.outcome, Outcome::Completed, "{fresh:#?}");
    assert!(
        !fresh
            .provider_notes
            .iter()
            .any(|n| n.contains("fake-claude-subagent")),
        "{:?}",
        fresh.provider_notes
    );
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn claude_notes_a_fresh_run_whose_stream_reports_two_sessions() {
    let rig = Rig::new();
    let result = run_checked(
        rig.request(Provider::Claude, "two-sessions"),
        rig.options(Provider::Claude),
    )
    .await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    assert!(
        result
            .provider_notes
            .iter()
            .any(|n| n.contains("fake-claude-session-1") && n.contains("fake-claude-session-2")),
        "{:?}",
        result.provider_notes
    );
    rig.assert_nothing_left().await;
}

async fn session_not_found(provider: Provider) {
    let rig = Rig::new();
    let mut request = rig.request(provider, "unknown-session");
    request.session.resume = Some("missing-session".into());
    let result = run_checked(request, rig.options(provider)).await;
    assert_eq!(result.outcome, Outcome::Failed, "{result:#?}");
    let error = result.error.expect("error");
    assert_eq!(error.kind, "session_not_found");
    assert_eq!(error.provider_exit_code, Some(1));
    let expected = match provider {
        Provider::Claude => "No conversation found with session ID: missing-session",
        Provider::Codex => "no rollout found for thread id missing-session",
    };
    assert!(error.message.contains(expected), "{}", error.message);
    rig.assert_nothing_left().await;
}
both!(
    names_a_missing_session_to_resume_session_not_found,
    session_not_found
);

async fn unknown_session_without_resume(provider: Provider) {
    let rig = Rig::new();
    let result = run_checked(
        rig.request(provider, "unknown-session"),
        rig.options(provider),
    )
    .await;
    assert_eq!(result.outcome, Outcome::Failed, "{result:#?}");
    let expected = match provider {
        Provider::Claude => "provider_error",
        Provider::Codex => "provider_exit",
    };
    assert_eq!(result.error.expect("error").kind, expected);
    rig.assert_nothing_left().await;
}
both!(
    keeps_the_generic_kind_for_a_missing_session_when_nothing_was_resumed,
    unknown_session_without_resume
);

async fn ephemeral(provider: Provider) {
    let rig = Rig::new();
    let mut request = rig.request(provider, "success");
    request.session.ephemeral = true;
    let result = run_checked(request, rig.options(provider)).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    assert_eq!(
        result.session_id, None,
        "an ephemeral session cannot be resumed"
    );
    let flag = match provider {
        Provider::Claude => "--no-session-persistence",
        Provider::Codex => "--ephemeral",
    };
    assert!(rig.argv().iter().any(|a| a == flag), "{:?}", rig.argv());
    rig.assert_nothing_left().await;
}
both!(
    passes_the_ephemeral_flag_and_drops_the_session_id,
    ephemeral
);

async fn descendant(provider: Provider) {
    let rig = Rig::new();
    let result = run_checked(rig.request(provider, "descendant"), rig.options(provider)).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    assert_eq!(result.cleanup, Cleanup::Confirmed, "{result:#?}");
    assert!(rig.pid("child.pid").is_some(), "the fake started a child");
    rig.assert_nothing_left().await;
}
both!(stops_a_background_child_left_after_success, descendant);

async fn instructions_reach_the_provider(provider: Provider) {
    let rig = Rig::new();
    let mut request = rig.request(provider, "success");
    request.instructions = Some("Prefer small commits.".into());
    request.model = Some("some-model".into());
    let result = run_checked(request, rig.options(provider)).await;
    assert_eq!(result.outcome, Outcome::Completed);
    let argv = rig.argv();
    let joined = argv.join("\n");
    assert!(
        joined.contains("You are a delegated CLI worker"),
        "{joined}"
    );
    assert!(joined.contains("Prefer small commits."), "{joined}");
    let at = argv
        .iter()
        .position(|a| a == "--model")
        .expect("model flag");
    assert_eq!(argv[at + 1], "some-model");
    rig.assert_nothing_left().await;
}
both!(
    sends_instructions_and_model_to_the_provider,
    instructions_reach_the_provider
);

async fn long_instructions(provider: Provider) {
    let rig = Rig::new();
    let mut request = rig.request(provider, "success");
    let long = "x".repeat(crate::provider::limits::ARGUMENT_LIMIT + 10);
    request.instructions = Some(long.clone());
    let result = run_checked(request, rig.options(provider)).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    assert!(
        rig.stdin().contains(&long),
        "instructions moved to standard input"
    );
    assert!(rig.argv().join("\n").len() < crate::provider::limits::ARGUMENT_LIMIT);
    assert!(
        result
            .provider_notes
            .iter()
            .any(|n| n.contains("sent on standard input")),
        "{:?}",
        result.provider_notes
    );
    rig.assert_nothing_left().await;
}
both!(
    moves_instructions_that_do_not_fit_an_argument_to_stdin,
    long_instructions
);

#[tokio::test]
async fn claude_uses_the_last_assistant_text_when_the_result_has_none() {
    let rig = Rig::new();
    let request = rig.request(Provider::Claude, "empty-success");
    let result = run_checked(request, rig.options(Provider::Claude)).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    assert_eq!(result.text, "working");
    assert!(
        result
            .provider_notes
            .iter()
            .any(|n| n.contains("last assistant message"))
    );
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn codex_commentary_is_not_taken_as_the_final_answer() {
    let rig = Rig::new();
    let request = rig.request(Provider::Codex, "empty-success");
    let result = run_checked(request, rig.options(Provider::Codex)).await;
    assert_eq!(result.outcome, Outcome::Failed, "{result:#?}");
    assert_eq!(result.error.expect("error").kind, "no_final_message");
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn codex_error_event_without_a_result_fails_with_its_message() {
    let rig = Rig::new();
    let result = run_checked(
        rig.request(Provider::Codex, "error-event"),
        rig.options(Provider::Codex),
    )
    .await;
    assert_eq!(result.outcome, Outcome::Failed, "{result:#?}");
    let error = result.error.expect("error");
    assert_eq!(error.kind, "provider_error");
    assert!(error.message.contains("Transport unavailable"));
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn codex_recoverable_error_event_does_not_fail_a_completed_run() {
    let rig = Rig::new();
    let request = rig.request(Provider::Codex, "recoverable-error");
    let result = run_checked(request, rig.options(Provider::Codex)).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    rig.assert_nothing_left().await;
}

async fn a_dropped_wait_future_stops_the_run(provider: Provider) {
    let rig = Rig::new();
    let handle = start(rig.request(provider, "hang"), rig.options(provider));
    rig.wait_started().await;
    // Poll the wait future, then drop it before the run has ended.
    let waited = tokio::time::timeout(Duration::from_millis(100), handle.wait()).await;
    assert!(waited.is_err(), "the run must still be going");
    rig.assert_nothing_left().await;
}
both!(
    dropping_the_wait_future_interrupts_the_run,
    a_dropped_wait_future_stops_the_run
);

/// Run one fake mode that ends in a failure and return its error.
async fn failure_of(provider: Provider, mode: &str) -> crate::contract::result::ErrorDetail {
    let rig = Rig::new();
    let result = run_checked(rig.request(provider, mode), rig.options(provider)).await;
    assert_eq!(result.outcome, Outcome::Failed, "{mode}: {result:#?}");
    rig.assert_nothing_left().await;
    result.error.expect("error")
}

#[tokio::test]
async fn claude_unknown_model_is_provider_model_unavailable_with_its_reason() {
    let error = failure_of(Provider::Claude, "model-not-found").await;
    assert_eq!(error.kind, "provider_model_unavailable");
    assert_eq!(error.provider_exit_code, Some(1));
    assert!(
        error
            .message
            .starts_with("There's an issue with the selected model (no-such-model)"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn claude_invalid_api_key_is_provider_auth() {
    let error = failure_of(Provider::Claude, "invalid-api-key").await;
    assert_eq!(error.kind, "provider_auth");
    assert!(
        error.message.contains("Invalid API key"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn claude_account_error_without_a_status_is_provider_auth() {
    let error = failure_of(Provider::Claude, "account-on-hold").await;
    assert_eq!(error.kind, "provider_auth");
}

#[tokio::test]
async fn claude_billing_error_is_provider_billing() {
    let error = failure_of(Provider::Claude, "billing-error").await;
    assert_eq!(error.kind, "provider_billing");
}

#[tokio::test]
async fn claude_overloaded_api_is_provider_capacity() {
    let error = failure_of(Provider::Claude, "overloaded").await;
    assert_eq!(error.kind, "provider_capacity");
    assert_eq!(error.message, "API Error: 529 overloaded");
}

#[tokio::test]
async fn claude_prompt_too_long_is_provider_context_exhausted() {
    let error = failure_of(Provider::Claude, "prompt-too-long").await;
    assert_eq!(error.kind, "provider_context_exhausted");
    assert_eq!(error.message, "Prompt is too long");
}

#[tokio::test]
async fn claude_usage_limit_outranks_its_rate_limit_error_and_429() {
    let error = failure_of(Provider::Claude, "usage-limit").await;
    assert_eq!(error.kind, "provider_usage_limit");
    assert!(
        error.message.starts_with("You've hit your session limit"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn claude_rejected_rate_limit_event_makes_a_429_failure_a_usage_limit() {
    let error = failure_of(Provider::Claude, "usage-limit-event").await;
    assert_eq!(error.kind, "provider_usage_limit");
}

#[tokio::test]
async fn claude_rate_limit_event_covered_by_overage_is_not_a_usage_limit() {
    let error = failure_of(Provider::Claude, "overage-rate-limit-event").await;
    assert_eq!(error.kind, "provider_capacity");
}

#[tokio::test]
async fn claude_rejected_rate_limit_event_does_not_change_a_success() {
    let rig = Rig::new();
    let request = rig.request(Provider::Claude, "rate-limit-event-then-success");
    let result = run_checked(request, rig.options(Provider::Claude)).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    assert_eq!(result.error, None);
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn claude_failed_result_without_errors_reports_its_result_text() {
    let error = failure_of(Provider::Claude, "result-text-error").await;
    assert_eq!(error.kind, "provider_error");
    assert_eq!(error.message, "Something unexpected happened");
}

#[tokio::test]
async fn codex_unsupported_model_is_provider_model_unavailable() {
    let error = failure_of(Provider::Codex, "model-unsupported").await;
    assert_eq!(error.kind, "provider_model_unavailable");
    assert_eq!(error.provider_exit_code, Some(1));
    assert!(
        error
            .message
            .contains("'no-such-model' model is not supported"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn codex_401_is_provider_auth() {
    let error = failure_of(Provider::Codex, "invalid-api-key").await;
    assert_eq!(error.kind, "provider_auth");
    assert!(
        error
            .message
            .starts_with("unexpected status 401 Unauthorized"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn codex_usage_limit_is_provider_usage_limit() {
    for mode in [
        "usage-limit",
        "usage-limit-error-only",
        "usage-limit-info-only",
    ] {
        let error = failure_of(Provider::Codex, mode).await;
        assert_eq!(error.kind, "provider_usage_limit", "{mode}");
    }
}

#[tokio::test]
async fn codex_context_window_is_provider_context_exhausted_not_a_usage_limit() {
    let error = failure_of(Provider::Codex, "context-window").await;
    assert_eq!(error.kind, "provider_context_exhausted");
}

#[tokio::test]
async fn codex_503_at_capacity_is_provider_capacity() {
    let error = failure_of(Provider::Codex, "capacity").await;
    assert_eq!(error.kind, "provider_capacity");
}

async fn unrelated_failure_keeps_its_kind(provider: Provider) {
    for (mode, kind) in [
        ("provider-error", "provider_error"),
        ("exit-nonzero", "provider_exit"),
    ] {
        let error = failure_of(provider, mode).await;
        assert_eq!(error.kind, kind, "{mode}");
    }
}
both!(
    keeps_the_old_kind_of_a_failure_that_names_no_account_or_model_problem,
    unrelated_failure_keeps_its_kind
);

async fn secret_at_clip(provider: Provider) {
    let modes: &[&str] = match provider {
        Provider::Claude => &["secret-at-clip"],
        Provider::Codex => &["secret-at-clip", "secret-at-clip-error-only"],
    };
    for mode in modes {
        let error = failure_of(provider, mode).await;
        // Clipped first, the message would keep "sk-ant-abcdefg", too short for
        // the redactor to recognise.
        assert!(
            !error.message.contains("sk-ant"),
            "{mode}: {}",
            error.message
        );
        assert!(
            error.message.contains("[redacted]"),
            "{mode}: {}",
            error.message
        );
    }
}
both!(
    redacts_a_credential_across_the_message_clip_point,
    secret_at_clip
);

#[tokio::test]
async fn claude_missing_session_with_a_404_not_found_is_session_not_found_not_a_missing_model() {
    let rig = Rig::new();
    let mut request = rig.request(Provider::Claude, "unknown-session-404");
    request.session.resume = Some("missing-session".into());
    let result = run_checked(request, rig.options(Provider::Claude)).await;
    assert_eq!(result.outcome, Outcome::Failed, "{result:#?}");
    let error = result.error.expect("error");
    assert_eq!(error.kind, "session_not_found");
    assert!(
        error
            .message
            .starts_with("No conversation found with session ID: missing-session"),
        "{}",
        error.message
    );
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn claude_stderr_excerpt_drops_a_credential_cut_by_the_scan_window() {
    let error = failure_of(Provider::Claude, "stderr-secret-at-scan").await;
    assert_eq!(error.kind, "provider_exit");
    // The scan window starts inside the key; its last characters must not
    // reach the message.
    assert!(!error.message.contains("uvwx"), "{}", error.message);
    assert!(error.message.ends_with("end"), "{}", error.message);
}

#[tokio::test]
async fn claude_stderr_excerpt_drops_a_bearer_token_cut_by_the_scan_window_and_keeps_later_lines() {
    let error = failure_of(Provider::Claude, "stderr-bearer-at-scan").await;
    assert_eq!(error.kind, "provider_exit");
    // The window opens at the space after "Bearer", so the token stands
    // alone; its whole line is dropped and the next full line stays.
    assert!(
        !error.message.contains("abcdefghijklmnop123"),
        "{}",
        error.message
    );
    assert!(
        error.message.ends_with("complete line survives"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn claude_stderr_excerpt_without_a_newline_drops_only_the_cut_word() {
    let error = failure_of(Provider::Claude, "stderr-secret-at-scan-one-line").await;
    assert_eq!(error.kind, "provider_exit");
    assert!(!error.message.contains("uvwx"), "{}", error.message);
    assert!(error.message.ends_with("end"), "{}", error.message);
}

#[tokio::test]
async fn codex_error_event_with_only_codex_error_info_is_provider_usage_limit() {
    let error = failure_of(Provider::Codex, "usage-limit-info-no-message").await;
    assert_eq!(error.kind, "provider_usage_limit");
    assert!(
        error
            .message
            .starts_with("Codex reported usage_limit_exceeded"),
        "{}",
        error.message
    );
}

async fn denied_refused_secret(provider: Provider) {
    let rig = Rig::new();
    let request = rig.request(provider, "denied-refused-secret");
    let result = run_checked(request, rig.options(provider)).await;
    assert_eq!(result.outcome, Outcome::Refused, "{result:#?}");
    let error = result.error.expect("error");
    assert_eq!(error.kind, "permission_denied");
    // Clipped first, the message would keep "sk-ant-abcdefg".
    assert!(!error.message.contains("sk-ant"), "{}", error.message);
    assert!(error.message.contains("[redacted]"), "{}", error.message);
    rig.assert_nothing_left().await;
}
both!(
    redacts_a_credential_across_the_clip_point_in_a_denial_stop_message,
    denied_refused_secret
);
