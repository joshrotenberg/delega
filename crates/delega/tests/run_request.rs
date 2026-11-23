//! How `delega run` builds the request, previews it, guards commands and
//! reports progress, against the fakes.

mod support;

use delega_core::contract::outcome::Outcome;
use delega_core::contract::permissions::Mode;
use delega_core::contract::request::Provider;
use serde_json::Value;
use std::io::Write;
use std::process::Stdio;
use support::{Guarded, PROVIDERS, Rig, result_of, text, wait_bounded, wait_bounded_for};

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
        .guarded_output()
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
        .guarded_output()
        .expect("run delega");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let result = result_of(&out);
    assert_eq!(result.provider, Some(Provider::Claude));
    assert_eq!(result.permissions.mode, Mode::Read);
    assert_eq!(result.timing.timeout_secs, 30);
    rig.assert_nothing_left();
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
        .guarded_output()
        .expect("run delega");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "fake final answer\n");
    rig.assert_nothing_left();
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
            "--env",
            "FAKE_RECORD_DIR",
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
    rig.assert_nothing_left();
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
            .guarded_output()
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
            .guarded_output()
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
        .guarded_output()
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
        .guarded_output()
        .expect("run delega")
}

#[test]
fn a_claude_dry_run_lists_deny_files_and_rules_under_their_own_heading_and_nothing_else() {
    let rig = Rig::new();
    let config = rig.path("home");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("settings.json"),
        r#"{"permissions":{"deny":["Bash(make deploy)"]},"env":{"X":"CANARY-ENV-BLOCK"},
            "hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"CANARY-HOOK"}]}]}}"#,
    )
    .unwrap();
    let out = dry_run(&rig, "claude", &[]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let shown = text(&out.stdout);
    assert!(
        shown.contains("Claude Code settings files read for deny rules:"),
        "{shown}"
    );
    assert!(shown.contains("settings.json: 1 deny rules"), "{shown}");
    assert!(shown.contains("user deny rules (1 applied;"), "{shown}");
    assert!(shown.contains("\"Bash(make deploy)\""), "{shown}");
    assert!(!shown.contains("CANARY"), "{shown}");

    let out = dry_run(&rig, "claude", &["--output", "json"]);
    let json: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        json["dry_run"]["deny_rules"]["user_deny_rules"],
        serde_json::json!(["Bash(make deploy)"])
    );
    assert!(!text(&out.stdout).contains("CANARY"));

    // Codex has no deny rules and prints no such heading.
    let out = dry_run(&rig, "codex", &[]);
    let shown = text(&out.stdout);
    assert!(!shown.contains("deny rules"), "{shown}");
}

#[test]
fn a_malformed_claude_settings_file_refuses_the_run_with_exit_two_naming_the_file() {
    let rig = Rig::new();
    let config = rig.path("home");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("settings.json"), "{ CANARY-NOT-JSON").unwrap();
    let out = dry_run(&rig, "claude", &[]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    let err = text(&out.stderr);
    assert!(err.contains("settings_unreadable"), "{err}");
    assert!(err.contains("settings.json"), "{err}");
    assert!(!err.contains("CANARY"), "{err}");
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
fn the_build_flag_selects_build_mode_for_each_provider() {
    for provider in PROVIDERS {
        let rig = Rig::new();
        let out = dry_run(&rig, provider, &["--build", "--output", "json"]);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{provider}: {}",
            text(&out.stderr)
        );
        let value: Value = serde_json::from_slice(&out.stdout).expect("JSON");
        assert_eq!(value["dry_run"]["mapping"]["mode"], "build", "{provider}");
        assert!(!rig.record.join("argv").exists());
    }
}

