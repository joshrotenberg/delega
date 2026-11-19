//! The exact command guard.

use serde_json::json;

use crate::provider::hook::{ALLOW_OUTPUT, DENIAL, GRANTS_ENV, command_granted, decide, run};

fn event(command: &str) -> Vec<u8> {
    json!({"tool_name": "Bash", "tool_input": {"command": command}})
        .to_string()
        .into_bytes()
}

#[test]
fn only_the_complete_command_is_allowed() {
    let grants = Some(r#"["printf ALLOWED"]"#);
    assert!(decide(&event("printf ALLOWED"), grants));
    for command in [
        "printf DENIED",
        "printf ALLOWED > file",
        "printf ALLOWED; touch file",
        "printf ALLOWED && touch file",
        "printf ALLOWED\n",
        " printf ALLOWED",
        "",
    ] {
        assert!(!decide(&event(command), grants), "{command:?}");
    }
}

#[test]
fn a_wildcard_grant_is_a_literal_command_and_never_a_prefix() {
    assert!(!decide(&event("go test ./..."), Some(r#"["go test:*"]"#)));
    assert!(!decide(&event("echo hi"), Some(r#"["echo *"]"#)));
}

#[test]
fn background_runs_and_sandbox_bypass_are_denied_even_for_a_granted_command() {
    let grants = json!(["printf ALLOWED"]);
    for extra in ["run_in_background", "dangerouslyDisableSandbox"] {
        let input = json!({"command": "printf ALLOWED", extra: true});
        assert!(!command_granted(&grants, &input), "{extra}");
    }
    let input = json!({"command": "printf ALLOWED", "run_in_background": false});
    assert!(command_granted(&grants, &input));
}

#[test]
fn anything_unusual_is_denied() {
    let grants = Some(r#"["printf ALLOWED"]"#);
    assert!(!decide(b"not JSON", grants));
    assert!(!decide(b"null", grants));
    assert!(!decide(&event("printf ALLOWED"), None));
    assert!(!decide(&event("printf ALLOWED"), Some("invalid")));
    assert!(!decide(&event("printf ALLOWED"), Some(r#"{"a":1}"#)));
    let other_tool = json!({"tool_name": "Read", "tool_input": {"command": "printf ALLOWED"}});
    assert!(!decide(other_tool.to_string().as_bytes(), grants));
    let no_command = json!({"tool_name": "Bash", "tool_input": {}});
    assert!(!decide(no_command.to_string().as_bytes(), grants));
}

#[test]
fn run_allows_with_exit_zero_and_denies_with_exit_two() {
    let grants = Some(r#"["make test"]"#);

    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run(
        &mut event("make test").as_slice(),
        grants,
        &mut out,
        &mut err,
    );
    assert_eq!(code, 0);
    assert_eq!(String::from_utf8(out).unwrap(), ALLOW_OUTPUT);
    assert!(err.is_empty());

    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run(
        &mut event("make deploy").as_slice(),
        grants,
        &mut out,
        &mut err,
    );
    assert_eq!(code, 2);
    assert!(out.is_empty());
    assert_eq!(String::from_utf8(err).unwrap(), DENIAL);
}

#[test]
fn run_denies_an_event_over_the_input_limit() {
    let big = vec![b' '; crate::provider::limits::HOOK_INPUT_LIMIT + 1];
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run(&mut big.as_slice(), Some(r#"["x"]"#), &mut out, &mut err);
    assert_eq!(code, 2);
    assert!(out.is_empty());
}

#[test]
fn the_grants_variable_name_is_stable() {
    assert_eq!(GRANTS_ENV, "DELEGA_EXACT_BASH_GRANTS");
}
