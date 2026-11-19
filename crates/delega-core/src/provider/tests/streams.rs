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
        Terminal::Failure("Turn limit reached; second".into())
    );
    let without = claude::parse(&lines(&[
        r#"{"type":"result","subtype":"error_max_budget_usd","is_error":true}"#,
    ]));
    assert_eq!(
        without.terminal,
        Terminal::Failure("Claude Code ended with error_max_budget_usd".into())
    );
    let flagged = claude::parse(&lines(&[
        r#"{"type":"result","subtype":"success","is_error":true,"result":"x"}"#,
    ]));
    assert!(matches!(flagged.terminal, Terminal::Failure(_)));
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
    assert_eq!(parsed.errors, ["Retrying connection"]);
    assert_eq!(parsed.terminal, Terminal::Failure("rejected".into()));
}

#[test]
fn denied_requests_are_bounded() {
    let mut parsed = crate::provider::stream::Parsed::default();
    for i in 0..(crate::provider::limits::MAX_DENIED + 5) {
        parsed.deny("Bash", Some(&format!("cmd {i}")));
    }
    assert_eq!(parsed.denied.len(), crate::provider::limits::MAX_DENIED);
    assert_eq!(parsed.denied_omitted, 5);
    let mut parsed = crate::provider::stream::Parsed::default();
    parsed.deny(
        "Bash",
        Some(&"x".repeat(crate::provider::limits::DENIAL_TARGET_CHARS + 50)),
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
