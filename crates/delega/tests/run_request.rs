//! How `delega run` builds the request, previews it, guards commands and
//! reports progress, against the fakes.

mod support;

use delega_core::contract::outcome::Outcome;
use delega_core::contract::permissions::Mode;
use delega_core::contract::request::Provider;
use serde_json::Value;
use std::io::Write;
use std::process::Stdio;
use support::{PROVIDERS, Rig, result_of, text, wait_bounded};

fn profile(rig: &Rig, body: &str) -> String {
    rig.file("profile.json", body)
        .to_string_lossy()
        .into_owned()
}

#[test]
fn a_profile_supplies_the_request_and_flags_override_it_field_by_field() {
    let rig = Rig::new();
    let file = profile(
        &rig,
        r#"{"schema_version":1,"provider":"codex","prompt":"success","timeout_secs":30,
            "permissions":{"mode":"write"}}"#,
    );
    // The profile alone: codex, 30 s, write.
    let out = rig
        .command()
        .args(["run", "--request", &file, "-C"])
        .arg(&rig.work)
        .args(["--output", "json"])
        .output()
        .expect("run delega");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let result = result_of(&out);
    assert_eq!(result.provider, Some(Provider::Codex));
    assert_eq!(result.timing.timeout_secs, 30);
    assert_eq!(result.permissions.mode, Mode::Write);

    // Flags win: claude and read replace the file's values, the time limit
    // the flags do not mention stays.
    let out = rig
        .command()
        .args(["run", "--request", &file, "--provider", "claude", "--read"])
        .args(["-C"])
        .arg(&rig.work)
        .args(["--output", "json"])
        .output()
        .expect("run delega");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let result = result_of(&out);
    assert_eq!(result.provider, Some(Provider::Claude));
    assert_eq!(result.permissions.mode, Mode::Read);
    assert_eq!(result.timing.timeout_secs, 30);
}

#[test]
fn an_explicit_prompt_replaces_the_prompt_in_the_profile() {
    let rig = Rig::new();
    let file = profile(
        &rig,
        r#"{"schema_version":1,"provider":"claude","prompt":"provider-error"}"#,
    );
    let out = rig
        .command()
        .args(["run", "--request", &file, "-p", "success", "-C"])
        .arg(&rig.work)
        .output()
        .expect("run delega");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "fake final answer\n");
}

#[test]
fn stdin_supplies_the_assignment_and_reaches_the_provider() {
    for provider in PROVIDERS {
        let rig = Rig::new();
        let args = [
            "run",
            "--provider",
            provider,
            "--stdin",
            "-C",
            &rig.work.to_string_lossy(),
        ]
        .map(String::from);
        let out = rig.with_stdin(&args, "success\nsecond line of the assignment\n");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{provider}: {}",
            text(&out.stderr)
        );
        assert_eq!(text(&out.stdout), "fake final answer\n");
        let seen = rig.recorded("stdin");
        assert!(
            seen.starts_with("success\nsecond line"),
            "{provider}: {seen}"
        );
        rig.assert_nothing_left();
    }
}