#[test]
fn the_build_flag_with_an_allowed_command_is_a_refused_request() {
    let rig = Rig::new();
    let out = dry_run(
        &rig,
        "claude",
        &[
            "--build",
            "--allow-command",
            "cargo test",
            "--output",
            "json",
        ],
    );
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    let result = result_of(&out);
    assert_eq!(result.outcome, Outcome::Refused);
    let error = result.error.expect("error");
    assert_eq!(error.kind, "invalid_request");
    let message = error.message;
    assert!(
        message.contains("permissions.commands: must be empty in build mode"),
        "{message}"
    );
    assert!(!rig.record.join("argv").exists());
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
        .guarded_spawn()
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

// The provider merges cleanup reasons from its subprocesses with "; ".
fn strip_load_cleanup_reason(reason: &str) -> Option<&str> {
    use delega_core::process::LATE_LISTING_REASON;

    if let Some(rest) = reason.strip_prefix(LATE_LISTING_REASON) {
        return Some(rest);
    }
    if let Some(reason) = reason.strip_prefix("process inspection was unavailable (") {
        let (error, rest) = reason.split_once(
            "); only the child's process group was checked with kill(-pgid, 0): \
             no process has the child's group id. Descendants outside the group and session \
             could not be checked.",
        )?;
        return (error == "process listing timed out"
            || error
                .strip_prefix("ps could not be started (")
                .and_then(|detail| detail.strip_suffix(')'))
                .is_some_and(|detail| !detail.is_empty()))
        .then_some(rest);
    }
    // The final cleanup pass can extend the deadline. The owner prints the
    // actual duration with one decimal place.
    let (seconds, rest) = reason
        .strip_prefix("stopping was not confirmed within ")?
        .split_once(" s: ")?;
    let (whole, tenth) = seconds.split_once('.')?;
    if whole.is_empty()
        || !whole.bytes().all(|digit| digit.is_ascii_digit())
        || tenth.len() != 1
        || !tenth.bytes().all(|digit| digit.is_ascii_digit())
    {
        return None;
    }
    rest.strip_prefix("the child had not exited or the sweeps were not yet clean")
}

fn log_load_cleanup_summary(line: &str) -> bool {
    let Some(mut reason) = line
        .strip_prefix("delega run: cleanup not confirmed: ")
        .and_then(|rest| rest.strip_suffix(" (pids [], process groups [])"))
    else {
        return false;
    };
    while let Some(rest) = strip_load_cleanup_reason(reason) {
        if rest.is_empty() {
            eprintln!("{line}");
            return true;
        }
        let Some(next) = rest.strip_prefix("; ") else {
            break;
        };
        reason = next;
    }
    false
}

#[test]
fn progress_lines_go_to_stderr_and_stdout_is_unchanged() {
    for provider in PROVIDERS {
        let rig = Rig::new();
        let quiet = rig.run(provider, "success", &[]);
        let progress_rig = Rig::new();
        let out = progress_rig.run(provider, "success", &["--progress"]);
        assert_eq!(quiet.status.code(), Some(0), "{provider}");
        assert_eq!(out.status.code(), Some(0), "{provider}");
        assert_eq!(out.stdout, quiet.stdout, "{provider}");
        let err = text(&out.stderr);
        let lines: Vec<_> = err
            .lines()
            .filter(|line| !log_load_cleanup_summary(line))
            .collect();
        assert!(lines.iter().all(|l| l.starts_with("progress: ")), "{err}");
        assert!(
            lines
                .first()
                .is_some_and(|line| line.starts_with(&format!("progress: started {provider} ("))),
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
        let quiet_err = text(&quiet.stderr);
        assert!(
            quiet_err.lines().all(log_load_cleanup_summary),
            "{quiet_err}"
        );
        rig.assert_nothing_left();
        progress_rig.assert_nothing_left();
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
        .args(rig.run_args(
            "codex",
            "denied-completed",
            &["--progress", "--env", "MY_SERVICE_API_TOKEN"],
        ))
        .guarded_output()
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

fn dry_json(rig: &Rig, args: &[&str]) -> Value {
    let out = rig
        .command()
        .args(["run", "--dry-run", "--output", "json"])
        .args(args)
        .guarded_output()
        .expect("run delega");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    serde_json::from_slice(&out.stdout).expect("JSON")
}

#[test]
fn a_relative_extra_directory_resolves_against_the_callers_directory() {
    let rig = Rig::new();
    std::fs::create_dir(rig.path("shared")).expect("create shared");
    // From the rig's root, the worker runs in `work` and may read `shared`, a
    // sibling. Resolved against the worker's directory it would not exist.
    let out = rig
        .command()
        .current_dir(rig.path(""))
        .args([
            "run",
            "--dry-run",
            "--output",
            "json",
            "--provider",
            "claude",
            "-p",
            "x",
        ])
        .args(["-C", "work", "--add-dir", "shared"])
        .guarded_output()
        .expect("run delega");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let value: Value = serde_json::from_slice(&out.stdout).expect("JSON");
    let dirs = value["dry_run"]["mapping"]["directories"]
        .as_array()
        .expect("directories");
    assert_eq!(dirs.len(), 1);
    assert_eq!(dirs[0], rig.path("shared").to_string_lossy().as_ref());
    assert_eq!(value["dry_run"]["cwd"], rig.work.to_string_lossy().as_ref());
}

#[test]
fn run_without_cwd_uses_the_callers_current_directory() {
    let rig = Rig::new();
    let out = rig
        .command()
        .current_dir(&rig.work)
        .args([
            "run",
            "--dry-run",
            "--output",
            "json",
            "--provider",
            "claude",
            "-p",
            "x",
        ])
        .guarded_output()
        .expect("run delega");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let value: Value = serde_json::from_slice(&out.stdout).expect("JSON");
    assert_eq!(value["dry_run"]["cwd"], rig.work.to_string_lossy().as_ref());
}

#[test]
fn relative_paths_in_a_profile_resolve_against_the_callers_directory_too() {
    let rig = Rig::new();
    std::fs::create_dir(rig.path("shared")).expect("create shared");
    let file = profile(
        &rig,
        r#"{"schema_version":1,"provider":"claude","prompt":"x","cwd":"work",
            "permissions":{"directories":["shared"]}}"#,
    );
    let out = rig
        .command()
        .current_dir(rig.path(""))
        .args(["run", "--dry-run", "--output", "json", "--request", &file])
        .guarded_output()
        .expect("run delega");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let value: Value = serde_json::from_slice(&out.stdout).expect("JSON");
    assert_eq!(value["dry_run"]["cwd"], rig.work.to_string_lossy().as_ref());
    assert_eq!(
        value["dry_run"]["mapping"]["directories"][0],
        rig.path("shared").to_string_lossy().as_ref()
    );
}

#[test]
fn a_relative_request_and_schema_path_are_read_from_the_callers_directory() {
    let rig = Rig::new();
    rig.file(
        "profile.json",
        r#"{"schema_version":1,"provider":"codex","prompt":"success"}"#,
    );
    rig.file("schema.json", r#"{"type":"object"}"#);
    let out = rig
        .command()
        .current_dir(rig.path(""))
        .args([
            "run",
            "--request",
            "profile.json",
            "--schema",
            "schema.json",
            "-C",
            "work",
        ])
        .args(["--dry-run"])
        .guarded_output()
        .expect("run delega");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
}

#[test]
fn a_list_flag_replaces_the_profiles_list() {
    let rig = Rig::new();
    let file = profile(
        &rig,
        r#"{"schema_version":1,"provider":"claude","prompt":"x",
            "permissions":{"commands":["from profile"]}}"#,
    );
    let cwd = rig.work.to_string_lossy().into_owned();
    let kept = dry_json(&rig, &["--request", &file, "-C", &cwd]);
    assert_eq!(
        kept["dry_run"]["mapping"]["commands"],
        serde_json::json!(["from profile"])
    );
    let replaced = dry_json(
        &rig,
        &[
            "--request",
            &file,
            "-C",
            &cwd,
            "--allow-command",
            "from flag",
        ],
    );
    assert_eq!(
        replaced["dry_run"]["mapping"]["commands"],
        serde_json::json!(["from flag"])
    );
}

#[test]
fn resume_replaces_an_ephemeral_session_from_the_profile() {
    let rig = Rig::new();
    let file = profile(
        &rig,
        r#"{"schema_version":1,"provider":"codex","prompt":"success","session":{"ephemeral":true}}"#,
    );
    let out = rig
        .command()
        .args(["run", "--request", &file, "--resume", "sess-4", "-C"])
        .arg(&rig.work)
        .args(["--output", "json"])
        .guarded_output()
        .expect("run delega");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    // An ephemeral run reports no session id; this one reports the resumed id.
    assert_eq!(result_of(&out).session_id.as_deref(), Some("sess-4"));
    rig.assert_nothing_left();
}

#[test]
fn a_prompt_that_starts_with_a_hyphen_is_given_in_the_equals_form() {
    for form in [
        "--prompt=-text that starts with a hyphen",
        "-p=-text that starts with a hyphen",
    ] {
        let rig = Rig::new();
        let out = rig
            .command()
            .args(["run", "--provider", "claude", form, "-C"])
            .arg(&rig.work)
            .args(["--env", "FAKE_RECORD_DIR"])
            .guarded_output()
            .expect("run delega");
        assert_eq!(out.status.code(), Some(0), "{form}: {}", text(&out.stderr));
        assert!(
            rig.recorded("stdin").starts_with("-text that starts"),
            "{form}"
        );
        rig.assert_nothing_left();
    }
}

#[test]
fn a_prompt_flag_does_not_swallow_the_flag_after_it() {
    let rig = Rig::new();
    let out = rig
        .command()
        .args(["run", "--provider", "claude", "-p", "--dry-run", "-C"])
        .arg(&rig.work)
        .guarded_output()
        .expect("run delega");
    // A usage error, not a run whose assignment is "--dry-run".
    assert_eq!(out.status.code(), Some(2));
    assert!(
        text(&out.stderr).contains("a value is required"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(text(&out.stdout), "");
    assert!(!rig.record.join("argv").exists());
}

#[test]
fn a_request_refused_while_it_is_built_ends_the_progress_with_a_finish_line() {
    let rig = Rig::new();
    let out = rig
        .command()
        .args(["run", "--progress"])
        .guarded_output()
        .expect("run delega");
    assert_eq!(out.status.code(), Some(2));
    let err = text(&out.stderr);
    assert!(err.starts_with("progress: finished: refused\n"), "{err}");
    assert!(err.contains("refused: provider: is required"), "{err}");
}

#[test]
fn a_profile_or_schema_that_is_not_a_regular_file_or_too_large_is_refused_naming_why() {
    let rig = Rig::new();
    // A device that never ends must be refused, not read.
    for (flag, field) in [("--request", "request"), ("--schema", "answer_schema")] {
        let out = rig
            .command()
            .args([
                "run",
                "--provider",
                "codex",
                "-p",
                "x",
                flag,
                "/dev/zero",
                "--output",
                "json",
            ])
            .guarded_output()
            .expect("run delega");
        assert_eq!(out.status.code(), Some(2), "{flag}");
        let message = result_of(&out).error.expect("error").message;
        assert!(message.starts_with(&format!("{field}:")), "{message}");
        assert!(message.contains("not a regular file"), "{message}");
    }
    // A file over the limit is refused, and the message names the limit.
    let big = rig.file("big.json", &" ".repeat(6 * 1024 * 1024));
    let big = big.to_string_lossy().into_owned();
    for flag in ["--request", "--schema"] {
        let out = rig
            .command()
            .args([
                "run",
                "--provider",
                "codex",
                "-p",
                "x",
                flag,
                &big,
                "--output",
                "json",
            ])
            .guarded_output()
            .expect("run delega");
        assert_eq!(out.status.code(), Some(2), "{flag}");
        let message = result_of(&out).error.expect("error").message;
        assert!(
            message.contains("larger than the limit of"),
            "{flag}: {message}"
        );
    }
}

#[test]
fn an_assignment_on_stdin_over_the_prompt_limit_is_refused() {
    let rig = Rig::new();
    let args = [
        "run",
        "--provider",
        "codex",
        "--stdin",
        "--output",
        "json",
        "-C",
        &rig.work.to_string_lossy(),
    ]
    .map(String::from);
    let out = rig.with_stdin(&args, &"x".repeat(4 * 1024 * 1024 + 100));
    assert_eq!(out.status.code(), Some(2));
    let message = result_of(&out).error.expect("error").message;
    assert!(
        message.contains("prompt:") && message.contains("limit of 4194304"),
        "{message}"
    );
    assert!(!rig.record.join("argv").exists());
}

#[test]
fn wait_bounded_reads_the_pipes_while_it_waits() {
    // More output than a pipe holds, on both streams. Without reading while
    // waiting the child blocks on its own output and looks hung.
    let child = std::process::Command::new("/bin/sh")
        .args([
            "-c",
            "head -c 300000 /dev/zero | tr '\\0' x; head -c 300000 /dev/zero | tr '\\0' y >&2",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .guarded_spawn()
        .expect("spawn sh");
    let out = wait_bounded_for(child, std::time::Duration::from_secs(120));
    assert_eq!(out.stdout.len(), 300_000);
    assert_eq!(out.stderr.len(), 300_000);
}

#[test]
#[should_panic(expected = "did not exit within")]
fn wait_bounded_kills_a_child_that_does_not_exit_and_fails_the_test() {
    let child = std::process::Command::new("/bin/sleep")
        .arg("30")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .guarded_spawn()
        .expect("spawn sleep");
    let _ = wait_bounded_for(child, std::time::Duration::from_millis(200));
}

const CANARY: &str = "canary-value-must-not-appear-5521";

/// A command whose environment carries a canary in names of every kind: on
/// the allowlist, withheld, secret-looking, owned by each provider, and named.
fn canary_command(rig: &Rig) -> std::process::Command {
    let mut command = rig.command();
    for name in [
        "LC_ALL",
        "HTTPS_PROXY",
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "MY_SERVICE_API_TOKEN",
        "EDITOR",
        "JAVA_HOME",
        "DATABASE_URL",
        "NAMED_ONE",
        "NAMED_TOKEN",
    ] {
        command.env(name, CANARY);
    }
    command
}

fn seen_by_the_fake(rig: &Rig) -> Vec<String> {
    rig.recorded("env").lines().map(String::from).collect()
}

#[test]
fn only_the_allowlist_and_named_variables_reach_the_worker() {
    for provider in PROVIDERS {
        let rig = Rig::new();
        let out = canary_command(&rig)
            .args(rig.run_args(
                provider,
                "success",
                &[
                    "--env",
                    "NAMED_ONE",
                    "--env",
                    "NAMED_TOKEN",
                    "--env",
                    "NOT_SET_ANYWHERE",
                    "--output",
                    "json",
                ],
            ))
            .guarded_output()
            .expect("run delega");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{provider}: {}",
            text(&out.stderr)
        );
        let seen = seen_by_the_fake(&rig);
        let own = if provider == "claude" {
            ("ANTHROPIC_API_KEY", "OPENAI_API_KEY")
        } else {
            ("OPENAI_API_KEY", "ANTHROPIC_API_KEY")
        };
        for name in ["LC_ALL", "HTTPS_PROXY", own.0, "NAMED_ONE", "NAMED_TOKEN"] {
            assert!(
                seen.contains(&name.to_string()),
                "{provider} {name}: {seen:?}"
            );
        }
        for name in [
            own.1,
            "MY_SERVICE_API_TOKEN",
            "EDITOR",
            "JAVA_HOME",
            "DATABASE_URL",
            "NOT_SET_ANYWHERE",
            "DELEGA_CLAUDE_BIN",
            "DELEGA_CODEX_BIN",
        ] {
            assert!(
                !seen.contains(&name.to_string()),
                "{provider} {name}: {seen:?}"
            );
        }
        let result = result_of(&out);
        let notes = result.provider_notes.join("\n");
        assert!(
            notes.contains("environment variables of the caller were withheld;"),
            "{notes}"
        );
        assert!(
            notes.contains("NAMED_TOKEN") && notes.contains("looks like a secret"),
            "{notes}"
        );
        assert!(!notes.contains("NAMED_ONE") && !notes.contains("NOT_SET_ANYWHERE"));
        assert!(!text(&out.stdout).contains(CANARY));
        assert!(!text(&out.stderr).contains(CANARY));
        rig.assert_nothing_left();
    }
}

#[test]
fn a_dry_run_groups_the_names_counts_the_withheld_and_prints_no_value() {
    for provider in PROVIDERS {
        let rig = Rig::new();
        let args = rig.run_args(
            provider,
            "success",
            &[
                "--dry-run",
                "--env",
                &format!("SET_ONE={CANARY}"),
                "--env",
                "NAMED_ONE",
                "--env",
                "NOT_SET_ANYWHERE",
            ],
        );
        let out = canary_command(&rig)
            .args(&args)
            .guarded_output()
            .expect("run delega");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{provider}: {}",
            text(&out.stderr)
        );
        let shown = text(&out.stdout);
        for line in [
            "  base: ",
            "  network: HTTPS_PROXY",
            "  provider: ",
            "  named by the caller: FAKE_RECORD_DIR, NAMED_ONE",
            "  set by the caller: SET_ONE",
            "  set by delega: ",
            "  named but not set: NOT_SET_ANYWHERE",
            "  withheld from the caller's environment: ",
        ] {
            assert!(shown.contains(line), "{provider} {line:?} missing: {shown}");
        }
        assert!(shown.contains("LC_ALL"), "{shown}");
        for name in [
            "EDITOR",
            "JAVA_HOME",
            "DATABASE_URL",
            "MY_SERVICE_API_TOKEN",
        ] {
            assert!(
                !shown.contains(name),
                "withheld names are not shown: {name}"
            );
        }
        assert!(!shown.contains(CANARY));
        assert!(!text(&out.stderr).contains(CANARY));

        let mut json_args = args.clone();
        json_args.extend(["--output".into(), "json".into()]);
        let out = canary_command(&rig)
            .args(&json_args)
            .guarded_output()
            .expect("run delega");
        let value: Value = serde_json::from_slice(&out.stdout).expect("JSON");
        let env = &value["dry_run"]["env"];
        assert!(
            env["named"]
                .as_array()
                .unwrap()
                .contains(&"NAMED_ONE".into())
        );
        assert_eq!(env["named_not_set"][0], "NOT_SET_ANYWHERE");
        // Ten withheld: the other provider's key and config path, a token,
        // EDITOR, JAVA_HOME, DATABASE_URL, NAMED_TOKEN, the two provider
        // binary overrides and the test system root.
        assert_eq!(env["withheld_count"], 10, "{env}");
        assert!(!text(&out.stdout).contains(CANARY));
        assert!(!rig.record.join("argv").exists(), "{provider} was started");
    }
}

#[test]
fn a_bad_env_name_is_refused_before_start_and_its_text_is_not_echoed() {
    for provider in PROVIDERS {
        let rig = Rig::new();
        // The name part is bad in each: it starts with a digit, is empty, or
        // has a space. The value must not be echoed.
        for value in [
            format!("1BAD={CANARY}"),
            format!("={CANARY}"),
            format!("BAD NAME={CANARY}"),
        ] {
            let out = rig
                .command()
                .args(rig.run_args(provider, "success", &["--env", &value, "--output", "json"]))
                .guarded_output()
                .expect("run delega");
            assert_eq!(out.status.code(), Some(2), "{provider}");
            let result = result_of(&out);
            assert_eq!(result.outcome, Outcome::Refused);
            assert_eq!(result.error.expect("error").kind, "invalid_env_name");
            assert!(!text(&out.stdout).contains(CANARY));
            assert!(!text(&out.stderr).contains(CANARY));
            assert!(!rig.record.join("argv").exists(), "{provider} was started");
        }

        let out = rig
            .command()
            .args(rig.run_args(provider, "success", &["--env", "1ABC", "--dry-run"]))
            .guarded_output()
            .expect("run delega");
        assert_eq!(out.status.code(), Some(2), "{provider}");
        assert!(text(&out.stderr).contains("invalid_env_name"));
    }
}

#[test]
fn env_refuses_a_calling_session_or_delega_variable_in_either_form() {
    let rig = Rig::new();
    for (name, needle) in [
        ("CLAUDE_CODE_SESSION_ID", "calling session"),
        ("CLAUDECODE", "calling session"),
        ("DELEGA_CLAUDE_BIN", "configures delega itself"),
    ] {
        for given in [name.to_string(), format!("{name}=value")] {
            let out = rig
                .command()
                .args(rig.run_args("claude", "success", &["--env", &given, "--output", "json"]))
                .guarded_output()
                .expect("run delega");
            assert_eq!(out.status.code(), Some(2), "{name}");
            let error = result_of(&out).error.expect("error");
            assert_eq!(error.kind, "forbidden_env_name", "{name}");
            assert!(
                error.message.contains(name) && error.message.contains(needle),
                "{}",
                error.message
            );
        }
    }
    assert!(!rig.record.join("argv").exists());
}

#[test]
fn the_help_documents_env_and_a_profile_does_not_carry_it() {
    let rig = Rig::new();
    let help = text(&rig.delega(&["run", "--help"]).stdout);
    assert!(help.contains("-e, --env <NAME[=VALUE]>"), "{help}");
    assert!(help.contains("Prefer NAME for secrets"), "{help}");
    let file = profile(
        &rig,
        r#"{"schema_version":1,"provider":"claude","prompt":"success","pass_env":["X"]}"#,
    );
    let out = rig
        .command()
        .args(["run", "--request", &file, "-C"])
        .arg(&rig.work)
        .args(["--output", "json"])
        .guarded_output()
        .expect("run delega");
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    let message = result_of(&out).error.expect("error").message;
    assert!(message.contains("unknown field `pass_env`"), "{message}");
    assert!(!rig.record.join("argv").exists());
}

#[test]
fn env_with_a_value_sets_it_for_the_worker_whatever_the_caller_has() {
    for provider in PROVIDERS {
        let rig = Rig::new();
        // FAKE_VERSION is printed by the fake as its version, so the result
        // shows the value arrived. The value holds `=` and spaces.
        let out = rig
            .command()
            .env("FAKE_VERSION", "caller-value")
            .args(rig.run_args(
                provider,
                "success",
                &[
                    "-e",
                    "FAKE_VERSION=7.7.7 (build=x y)",
                    "-e",
                    "EMPTY_ONE=",
                    "--output",
                    "json",
                ],
            ))
            .guarded_output()
            .expect("run delega");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{provider}: {}",
            text(&out.stderr)
        );
        let seen = seen_by_the_fake(&rig);
        assert!(seen.contains(&"EMPTY_ONE".to_string()), "{seen:?}");
        let value: Value = serde_json::from_slice(&out.stdout).expect("JSON");
        assert_eq!(
            value["provenance"]["provider_version"], "7.7.7 (build=x y)",
            "{provider}"
        );
        rig.assert_nothing_left();
    }
}

#[test]
fn env_with_a_secret_looking_name_is_noted_in_both_forms_without_the_value() {
    let rig = Rig::new();
    let out = canary_command(&rig)
        .args(rig.run_args(
            "claude",
            "success",
            &[
                "-e",
                "NAMED_TOKEN",
                "-e",
                &format!("SET_API_TOKEN={CANARY}"),
                "--output",
                "json",
            ],
        ))
        .guarded_output()
        .expect("run delega");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let notes = result_of(&out).provider_notes.join("\n");
    assert!(notes.contains("NAMED_TOKEN was passed"), "{notes}");
    assert!(notes.contains("SET_API_TOKEN was set"), "{notes}");
    assert!(!text(&out.stdout).contains(CANARY));
    assert!(!text(&out.stderr).contains(CANARY));
}

/// Names that must never reach a worker, in the caller's environment with a
/// canary value each: session variables of both hosts, and ordinary variables
/// a reviewer found leaking when the policy was weakened.
const NEVER: [&str; 14] = [
    "CLAUDECODE",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_SSE_PORT",
    "CODEX_THREAD_ID",
    "CODEX_SANDBOX",
    "GIT_AUTHOR_NAME",
    "GIT_SSH_COMMAND",
    "SSH_AUTH_SOCK",
    "NODE_OPTIONS",
    "LC_SECRET_TOKEN",
    "EDITOR",
    "GH_TOKEN",
    "AWS_SECRET_ACCESS_KEY",
    "PROMPT_COMMAND",
];

#[test]
fn the_worker_receives_exactly_the_expected_names_and_no_others() {
    for provider in PROVIDERS {
        let rig = Rig::new();
        let mut command = rig.command();
        for name in NEVER {
            command.env(name, CANARY);
        }
        let allowed = [
            "LANG",
            "LANGUAGE",
            "LC_TIME",
            "TZ",
            "CI",
            "NO_COLOR",
            "ALL_PROXY",
        ];
        for name in allowed {
            command.env(name, CANARY);
        }
        command.env("ANTHROPIC_API_KEY", CANARY);
        command.env("OPENAI_API_KEY", CANARY);
        command.env("CODEX_HOME_NOT_THE_REAL_ONE", CANARY);
        let out = command
            .args(rig.run_args(provider, "success", &["-e", "NAMED_ONE", "-e", "SET_ONE=v"]))
            .env("NAMED_ONE", CANARY)
            .guarded_output()
            .expect("run delega");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{provider}: {}",
            text(&out.stderr)
        );
        // What the rig gives delega: PATH, HOME, XDG_CONFIG_HOME, the two
        // configuration directories and the record directory, which the
        // helper passes by name. The shell adds its own four.
        let mut expected: Vec<&str> = vec![
            "PATH",
            "HOME",
            "XDG_CONFIG_HOME",
            "FAKE_RECORD_DIR",
            "NAMED_ONE",
            "SET_ONE",
            // The keeper's run marker, which cleanup uses to find a
            // descendant that left the provider's session.
            "DELEGA_RUN_MARKER",
        ];
        expected.extend(allowed);
        if provider == "claude" {
            expected.extend([
                "CLAUDE_CONFIG_DIR",
                "ANTHROPIC_API_KEY",
                "CLAUDE_CODE_DISABLE_AUTO_MEMORY",
                "CLAUDE_CODE_DISABLE_BACKGROUND_TASKS",
                "CLAUDE_CODE_DISABLE_CLAUDE_MDS",
            ]);
        } else {
            // Any CODEX_* name belongs to Codex; Claude Code gets none of them.
            expected.extend([
                "CODEX_HOME",
                "OPENAI_API_KEY",
                "CODEX_HOME_NOT_THE_REAL_ONE",
            ]);
        }
        expected.sort_unstable();
        let mut seen = seen_by_the_fake(&rig);
        seen.retain(|n| !matches!(n.as_str(), "PWD" | "OLDPWD" | "SHLVL" | "_"));
        seen.sort();
        assert_eq!(seen, expected, "{provider}");
        rig.assert_nothing_left();
    }
}

#[test]
fn env_refuses_a_name_delega_sets_itself_instead_of_dropping_it() {
    let rig = Rig::new();
    for given in [
        "CLAUDE_CODE_DISABLE_BACKGROUND_TASKS",
        "CLAUDE_CODE_DISABLE_BACKGROUND_TASKS=0",
    ] {
        let out = rig
            .command()
            .args(rig.run_args("claude", "success", &["-e", given, "--output", "json"]))
            .guarded_output()
            .expect("run delega");
        assert_eq!(out.status.code(), Some(2), "{given}");
        let error = result_of(&out).error.expect("error");
        assert_eq!(error.kind, "forbidden_env_name", "{given}");
        assert!(error.message.contains("set by delega"), "{}", error.message);
    }
    assert!(!rig.record.join("argv").exists());
}

/// Whether every object in the value, at any depth, has its keys in
/// alphabetical order. `serde_json` is built here with `preserve_order` (the
/// MCP server's `tower-mcp` turns it on), so the order of the parsed value is
/// the order of the text.
fn keys_sorted(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => {
            let keys: Vec<&String> = map.keys().collect();
            keys.windows(2).all(|w| w[0] < w[1]) && map.values().all(keys_sorted)
        }
        serde_json::Value::Array(items) => items.iter().all(keys_sorted),
        _ => true,
    }
}

#[test]
fn the_dry_run_json_document_has_alphabetical_keys_in_every_build_configuration() {
    for provider in PROVIDERS {
        let rig = Rig::new();
        let out = rig.run(provider, "success", &["--dry-run", "--output", "json"]);
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        let document: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
        assert!(keys_sorted(&document), "{provider}: {document}");
        // The document is a value built with `json!`, which is where the order
        // would otherwise depend on the build.
        let first = text(&out.stdout);
        assert!(
            first.find("\"dry_run\"") < first.find("\"schema_version\""),
            "{first}"
        );
    }
}

#[test]
fn strict_refuses_codex_before_start_and_names_what_it_cannot_enforce() {
    let rig = Rig::new();
    let out = rig.run(
        "codex",
        "success",
        &["--strict", "--allow-command", "ls", "--output", "json"],
    );
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    let result = result_of(&out);
    assert_eq!(result.outcome, Outcome::Refused);
    let error = result.error.expect("error");
    assert_eq!(error.kind, "permissions_not_enforceable");
    for needle in [
        "reads are not confined",
        "commands that need no write access",
        "permissions.commands has no effect",
    ] {
        assert!(error.message.contains(needle), "{}", error.message);
    }
    assert!(!rig.record.join("argv").exists(), "codex was started");
    assert!(!rig.record.join("pid").exists());
    rig.assert_nothing_left();

    // Text output: the refusal is one line on standard error.
    let out = rig.run("codex", "success", &["--strict", "--write"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(text(&out.stdout), "");
    assert!(
        text(&out.stderr).contains("refused: permissions_not_enforceable: "),
        "{}",
        text(&out.stderr)
    );
    assert!(!rig.record.join("argv").exists());
}

#[test]
fn strict_dry_run_shows_the_same_refusal_and_starts_nothing() {
    let rig = Rig::new();
    let out = rig.run(
        "codex",
        "success",
        &["--strict", "--dry-run", "--output", "json"],
    );
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    let result = result_of(&out);
    assert_eq!(result.outcome, Outcome::Refused);
    assert_eq!(
        result.error.expect("error").kind,
        "permissions_not_enforceable"
    );
    assert!(!rig.record.join("argv").exists());
}

#[test]
fn strict_lets_full_runs_and_runs_that_name_no_unenforceable_part_go_ahead() {
    let rig = Rig::new();
    for provider in PROVIDERS {
        let out = rig.run(provider, "success", &["--strict", "--full"]);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{provider}: {}",
            text(&out.stderr)
        );
    }
    // Claude with no commands asks for nothing it cannot enforce.
    let out = rig.run("claude", "success", &["--strict"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    rig.assert_nothing_left();
}

#[test]
fn a_profile_with_strict_merges_and_refuses() {
    let rig = Rig::new();
    let file = profile(
        &rig,
        r#"{"schema_version":1,"provider":"codex","prompt":"success",
            "permissions":{"mode":"write","strict":true}}"#,
    );
    let out = rig
        .command()
        .args(["run", "--request", &file, "-C"])
        .arg(&rig.work)
        .args(["--output", "json"])
        .guarded_output()
        .expect("run delega");
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    let result = result_of(&out);
    assert_eq!(result.outcome, Outcome::Refused);
    assert_eq!(
        result.error.expect("error").kind,
        "permissions_not_enforceable"
    );
    assert!(!rig.record.join("argv").exists());
}

#[test]
fn cli_refuses_claude_with_managed_mcp_before_starting_the_provider() {
    let rig = Rig::new();
    let path = support::managed_mcp_path(&rig.system_root());
    std::fs::create_dir_all(path.parent().expect("managed directory")).expect("create directory");
    std::fs::write(&path, r#"{"mcpServers":{}}"#).expect("write managed MCP");
    let out = rig
        .command()
        .args(rig.run_args(
            "claude",
            "success",
            &["--output", "json", "--env", "FAKE_VERSION_SLEEP=0"],
        ))
        .guarded_output()
        .expect("run delega");
    assert_eq!(
        out.status.code(),
        Some(i32::from(Outcome::Refused.exit_status()))
    );
    let result = result_of(&out);
    assert_eq!(result.outcome, Outcome::Refused);
    let error = result.error.expect("refusal");
    assert_eq!(error.kind, "managed_mcp_exclusive");
    assert!(error.message.contains(&path.display().to_string()));
    assert!(rig.record.read_dir().expect("records").next().is_none());
}

#[test]
fn dry_run_refuses_claude_with_managed_mcp_before_starting_the_provider() {
    let rig = Rig::new();
    let path = support::managed_mcp_path(&rig.system_root());
    std::fs::create_dir_all(path.parent().expect("managed directory")).expect("create directory");
    for contents in ["[]", "null"] {
        std::fs::write(&path, contents).expect("write managed MCP");
        let out = rig.run(
            "claude",
            "success",
            &[
                "--dry-run",
                "--output",
                "json",
                "--env",
                "FAKE_VERSION_SLEEP=0",
            ],
        );
        assert_eq!(out.status.code(), Some(2));
        let result = result_of(&out);
        assert_eq!(result.outcome, Outcome::Refused);
        let error = result.error.expect("refusal");
        assert_eq!(error.kind, "managed_mcp_exclusive");
        assert!(error.message.contains(&path.display().to_string()));
        assert!(rig.record.read_dir().expect("records").next().is_none());
    }
}

#[test]
fn the_run_cli_refuses_live_asking_before_start_even_for_dry_run() {
    for provider in PROVIDERS {
        for dry_run in [false, true] {
            let rig = Rig::new();
            let file = profile(
                &rig,
                &format!(
                    r#"{{"schema_version":1,"provider":"{provider}","prompt":"success","permissions":{{"on_request":"ask"}}}}"#,
                ),
            );
            let mut command = rig.command();
            command
                .args(["run", "--request", &file, "-C"])
                .arg(&rig.work)
                .args(["--output", "json"]);
            if dry_run {
                command.arg("--dry-run");
            }
            let out = command.guarded_output().expect("run delega");
            assert_eq!(out.status.code(), Some(2));
            let result = result_of(&out);
            assert_eq!(result.outcome, Outcome::Refused);
            assert_eq!(result.error.unwrap().kind, "permissions_not_supported");
            assert!(result.timing.started_at.is_none());
            rig.assert_nothing_left();
        }
    }
}
