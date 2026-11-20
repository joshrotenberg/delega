//! Parsing the providers' output streams.

use crate::provider::stream::{Terminal, for_each_event};
use crate::provider::{claude, codex};

fn lines(items: &[&str]) -> Vec<u8> {
    items.join("\n").into_bytes()
}

#[test]
fn framing_counts_unusable_and_oversize_lines() {
    let mut seen = 0;
    let big = format!(
        "{{\"x\":\"{}\"}}",
        "a".repeat(crate::provider::limits::MAX_LINE_BYTES)
    );
    let input = lines(&[
        "{\"a\":1}",
        "",
        "  ",
        "not json",
        "[1,2]",
        "42",
        &big,
        "{\"b\":2}",
    ]);
    let stats = for_each_event(&input, |_| seen += 1);
    assert_eq!(seen, 2);
    assert_eq!(stats.unparseable, 3);
    assert_eq!(stats.oversize, 1);
}

#[test]
fn claude_result_event_gives_text_usage_cost_session_and_denials() {
    let stream = lines(&[
        r#"{"type":"system","subtype":"init","session_id":"s-1"}"#,
        r#"{"type":"assistant","session_id":"s-1","message":{"content":[{"type":"text","text":"thinking"},{"type":"tool_use","id":"t","name":"Bash"}]}}"#,
        r#"{"type":"assistant","parent_tool_use_id":"x","message":{"content":[{"type":"text","text":"sub-agent"}]}}"#,
        r#"{"type":"result","subtype":"success","is_error":false,"session_id":"s-1","result":"the answer","total_cost_usd":0.25,"usage":{"input_tokens":3,"cache_read_input_tokens":2,"output_tokens":9},"permission_denials":[{"tool_name":"Bash","tool_use_id":"t","tool_input":{"command":"rm -rf x"}},{"tool_name":"Read","tool_input":{"file_path":"/etc/passwd"}},{"tool_name":"Grep"}]}"#,
    ]);
    let parsed = claude::parse(&stream);
    assert_eq!(parsed.session_id.as_deref(), Some("s-1"));
    assert_eq!(parsed.final_text.as_deref(), Some("the answer"));
    assert_eq!(parsed.last_text.as_deref(), Some("thinking"));
    assert_eq!(parsed.terminal, Terminal::Success);
    let usage = parsed.usage.unwrap();
    assert_eq!(
        (usage.input_tokens, usage.output_tokens, usage.cached_tokens),
        (Some(3), Some(9), Some(2))
    );
    assert_eq!(usage.cost_usd, Some(0.25));
    let denied: Vec<&str> = parsed.denied.iter().map(|d| d.request.as_str()).collect();
    assert_eq!(denied, ["Bash: rm -rf x", "Read: /etc/passwd", "Grep"]);
}

#[test]
fn claude_failure_subtypes_and_errors_become_a_failure_message() {
    let with_errors = claude::parse(&lines(&[
        r#"{"type":"result","subtype":"error_max_turns","is_error":true,"errors":["Turn limit reached","second"]}"#,
    ]));
    assert_eq!(
        with_errors.terminal,
        Terminal::Failure {
            message: "Turn limit reached; second".into(),
            code: Some("provider_turn_limit")
        }
    );
    let without = claude::parse(&lines(&[
        r#"{"type":"result","subtype":"error_max_budget_usd","is_error":true}"#,
    ]));
    assert_eq!(
        without.terminal,
        Terminal::Failure {
            message: "Claude Code ended with error_max_budget_usd".into(),
            code: Some("provider_budget_limit")
        }
    );
    let flagged = claude::parse(&lines(&[
        r#"{"type":"result","subtype":"success","is_error":true,"result":"x"}"#,
    ]));
    assert!(matches!(flagged.terminal, Terminal::Failure { .. }));
    assert_eq!(flagged.final_text.as_deref(), Some("x"));
}

#[test]
fn claude_with_no_usable_numbers_reports_no_usage() {
    let parsed = claude::parse(&lines(&[
        r#"{"type":"result","subtype":"success","result":"x","usage":{"service_tier":"standard"}}"#,
    ]));
    assert_eq!(parsed.usage, None);
}

