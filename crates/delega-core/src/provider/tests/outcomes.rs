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
