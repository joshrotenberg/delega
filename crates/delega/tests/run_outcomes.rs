//! `delega run` for every outcome and its exit status, against the fakes.

mod support;

use delega_core::contract::outcome::Outcome;
use delega_core::contract::result::Cleanup;
use support::{PROVIDERS, Rig, result_of, signal, text, wait_bounded};

#[test]
fn a_completed_run_prints_the_answer_alone_and_exits_zero() {
    for provider in PROVIDERS {
        let rig = Rig::new();
        let out = rig.run(provider, "success", &[]);
        assert_eq!(out.status.code(), Some(0), "{provider}");
        assert_eq!(text(&out.stdout), "fake final answer\n", "{provider}");
        assert_eq!(text(&out.stderr), "", "{provider}");
        rig.assert_nothing_left();
    }
}

#[test]
fn a_completed_run_in_json_prints_the_whole_result() {
    for provider in PROVIDERS {
        let rig = Rig::new();
        let out = rig.run(provider, "success", &["--output", "json"]);
        assert_eq!(out.status.code(), Some(0), "{provider}");
        let result = result_of(&out);
        assert_eq!(result.outcome, Outcome::Completed);
        assert_eq!(result.text, "fake final answer");
        assert_eq!(result.cleanup, Cleanup::Confirmed);
        assert_eq!(result.timing.timeout_secs, 600);
        assert!(result.session_id.is_some());
        assert!(result.usage.is_some());
        // The document is the only thing on standard output.
        let whole = text(&out.stdout);
        assert!(whole.trim_start().starts_with('{') && whole.ends_with("}\n"));
        rig.assert_nothing_left();
    }
}

#[test]
fn a_failed_run_exits_one_and_says_why_on_stderr_in_text_mode() {
    for provider in PROVIDERS {
        let rig = Rig::new();
        let out = rig.run(provider, "provider-error", &[]);
        assert_eq!(out.status.code(), Some(1), "{provider}");
        let err = text(&out.stderr);
        assert!(
            err.contains("failed: provider_error: Provider rejected the turn"),
            "{provider}: {err}"
        );
        rig.assert_nothing_left();
    }
}

#[test]
fn a_failed_run_in_json_is_the_result_with_exit_one() {
    for provider in PROVIDERS {
        let rig = Rig::new();
        let out = rig.run(provider, "exit-nonzero", &["--output", "json"]);
        assert_eq!(out.status.code(), Some(1), "{provider}");
        let result = result_of(&out);
        assert_eq!(result.outcome, Outcome::Failed);
        let error = result.error.expect("error");
        assert_eq!(error.kind, "provider_exit");
        assert_eq!(error.provider_exit_code, Some(3));
        rig.assert_nothing_left();
    }
}

#[test]
fn a_request_refused_before_start_exits_two_and_starts_nothing() {
    for provider in PROVIDERS {
        let rig = Rig::new();
        let missing = rig.path("no-such-directory");
        let missing = missing.to_string_lossy();
        let out = rig
            .command()
            .args([
                "run",
                "--provider",
                provider,
                "-p",
                "success",
                "-C",
                &missing,
                "--output",
                "json",
            ])
            .output()
            .expect("run delega");
        assert_eq!(out.status.code(), Some(2), "{provider}");
        let result = result_of(&out);
        assert_eq!(result.outcome, Outcome::Refused);
        assert_eq!(result.error.expect("error").kind, "cwd_unusable");
        assert!(!rig.record.join("argv").exists(), "{provider} was started");
    }
}

#[test]
fn a_missing_provider_executable_is_refused_naming_the_variable_to_set() {
    let rig = Rig::new();
    let out = rig
        .command()
        .env_remove("DELEGA_CODEX_BIN")
        .args(rig.run_args("codex", "success", &["--output", "json"]))
        .output()
        .expect("run delega");
    assert_eq!(out.status.code(), Some(2));
    let result = result_of(&out);
    let error = result.error.expect("error");
    assert_eq!(error.kind, "provider_not_found");
    assert!(
        error.message.contains("DELEGA_CODEX_BIN"),
        "{}",
        error.message
    );
}

#[test]
fn a_run_stopped_by_a_denial_is_refused_after_start_with_exit_two() {
    for provider in PROVIDERS {
        let rig = Rig::new();
        let out = rig.run(provider, "denied-refused", &["--output", "json"]);
        assert_eq!(out.status.code(), Some(2), "{provider}");
        let result = result_of(&out);
        assert_eq!(result.outcome, Outcome::Refused);
        assert_eq!(result.error.expect("error").kind, "permission_denied");
        assert_eq!(result.permissions.denied.len(), 1);
        assert!(result.timing.started_at.is_some(), "it did start");
        rig.assert_nothing_left();
    }
}

#[test]
fn a_denial_in_a_completed_run_is_listed_on_stderr_and_exits_zero() {
    for provider in PROVIDERS {
        let rig = Rig::new();
        let out = rig.run(provider, "denied-completed", &[]);
        assert_eq!(out.status.code(), Some(0), "{provider}");
        assert_eq!(text(&out.stdout), "done without the command\n");
        assert!(
            text(&out.stderr).contains("denied: ") && text(&out.stderr).contains("rm -rf"),
            "{provider}: {}",
            text(&out.stderr)
        );
        rig.assert_nothing_left();
    }
}