#[test]
fn codex_events_give_thread_final_message_usage_and_declined_commands() {
    let stream = lines(&[
        r#"{"type":"thread.started","thread_id":"th-1"}"#,
        r#"{"type":"item.completed","item":{"type":"agent_message","phase":"commentary","text":"progress"}}"#,
        r#"{"type":"item.started","item":{"id":"c","type":"command_execution","command":"ls","status":"in_progress"}}"#,
        r#"{"type":"item.completed","item":{"id":"c","type":"command_execution","command":"ls","status":"completed","exit_code":0}}"#,
        r#"{"type":"item.completed","item":{"id":"d","type":"command_execution","command":"rm x","status":"declined"}}"#,
        r#"{"type":"item.completed","item":{"type":"agent_message","phase":"final_answer","text":"done"}}"#,
        r#"{"type":"turn.completed","usage":{"input_tokens":5,"cached_input_tokens":1,"output_tokens":2,"reasoning_output_tokens":4}}"#,
    ]);
    let parsed = codex::parse(&stream);
    assert_eq!(parsed.session_id.as_deref(), Some("th-1"));
    assert_eq!(parsed.final_text.as_deref(), Some("done"));
    assert_eq!(parsed.terminal, Terminal::Success);
    let usage = parsed.usage.unwrap();
    assert_eq!(
        (
            usage.input_tokens,
            usage.output_tokens,
            usage.cached_tokens,
            usage.reasoning_tokens
        ),
        (Some(5), Some(2), Some(1), Some(4))
    );
    assert_eq!(usage.cost_usd, None);
    let denied: Vec<&str> = parsed.denied.iter().map(|d| d.request.as_str()).collect();
    assert_eq!(denied, ["command: rm x"]);
}

#[test]
fn codex_failed_turn_keeps_its_message_and_error_events_are_collected() {
    let parsed = codex::parse(&lines(&[
        r#"{"type":"error","message":"Retrying connection"}"#,
        r#"{"type":"turn.failed","error":{"message":"rejected"}}"#,
    ]));
    assert_eq!(parsed.error_count, 1);
    assert_eq!(parsed.last_error.as_deref(), Some("Retrying connection"));
    assert_eq!(
        parsed.terminal,
        Terminal::Failure {
            message: "rejected".into(),
            code: None
        }
    );
}

#[test]
fn denied_requests_are_bounded() {
    let mut parsed = crate::provider::stream::Parsed::default();
    for i in 0..(crate::provider::limits::MAX_DENIED + 5) {
        parsed.deny("Bash", Some(&format!("cmd {i}")), None);
    }
    assert_eq!(parsed.denied.len(), crate::provider::limits::MAX_DENIED);
    assert_eq!(parsed.denied_omitted, 5);
    let mut parsed = crate::provider::stream::Parsed::default();
    parsed.deny(
        "Bash",
        Some(&"x".repeat(crate::provider::limits::DENIAL_TARGET_CHARS + 50)),
        None,
    );
    assert!(parsed.denied[0].request.ends_with("..."));
}

#[test]
fn the_mcp_override_quotes_names_and_disables_each_transport() {
    let text = codex::disabled_servers(
        r#"[{"name":"a\"b","transport":{"type":"stdio"}},{"name":"x.y","transport":{"type":"streamable_http"}}]"#,
    )
    .unwrap();
    assert_eq!(
        text,
        "mcp_servers={\"a\\\"b\"={enabled=false,command=\"__delega_disabled__\"},\"x.y\"={enabled=false,url=\"http://127.0.0.1:9\"}}"
    );
    assert_eq!(codex::disabled_servers("[]").unwrap(), "mcp_servers={}");
    for bad in [
        "",
        "{}",
        "[{\"transport\":{\"type\":\"stdio\"}}]",
        "[{\"name\":\"n\"}]",
    ] {
        assert!(codex::disabled_servers(bad).is_err(), "{bad}");
    }
}

