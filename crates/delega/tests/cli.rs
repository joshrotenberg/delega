//! Runs the built `delega` binary and checks its output and exit status.

use std::process::{Command, Output};

fn delega(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_delega"))
        .args(args)
        .output()
        .expect("run delega")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("utf-8 output")
}

#[test]
fn schema_request_prints_the_request_schema_and_exits_zero() {
    let out = delega(&["schema", "request"]);
    assert_eq!(out.status.code(), Some(0));
    let schema: serde_json::Value = serde_json::from_str(&text(&out.stdout)).expect("JSON");
    assert_eq!(schema["title"], "Request");
    assert_eq!(schema["additionalProperties"], false);
}

#[test]
fn schema_result_prints_the_result_schema_and_exits_zero() {
    let out = delega(&["schema", "result"]);
    assert_eq!(out.status.code(), Some(0));
    let schema: serde_json::Value = serde_json::from_str(&text(&out.stdout)).expect("JSON");
    assert_eq!(schema["title"], "RunResult");
}

#[test]
fn schema_output_equals_the_committed_file() {
    let out = delega(&["schema", "request"]);
    let committed = include_str!("../../../schemas/request.schema.json");
    assert_eq!(text(&out.stdout), committed);
}

#[test]
fn version_prints_the_core_version_and_exits_zero() {
    let out = delega(&["version"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(text(&out.stdout).trim(), delega_core_version());
}

fn delega_core_version() -> &'static str {
    // The workspace version is shared by every crate, so this crate's version
    // is the one the binary must report.
    env!("CARGO_PKG_VERSION")
}

#[test]
fn unimplemented_subcommands_say_so_on_stderr_and_exit_two() {
    for args in [
        &["run", "--provider", "codex", "-p", "hi"][..],
        &["mcp"],
        &["init", "--dry-run"],
        &["skill", "list"],
        &["doctor"],
    ] {
        let out = delega(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(
            text(&out.stderr).contains("not implemented yet"),
            "{args:?}"
        );
        assert!(out.stdout.is_empty(), "{args:?} wrote to stdout");
    }
}

#[test]
fn unknown_subcommand_is_a_usage_error_with_exit_two() {
    let out = delega(&["frobnicate"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stderr).contains("Usage"));
}

#[test]
fn bad_schema_argument_is_a_usage_error_with_exit_two() {
    let out = delega(&["schema", "response"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn conflicting_permission_flags_are_a_usage_error_with_exit_two() {
    let out = delega(&["run", "--write", "--full"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn help_lists_every_planned_subcommand() {
    let out = delega(&["--help"]);
    assert_eq!(out.status.code(), Some(0));
    let help = text(&out.stdout);
    for name in ["run", "mcp", "init", "schema", "skill", "doctor", "version"] {
        assert!(
            help.lines().any(|l| l.trim_start().starts_with(name)),
            "help does not list {name}"
        );
    }
}