#[test]
fn a_run_past_its_time_limit_exits_124_and_leaves_nothing_running() {
    for provider in PROVIDERS {
        let rig = Rig::new();
        let out = rig.run(provider, "hang", &["--timeout", "1s", "--output", "json"]);
        assert_eq!(out.status.code(), Some(124), "{provider}");
        let result = result_of(&out);
        assert_eq!(result.outcome, Outcome::TimedOut);
        assert_eq!(result.timing.timeout_secs, 1);
        assert_eq!(result.cleanup, Cleanup::Confirmed);
        rig.assert_nothing_left();
    }
}

#[test]
fn a_timed_out_run_in_text_mode_says_so_on_stderr() {
    let rig = Rig::new();
    let out = rig.run("claude", "hang", &["--timeout", "1"]);
    assert_eq!(out.status.code(), Some(124));
    assert!(text(&out.stderr).contains("timed_out: timed_out:"));
    rig.assert_nothing_left();
}

/// Start `hang`, send `names` in turn once it is running, and return the result
/// after it exits.
fn interrupted(provider: &str, names: &[&str]) -> (std::process::Output, Rig) {
    let rig = Rig::new();
    let child = rig.spawn_run(provider, "hang", &["--output", "json"]);
    rig.wait_started();
    for name in names {
        signal(child.id(), name);
    }
    (wait_bounded(child), rig)
}

#[test]
fn sigint_stops_the_worker_prints_the_result_and_exits_130() {
    for provider in PROVIDERS {
        let (out, rig) = interrupted(provider, &["INT"]);
        assert_eq!(out.status.code(), Some(130), "{provider}");
        let result = result_of(&out);
        assert_eq!(result.outcome, Outcome::Interrupted);
        assert_eq!(result.cleanup, Cleanup::Confirmed);
        rig.assert_nothing_left();
    }
}

#[test]
fn sigterm_stops_the_worker_prints_the_result_and_exits_130() {
    for provider in PROVIDERS {
        let (out, rig) = interrupted(provider, &["TERM"]);
        assert_eq!(out.status.code(), Some(130), "{provider}");
        let result = result_of(&out);
        assert_eq!(result.outcome, Outcome::Interrupted);
        assert_eq!(result.cleanup, Cleanup::Confirmed);
        rig.assert_nothing_left();
    }
}

#[test]
fn signals_while_the_worker_is_stopping_do_not_end_delega_early() {
    // The fake ignores SIGTERM, so the stop takes the grace period. A second
    // and third signal arrive during it and must be ignored: delega still
    // prints the result and the worker is gone when it exits.
    for provider in PROVIDERS {
        let (out, rig) = interrupted(provider, &["INT", "TERM", "INT"]);
        assert_eq!(out.status.code(), Some(130), "{provider}");
        let result = result_of(&out);
        assert_eq!(result.outcome, Outcome::Interrupted);
        assert_eq!(result.cleanup, Cleanup::Confirmed);
        let err = text(&out.stderr);
        assert!(err.contains("stopping the worker"), "{err}");
        rig.assert_nothing_left();
    }
}

#[test]
fn an_interrupted_run_in_text_mode_exits_130_with_a_summary_on_stderr() {
    let rig = Rig::new();
    let child = rig.spawn_run("codex", "hang", &[]);
    rig.wait_started();
    signal(child.id(), "INT");
    let out = wait_bounded(child);
    assert_eq!(out.status.code(), Some(130));
    assert!(text(&out.stderr).contains("interrupted: interrupted:"));
    rig.assert_nothing_left();
}

#[test]
fn a_descendant_the_provider_leaves_behind_is_stopped_before_delega_exits() {
    for provider in PROVIDERS {
        let rig = Rig::new();
        let out = rig.run(provider, "descendant", &["--output", "json"]);
        assert_eq!(out.status.code(), Some(0), "{provider}");
        let result = result_of(&out);
        assert_eq!(result.cleanup, Cleanup::Confirmed);
        assert!(
            rig.recorded("child.pid").trim().parse::<i32>().is_ok(),
            "{provider}: the fake did not record its child"
        );
        rig.assert_nothing_left();
    }
}

#[test]
fn a_closed_stdout_pipe_does_not_panic_and_keeps_the_outcome_status() {
    for format in ["text", "json"] {
        for provider in PROVIDERS {
            let rig = Rig::new();
            let (reader, writer) = std::io::pipe().expect("pipe");
            drop(reader);
            let out = rig
                .command()
                .args(rig.run_args(provider, "success", &["--output", format]))
                .stdout(writer)
                .output()
                .expect("run delega");
            let err = text(&out.stderr);
            assert!(!err.contains("panicked"), "{provider} {format}: {err}");
            assert_eq!(out.status.code(), Some(0), "{provider} {format}: {err}");
            rig.assert_nothing_left();
        }
    }
}