#[test]
fn claude_cache_creation_tokens_count_as_input_and_an_error_flagged_success_has_a_plain_message() {
    let parsed = claude::parse(&lines(&[
        r#"{"type":"result","subtype":"success","result":"x","usage":{"input_tokens":3,"cache_creation_input_tokens":10,"cache_read_input_tokens":2,"output_tokens":1}}"#,
    ]));
    let usage = parsed.usage.unwrap();
    assert_eq!(
        (usage.input_tokens, usage.cached_tokens),
        (Some(13), Some(2))
    );
    let flagged = claude::parse(&lines(&[
        r#"{"type":"result","subtype":"success","is_error":true}"#,
    ]));
    assert_eq!(
        flagged.terminal,
        Terminal::Failure {
            message: "Claude Code reported an error".into(),
            code: None
        }
    );
}

fn failure_code(parsed: &crate::provider::stream::Parsed) -> Option<&'static str> {
    match parsed.terminal {
        Terminal::Failure { code, .. } => code,
        ref other => panic!("not a failure: {other:?}"),
    }
}

#[test]
fn claude_api_error_fields_on_the_result_alone_name_the_kind() {
    for (fields, kind) in [
        (r#""api_error_type":"permission_error""#, "provider_auth"),
        (r#""api_error_status":403"#, "provider_auth"),
        (r#""api_error_type":"billing_error""#, "provider_billing"),
        (
            r#""api_error_type":"not_found_error","api_error_status":404"#,
            "provider_model_unavailable",
        ),
        (r#""api_error_status":503"#, "provider_capacity"),
        (
            r#""api_error":"usage_limit_reached","api_error_status":429"#,
            "provider_usage_limit",
        ),
    ] {
        let line = format!(
            r#"{{"type":"result","subtype":"success","is_error":true,{fields},"result":"x"}}"#
        );
        let parsed = claude::parse(line.as_bytes());
        assert_eq!(failure_code(&parsed), Some(kind), "{fields}");
    }
}

#[test]
fn claude_not_found_without_404_is_not_a_missing_model() {
    let parsed = claude::parse(
        br#"{"type":"result","subtype":"success","is_error":true,"api_error_type":"not_found_error","result":"x"}"#,
    );
    assert_eq!(failure_code(&parsed), None);
}

#[test]
fn claude_sub_agent_api_errors_and_rate_limit_events_do_not_name_the_kind() {
    let parsed = claude::parse(&lines(&[
        r#"{"type":"assistant","parent_tool_use_id":"t1","is_api_error_message":true,"error":"model_not_found","message":{"content":[]}}"#,
        r#"{"type":"rate_limit_event","parent_tool_use_id":"t1","rate_limit_info":{"status":"rejected"}}"#,
        r#"{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["Connection reset"]}"#,
    ]));
    assert_eq!(failure_code(&parsed), None);
}

#[test]
fn claude_api_error_message_followed_by_more_work_does_not_name_a_later_failure() {
    let parsed = claude::parse(&lines(&[
        r#"{"type":"assistant","is_api_error_message":true,"error":"overloaded","api_error_status":529,"message":{"content":[]}}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"retried and went on"}]}}"#,
        r#"{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["Connection reset"]}"#,
    ]));
    assert_eq!(failure_code(&parsed), None);
}

#[test]
fn claude_rate_limit_event_with_allowed_warning_overage_is_not_a_usage_limit() {
    let parsed = claude::parse(&lines(&[
        r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","overageStatus":"allowed_warning"}}"#,
        r#"{"type":"result","subtype":"success","is_error":true,"result":"stopped"}"#,
    ]));
    assert_eq!(failure_code(&parsed), None);
}

#[test]
fn claude_api_error_fields_never_turn_a_success_into_a_failure() {
    let parsed = claude::parse(&lines(&[
        r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected"}}"#,
        r#"{"type":"result","subtype":"success","is_error":false,"api_error_status":429,"result":"fine"}"#,
    ]));
    assert_eq!(parsed.terminal, Terminal::Success);
}

#[test]
fn codex_structured_codes_and_json_documents_come_before_words() {
    for (line, kind) in [
        (
            r#"{"type":"turn.failed","error":{"message":"rate limit","codex_error_info":"context_window_exceeded"}}"#,
            "provider_context_exhausted",
        ),
        (
            r#"{"type":"turn.failed","error":{"message":"stopped","codex_error_info":{"usage_limit_exceeded":{}}}}"#,
            "provider_usage_limit",
        ),
        (
            r#"{"type":"turn.failed","error":{"message":"{\"error\":{\"type\":\"invalid_request_error\",\"code\":\"model_not_found\",\"message\":\"rate limit\"}}"}}"#,
            "provider_model_unavailable",
        ),
        (
            r#"{"type":"turn.failed","error":{"message":"{\"error\":{\"type\":\"insufficient_quota\"}}"}}"#,
            "provider_usage_limit",
        ),
    ] {
        let parsed = codex::parse(line.as_bytes());
        assert_eq!(failure_code(&parsed), Some(kind), "{line}");
    }
}

#[test]
fn codex_words_name_the_kind() {
    for (message, kind) in [
        ("Rate limit reached for requests", Some("provider_capacity")),
        ("The service is overloaded", Some("provider_capacity")),
        (
            "unexpected status 429 Too Many Requests",
            Some("provider_capacity"),
        ),
        ("unexpected status 403 Forbidden", Some("provider_auth")),
        (
            "The model `gpt-x` does not exist",
            Some("provider_model_unavailable"),
        ),
        (
            "You exceeded your current quota",
            Some("provider_usage_limit"),
        ),
        ("The file does not exist", None),
        ("Stream disconnected", None),
    ] {
        let line = serde_json::json!({"type": "turn.failed", "error": {"message": message}});
        let parsed = codex::parse(line.to_string().as_bytes());
        assert_eq!(failure_code(&parsed), kind, "{message}");
    }
}

#[test]
fn claude_work_after_a_rejected_rate_limit_event_keeps_a_later_failure_unnamed() {
    let parsed = claude::parse(&lines(&[
        r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","overageStatus":"rejected","isUsingOverage":false}}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"went on working"}]}}"#,
        r#"{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["Connection reset"]}"#,
    ]));
    assert_eq!(failure_code(&parsed), None);
}

#[test]
fn claude_sub_agent_result_neither_ends_nor_names_the_run() {
    let parsed = claude::parse(&lines(&[
        r#"{"type":"result","parent_tool_use_id":"t1","subtype":"success","is_error":true,"api_error_status":401,"result":"sub-agent auth failed"}"#,
        r#"{"type":"result","subtype":"success","is_error":false,"result":"the answer"}"#,
        r#"{"type":"result","parent_tool_use_id":"t2","subtype":"success","is_error":true,"result":"late"}"#,
    ]));
    assert_eq!(parsed.terminal, Terminal::Success);
    assert_eq!(parsed.final_text.as_deref(), Some("the answer"));
    assert!(parsed.notes.is_empty(), "{:?}", parsed.notes);

    let alone = claude::parse(
        br#"{"type":"result","parent_tool_use_id":"t1","subtype":"success","is_error":true,"api_error_status":401,"result":"x"}"#,
    );
    assert_eq!(alone.terminal, Terminal::None);
}

#[test]
fn codex_words_that_only_mention_a_model_or_a_quota_name_no_kind() {
    for message in [
        "Working directory /tmp/model-tests does not exist",
        "Local disk quota exceeded while saving the transcript",
        "The model output file does not exist",
        "model '' does not exist",
    ] {
        let line = serde_json::json!({"type": "turn.failed", "error": {"message": message}});
        let parsed = codex::parse(line.to_string().as_bytes());
        assert_eq!(failure_code(&parsed), None, "{message}");
    }
}

#[test]
fn codex_words_follow_the_documented_precedence() {
    for (message, kind) in [
        (
            "unexpected status 401 Unauthorized: access to the extended context window was denied",
            "provider_auth",
        ),
        (
            "The model 'gpt-x' does not exist; the context window is unknown",
            "provider_model_unavailable",
        ),
        (
            "context window full, rate limit also reached",
            "provider_context_exhausted",
        ),
        (
            "unexpected status 429: insufficient_quota",
            "provider_usage_limit",
        ),
        (
            "The model \"gpt-y\" does not exist",
            "provider_model_unavailable",
        ),
    ] {
        let line = serde_json::json!({"type": "turn.failed", "error": {"message": message}});
        let parsed = codex::parse(line.to_string().as_bytes());
        assert_eq!(failure_code(&parsed), Some(kind), "{message}");
    }
}