#[test]
fn prompt_together_with_stdin_is_a_refused_result_not_a_usage_error() {
    let rig = Rig::new();
    let args = rig.run_args("claude", "success", &["--stdin", "--output", "json"]);
    let out = rig.with_stdin(&args, "ignored");
    assert_eq!(out.status.code(), Some(2));
    let result = result_of(&out);
    assert_eq!(result.outcome, Outcome::Refused);
    let error = result.error.expect("error");
    assert_eq!(error.kind, "invalid_request");
    assert!(error.message.contains("--stdin"), "{}", error.message);
    assert!(!rig.record.join("argv").exists());

    let args = rig.run_args("claude", "success", &["--stdin"]);
    let out = rig.with_stdin(&args, "ignored");
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(text(&out.stdout), "");
    assert!(
        text(&out.stderr).contains("refused: prompt:"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn an_answer_schema_file_asks_for_and_validates_a_structured_answer() {
    for provider in PROVIDERS {
        let rig = Rig::new();
        let schema = rig
            .file(
                "schema.json",
                r#"{"type":"object","properties":{"answer":{"type":"integer"}},"required":["answer"]}"#,
            )
            .to_string_lossy()
            .into_owned();
        let out = rig.run(
            provider,
            "structured",
            &["--schema", &schema, "--output", "json"],
        );
        assert_eq!(out.status.code(), Some(0), "{provider}");
        let result = result_of(&out);
        assert_eq!(result.structured, Some(serde_json::json!({"answer": 42})));
        assert_eq!(result.structured_valid, Some(true));

        let out = rig.run(
            provider,
            "structured-invalid",
            &["--schema", &schema, "--output", "json"],
        );
        let result = result_of(&out);
        assert_eq!(result.outcome, Outcome::Completed);
        assert_eq!(result.structured_valid, Some(false));
        rig.assert_nothing_left();
    }
}

#[test]
fn an_unreadable_or_malformed_schema_file_is_a_refused_result() {
    let rig = Rig::new();
    let missing = rig.path("missing.json").to_string_lossy().into_owned();
    let out = rig.run(
        "codex",
        "success",
        &["--schema", &missing, "--output", "json"],
    );
    assert_eq!(out.status.code(), Some(2));
    let result = result_of(&out);
    assert_eq!(result.outcome, Outcome::Refused);
    assert!(
        result
            .error
            .expect("error")
            .message
            .contains("answer_schema")
    );

    let broken = rig
        .file("broken.json", "{not json")
        .to_string_lossy()
        .into_owned();
    let out = rig.run(
        "codex",
        "success",
        &["--schema", &broken, "--output", "json"],
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(
        result_of(&out)
            .error
            .expect("error")
            .message
            .contains("is not valid JSON")
    );
    assert!(!rig.record.join("argv").exists());
}

#[test]
fn resume_and_ephemeral_reach_the_provider() {
    let rig = Rig::new();
    let out = rig.run(
        "codex",
        "success",
        &["--resume", "sess-9", "--output", "json"],
    );
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(result_of(&out).session_id.as_deref(), Some("sess-9"));

    let rig = Rig::new();
    let out = rig.run("claude", "success", &["--ephemeral", "--output", "json"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(result_of(&out).session_id, None);
}

#[test]
fn an_invalid_profile_is_a_refused_result_in_json_and_problems_on_stderr_in_text() {
    let rig = Rig::new();
    let cases = [
        (
            "missing file",
            rig.path("none.json").to_string_lossy().into_owned(),
        ),
        ("not json", profile(&rig, "not json")),
        (
            "unknown field",
            profile(
                &rig,
                r#"{"schema_version":1,"provider":"codex","prompt":"x","mode":"write"}"#,
            ),
        ),
        (
            "no schema_version",
            profile(&rig, r#"{"provider":"codex","prompt":"x"}"#),
        ),
    ];
    for (what, file) in cases {
        let out = rig
            .command()
            .args(["run", "--request", &file, "--output", "json"])
            .output()
            .expect("run delega");
        assert_eq!(out.status.code(), Some(2), "{what}");
        let result = result_of(&out);
        assert_eq!(result.outcome, Outcome::Refused, "{what}");
        let error = result.error.expect("error");
        assert_eq!(error.kind, "invalid_request", "{what}");
        assert!(
            error.message.starts_with("request:"),
            "{what}: {}",
            error.message
        );

        let out = rig
            .command()
            .args(["run", "--request", &file])
            .output()
            .expect("run delega");
        assert_eq!(out.status.code(), Some(2), "{what}");
        assert_eq!(text(&out.stdout), "", "{what}");
        assert!(text(&out.stderr).contains("refused: request:"), "{what}");
    }
    assert!(!rig.record.join("argv").exists());
}

#[test]
fn validation_problems_are_all_listed_in_one_refused_result() {
    let rig = Rig::new();
    // No provider, no prompt, and a time limit the contract rejects.
    let out = rig
        .command()
        .args(["run", "--timeout", "0", "--output", "json"])
        .output()
        .expect("run delega");
    assert_eq!(out.status.code(), Some(2));
    let result = result_of(&out);
    assert_eq!(result.outcome, Outcome::Refused);
    let message = result.error.expect("error").message;
    for needle in [
        "provider: is required",
        "prompt: is required",
        "timeout_secs",
    ] {
        assert!(message.contains(needle), "{needle} missing from {message}");
    }
    assert_eq!(result.provider, None);
}

fn dry_run(rig: &Rig, provider: &str, extra: &[&str]) -> std::process::Output {
    let mut command = rig.command();
    command.env("MY_SERVICE_API_TOKEN", "never-print-this-value-1234");
    command
        .args(rig.run_args(provider, "success", &["--dry-run"]))
        .args(extra)
        .output()
        .expect("run delega")
}

#[test]
fn a_dry_run_describes_the_launch_for_each_provider_and_starts_nothing() {
    for provider in PROVIDERS {
        let rig = Rig::new();
        let out = dry_run(&rig, provider, &["--allow-command", "make test"]);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{provider}: {}",
            text(&out.stderr)
        );
        let shown = text(&out.stdout);
        assert!(shown.starts_with("dry run: nothing was started"), "{shown}");
        assert!(shown.contains(&format!("provider: {provider}")), "{shown}");
        assert!(shown.contains("fake-"), "the program is named: {shown}");
        assert!(shown.contains("time limit: 600 s"), "{shown}");
        assert!(shown.contains("environment variable names:"), "{shown}");
        assert!(shown.contains("PATH"), "names only: {shown}");
        assert!(!shown.contains("never-print-this-value"), "{shown}");
        assert!(!text(&out.stderr).contains("never-print-this-value"));
        assert!(!rig.record.join("argv").exists(), "{provider} was started");
        assert!(!rig.record.join("pid").exists(), "{provider} was started");
    }
}

#[test]
fn a_dry_run_in_json_is_a_versioned_object_with_names_and_no_values() {
    for provider in PROVIDERS {
        let rig = Rig::new();
        let out = dry_run(&rig, provider, &["--output", "json"]);
        assert_eq!(out.status.code(), Some(0), "{provider}");
        let value: Value = serde_json::from_slice(&out.stdout).expect("JSON");
        assert_eq!(value["schema_version"], 1);
        let plan = &value["dry_run"];
        assert_eq!(plan["provider"], provider);
        assert_eq!(plan["time_limit_secs"], 600);
        assert_eq!(plan["mapping"]["mode"], "read");
        let names: Vec<&str> = plan["env_names"]
            .as_array()
            .expect("env_names")
            .iter()
            .map(|n| n.as_str().expect("name"))
            .collect();
        assert!(names.contains(&"PATH"), "{names:?}");
        assert!(!text(&out.stdout).contains("never-print-this-value"));
        assert!(!rig.record.join("argv").exists());
    }
}

#[test]
fn a_dry_run_of_a_request_that_would_be_refused_prints_the_refusal_and_exits_two() {
    let rig = Rig::new();
    let missing = rig.path("nowhere");
    let out = rig.run_in(&missing, "claude", "success", &["--dry-run"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(text(&out.stdout), "");
    assert!(
        text(&out.stderr).contains("refused: cwd_unusable"),
        "{}",
        text(&out.stderr)
    );

    let out = rig.run_in(
        &missing,
        "claude",
        "success",
        &["--dry-run", "--output", "json"],
    );
    assert_eq!(out.status.code(), Some(2));
    let result = result_of(&out);
    assert_eq!(result.outcome, Outcome::Refused);
    assert_eq!(result.error.expect("error").kind, "cwd_unusable");
    assert!(!rig.record.join("argv").exists());
}

#[test]
fn an_allowed_command_runs_through_the_real_guard_and_an_unlisted_one_is_denied() {
    let rig = Rig::new();
    let out = rig.run(
        "claude",
        "tool-bash:make test",
        &["--allow-command", "make test", "--output", "json"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let result = result_of(&out);
    assert_eq!(result.text, "ran: make test");
    assert!(result.permissions.denied.is_empty());
    assert_eq!(
        rig.recorded("hook").trim(),
        "0",
        "the fake ran this executable's guard and it allowed the call"
    );

    let rig = Rig::new();
    let out = rig.run(
        "claude",
        "tool-bash:rm -rf data",
        &["--allow-command", "make test", "--output", "json"],
    );
    assert_eq!(out.status.code(), Some(0));
    let result = result_of(&out);
    assert_eq!(result.text, "could not run it");
    assert_eq!(result.permissions.denied.len(), 1);
    assert!(result.permissions.denied[0].request.contains("rm -rf data"));
    rig.assert_nothing_left();
}

#[test]
fn without_allow_command_no_bash_is_granted_and_the_guard_is_not_installed() {
    let rig = Rig::new();
    let out = rig.run("claude", "tool-bash:make test", &["--output", "json"]);
    let result = result_of(&out);
    assert_eq!(result.permissions.denied.len(), 1);
    assert!(rig.recorded("hook").is_empty(), "no hook ran");
}

fn guard(rig: &Rig, grants: Option<&str>, event: &str) -> std::process::Output {
    let mut command = rig.command();
    if let Some(grants) = grants {
        command.env("DELEGA_EXACT_BASH_GRANTS", grants);
    }
    let mut child = command
        .arg("command-guard")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn delega");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(event.as_bytes())
        .expect("write event");
    wait_bounded(child)
}

#[test]
fn the_command_guard_subcommand_allows_with_zero_and_denies_with_two() {
    let rig = Rig::new();
    let grants = Some(r#"["make test"]"#);
    let allow = r#"{"tool_name":"Bash","tool_input":{"command":"make test"}}"#;
    let deny = r#"{"tool_name":"Bash","tool_input":{"command":"make test; rm -rf /"}}"#;

    let out = guard(&rig, grants, allow);
    assert_eq!(out.status.code(), Some(0));
    assert!(text(&out.stdout).contains("\"permissionDecision\":\"allow\""));

    let out = guard(&rig, grants, deny);
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stderr).contains("denied by the exact command guard"));

    // No grants in the environment denies everything.
    let out = guard(&rig, None, allow);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn the_command_guard_is_hidden_from_help() {
    let rig = Rig::new();
    let out = rig.delega(&["--help"]);
    assert!(!text(&out.stdout).contains("command-guard"));
    let out = rig.delega(&["run", "--help"]);
    assert!(!text(&out.stdout).contains("command-guard"));
}

#[test]
fn progress_lines_go_to_stderr_and_stdout_is_unchanged() {
    for provider in PROVIDERS {
        let rig = Rig::new();
        let quiet = rig.run(provider, "success", &[]);
        let out = rig.run(provider, "success", &["--progress"]);
        assert_eq!(out.status.code(), Some(0), "{provider}");
        assert_eq!(out.stdout, quiet.stdout, "{provider}");
        let err = text(&out.stderr);
        let lines: Vec<&str> = err.lines().collect();
        assert!(lines.iter().all(|l| l.starts_with("progress: ")), "{err}");
        assert!(
            lines[0].starts_with(&format!("progress: started {provider} (")),
            "{err}"
        );
        assert_eq!(
            lines.last(),
            Some(&"progress: finished: completed"),
            "{err}"
        );
        assert!(lines.len() >= 4, "{err}");
        let last_event = if provider == "claude" {
            "progress: result: success"
        } else {
            "progress: turn.completed"
        };
        assert!(lines.contains(&last_event), "{err}");
        assert_eq!(text(&quiet.stderr), "");
        rig.assert_nothing_left();
    }
}

#[test]
fn progress_in_json_mode_leaves_stdout_a_single_result_document() {
    let rig = Rig::new();
    let out = rig.run("codex", "success", &["--progress", "--output", "json"]);
    assert_eq!(out.status.code(), Some(0));
    let result = result_of(&out);
    assert_eq!(result.outcome, Outcome::Completed);
    assert!(text(&out.stderr).contains("progress: finished: completed"));
}

#[test]
fn progress_reports_a_denial_and_never_a_command_or_an_environment_value() {
    let rig = Rig::new();
    let out = rig
        .command()
        .env("MY_SERVICE_API_TOKEN", "never-print-this-value-1234")
        .args(rig.run_args("codex", "denied-completed", &["--progress"]))
        .output()
        .expect("run delega");
    assert_eq!(out.status.code(), Some(0));
    let err = text(&out.stderr);
    assert!(
        err.contains("progress: denied: command: rm -rf /tmp/never"),
        "{err}"
    );
    assert!(!err.contains("never-print-this-value"), "{err}");
    // The provider's own command events carry the type and status only.
    assert!(
        err.contains("progress: item.completed: command_execution declined"),
        "{err}"
    );
}

#[test]
fn progress_for_a_refused_request_is_the_finish_line_only() {
    let rig = Rig::new();
    let missing = rig.path("nowhere");
    let out = rig.run_in(&missing, "codex", "success", &["--progress"]);
    assert_eq!(out.status.code(), Some(2));
    let err = text(&out.stderr);
    let lines: Vec<&str> = err.lines().collect();
    assert_eq!(lines[0], "progress: finished: refused", "{err}");
    assert!(
        lines[1].starts_with("delega run: refused: cwd_unusable: "),
        "{err}"
    );
    assert_eq!(lines.len(), 2, "{err}");
}
