//! `delega run` for every outcome and its exit status, against the fakes.

mod support;

use delega_core::contract::outcome::Outcome;
use delega_core::contract::result::Cleanup;
use std::process::{Command, Stdio};
use std::time::Duration;
use support::{
    Guarded, PROVIDERS, Rig, assert_stop_cleanup, guarded_pipe, result_of, signal, text,
    wait_bounded, wait_exit_only, wait_for_ack,
};

#[test]
fn a_completed_run_prints_the_answer_alone_and_exits_zero() {
    for provider in PROVIDERS {
        let (rig, out) = Rig::run_with_timely_listing(provider, "success", &[]);
        assert_eq!(out.status.code(), Some(0), "{provider}");
        assert_eq!(text(&out.stdout), "fake final answer\n", "{provider}");
        assert_eq!(text(&out.stderr), "", "{provider}");
        rig.assert_nothing_left();
    }
}

#[test]
fn a_completed_run_in_json_prints_the_whole_result() {
    for provider in PROVIDERS {
        let (rig, out) = Rig::run_with_timely_listing(provider, "success", &["--output", "json"]);
        assert_eq!(out.status.code(), Some(0), "{provider}");
        let result = result_of(&out);
        assert_eq!(result.outcome, Outcome::Completed);
        assert_eq!(result.text, "fake final answer");
        assert_eq!(result.cleanup, Cleanup::Confirmed);
        assert_eq!(result.timing.timeout_secs, 600);
        assert!(result.session_id.is_some());
        // A run that started its session: its usage is the session's totals.
        assert!(result.usage.is_some());
        assert_eq!(result.session_usage, result.usage);
        // The document is the only thing on standard output.
        let whole = text(&out.stdout);
        let json: serde_json::Value = serde_json::from_str(&whole).expect("json");
        assert!(json["usage"].is_object(), "{provider}: {whole}");
        assert!(json["session_usage"].is_object(), "{provider}: {whole}");
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
            .guarded_output()
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
        .guarded_output()
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
        assert_stop_cleanup(&result, "stop");
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
        // Far enough apart that each one is a signal of its own.
        std::thread::sleep(Duration::from_millis(100));
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
        assert_stop_cleanup(&result, "stop");
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
        assert_stop_cleanup(&result, "stop");
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
        assert_stop_cleanup(&result, "stop");
        let err = text(&out.stderr);
        // Each message is said exactly once, the first by the first signal and
        // the second by a later one, so neither can satisfy the other's check.
        assert_eq!(
            err.matches("signal received, stopping the worker").count(),
            1,
            "{err}"
        );
        assert_eq!(err.matches("still stopping the worker").count(), 1, "{err}");
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
        let (rig, out) =
            Rig::run_with_timely_listing(provider, "descendant", &["--output", "json"]);
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
            let (reader, writer) = guarded_pipe().expect("pipe");
            drop(reader);
            let out = rig
                .command()
                .args(rig.run_args(provider, "success", &["--output", format]))
                .stdout(writer)
                .guarded_output()
                .expect("run delega");
            let err = text(&out.stderr);
            assert!(!err.contains("panicked"), "{provider} {format}: {err}");
            assert_eq!(out.status.code(), Some(0), "{provider} {format}: {err}");
            rig.assert_nothing_left();
        }
    }
}

#[test]
fn sighup_and_sigquit_stop_the_worker_like_sigterm() {
    for name in ["HUP", "QUIT"] {
        for provider in PROVIDERS {
            let (out, rig) = interrupted(provider, &[name]);
            assert_eq!(out.status.code(), Some(130), "{provider} {name}");
            let result = result_of(&out);
            assert_eq!(result.outcome, Outcome::Interrupted, "{provider} {name}");
            assert_stop_cleanup(&result, &format!("{provider} {name}"));
            rig.assert_nothing_left();
        }
    }
}

#[test]
fn a_signal_during_the_version_probe_stops_the_probe_and_starts_no_worker() {
    for provider in PROVIDERS {
        let rig = Rig::new();
        let child = rig
            .command()
            .env("FAKE_VERSION_SLEEP", "30")
            .args(rig.run_args(
                provider,
                "success",
                &["--output", "json", "--env", "FAKE_VERSION_SLEEP"],
            ))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .guarded_spawn()
            .expect("spawn delega");
        let began = std::time::Instant::now();
        while !rig.record.join("probe-started").exists() {
            assert!(
                began.elapsed() < Duration::from_secs(120),
                "the probe never started"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        signal(child.id(), "TERM");
        let out = wait_bounded(child);
        assert_eq!(out.status.code(), Some(130), "{provider}");
        let result = result_of(&out);
        assert_eq!(result.outcome, Outcome::Interrupted);
        assert_stop_cleanup(&result, "stop");
        assert!(
            !rig.record.join("argv").exists(),
            "{provider}: the worker started"
        );
        // The probe slept in a shell of its own; nothing of it remains.
        let ps = Command::new("/bin/ps")
            .args(["-ax", "-o", "command="])
            .captured()
            .guarded_output()
            .expect("ps");
        let root = rig.path("");
        let leftovers = text(&ps.stdout)
            .lines()
            .filter(|l| l.contains("fake-") && l.contains(root.to_string_lossy().as_ref()))
            .count();
        assert_eq!(leftovers, 0);
    }
}

#[test]
fn a_signal_at_the_moment_a_run_completes_still_gives_one_consistent_result() {
    // Signals at a spread of moments around a quick run's end. Whatever the
    // timing, the exit status matches the outcome that was printed, a run
    // killed before the handlers existed started nothing, and nothing is left.
    for delay_ms in [0u64, 20, 40, 60, 80, 100, 130, 160, 200, 260] {
        let rig = Rig::new();
        let child = rig.spawn_run("codex", "success", &["--output", "json"]);
        std::thread::sleep(Duration::from_millis(delay_ms));
        signal(child.id(), "TERM");
        let out = wait_bounded(child);
        match out.status.code() {
            Some(0) => assert_eq!(result_of(&out).outcome, Outcome::Completed, "{delay_ms}"),
            // 130 is either an interrupted run that printed its whole result, or
            // a result whose write the signal stopped. A stopped write leaves
            // less than the whole: nothing or a document cut off (an
            // end-of-input parse error, not any parse error; the document is
            // pretty-printed, so a cut can fall after one of its own newlines),
            // or a complete document whose closing newline was the part not
            // written. delega then says why on stderr. A complete document with
            // its newline is a whole result and must be interrupted.
            Some(130) => {
                let stopped =
                    text(&out.stderr).contains("signal received while writing the result");
                match serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                    Err(err) => assert!(
                        err.is_eof() && stopped,
                        "{delay_ms}: exit 130 with stdout that is neither a result nor a cut-off \
                         one from a stopped write: {err}: stdout {:?}, stderr {}",
                        text(&out.stdout),
                        text(&out.stderr)
                    ),
                    Ok(_) => {
                        let result = result_of(&out);
                        if out.stdout.ends_with(b"\n") {
                            assert_eq!(result.outcome, Outcome::Interrupted, "{delay_ms}");
                        } else {
                            assert!(
                                stopped,
                                "{delay_ms}: a result without its newline and no stopped write: {}",
                                text(&out.stderr)
                            );
                        }
                    }
                }
            }
            // The signal came before delega could handle it: nothing had started.
            // The signal came before delega had installed its handlers, which
            // happens before any worker is started. A worker that exists here
            // would be a worker nothing could have stopped.
            None => assert!(
                !rig.record.join("argv").exists() && !rig.record.join("pid").exists(),
                "{delay_ms}: delega was killed after a worker started"
            ),
            other => panic!(
                "{delay_ms}: unexpected status {other:?}: {}",
                text(&out.stderr)
            ),
        }
        rig.assert_nothing_left();
    }
}

// Uses the print thread's hold, which exists only in debug builds.
#[cfg(debug_assertions)]
#[test]
fn a_signal_after_the_result_is_written_keeps_the_outcome_exit_status() {
    for provider in PROVIDERS {
        let rig = Rig::new();
        let marker = rig.path("printed");
        // The print thread creates the marker once the whole result is written,
        // then holds before reporting its exit code: the signal lands in that
        // window every time.
        let mut child = Reaped(Some(
            rig.command()
                .args(rig.run_args(provider, "success", &["--output", "json"]))
                .env("DELEGA_TEST_HOLD_AFTER_PRINT", &marker)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .guarded_spawn()
                .expect("spawn delega"),
        ));
        let began = std::time::Instant::now();
        while !marker.exists() {
            assert!(
                began.elapsed() < Duration::from_secs(120),
                "{provider}: the result was never written"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        signal(child.id(), "TERM");
        let out = wait_bounded(child.take());
        assert_eq!(result_of(&out).outcome, Outcome::Completed, "{provider}");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{provider}: a written completed result must exit 0: {}",
            text(&out.stderr)
        );
        rig.assert_nothing_left();
    }
}

/// A child that is killed and reaped when the test fails before it was waited
/// for, so a failed assertion leaves no `delega` behind.
struct Reaped(Option<std::process::Child>);

impl Reaped {
    fn take(&mut self) -> std::process::Child {
        self.0.take().expect("the child was already taken")
    }
}

impl std::ops::Deref for Reaped {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        self.0.as_ref().expect("the child was already taken")
    }
}

impl std::ops::DerefMut for Reaped {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.as_mut().expect("the child was already taken")
    }
}

impl Drop for Reaped {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Fill `pipe` until it takes no more, without assuming its capacity (Linux
/// gives a pipe only two pages once the per-user pipe buffer soft limit is
/// exceeded). The write end is the test's own, so it is made non-blocking and
/// written until `EAGAIN`: first in pages, then a byte at a time to use up any
/// partly filled page. A macOS pipe grows when a write does not fit, until it
/// reaches its size limit; a non-blocking write grows it too, so `EAGAIN` comes
/// only once it can grow no more and a short line from `delega` cannot fit. The
/// write end is made blocking again before a child inherits it, so the child's
/// writes block rather than fail.
fn fill(pipe: &std::io::PipeWriter) {
    use rustix::io::{Errno, ioctl_fionbio, write};
    ioctl_fionbio(pipe, true).expect("make the pipe non-blocking");
    for piece in [&[b'.'; 4096][..], b"."] {
        loop {
            match write(pipe, piece) {
                Ok(_) | Err(Errno::INTR) => {}
                Err(Errno::AGAIN) => break,
                Err(err) => panic!("fill the pipe: {err}"),
            }
        }
    }
    ioctl_fionbio(pipe, false).expect("make the pipe blocking again");
}

/// Wait until `pipe` has something to read, failing the test past `limit`.
fn wait_readable(pipe: &impl std::os::fd::AsFd, limit: Duration) {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    let step = Timespec {
        tv_sec: 0,
        tv_nsec: 10_000_000,
    };
    let began = std::time::Instant::now();
    loop {
        let mut fds = [PollFd::new(pipe, PollFlags::IN)];
        match poll(&mut fds, Some(&step)) {
            Ok(n) if n > 0 => return,
            Ok(_) | Err(rustix::io::Errno::INTR) => {}
            Err(err) => panic!("poll the pipe: {err}"),
        }
        assert!(
            began.elapsed() < limit,
            "nothing was written within {limit:?}"
        );
    }
}

#[test]
fn a_signal_while_standard_output_is_stalled_exits_130_only_if_the_result_was_not_delivered() {
    // The case from the review: nobody reads standard output while delega
    // writes a large result, standard error is full so delega stays alive a
    // while after deciding its status, a signal arrives, and standard output is
    // drained later. Whatever the reader ends up with must agree with the exit
    // status: a whole `completed` result with 0, anything less with 130.
    for provider in PROVIDERS {
        let rig = Rig::new();
        let (stderr_reader, stderr_writer) = guarded_pipe().expect("a pipe");
        fill(&stderr_writer);
        let stop_requested = rig.path("stop-requested");
        let mut child = Reaped(Some(
            rig.command()
                .args(rig.run_args(provider, "large", &["--output", "json"]))
                .env("DELEGA_TEST_ACK_RESULT_STOP_REQUESTED", &stop_requested)
                .stdout(Stdio::piped())
                .stderr(Stdio::from(stderr_writer))
                .guarded_spawn()
                .expect("spawn delega"),
        ));
        let stdout = child.stdout.take().expect("stdout");
        // The result is written only once the worker is gone, so delega is now
        // writing it, and blocks once the pipe is full.
        wait_readable(&stdout, Duration::from_secs(120));
        signal(child.id(), "TERM");
        // delega has taken the signal and asked the write to stop; the reader
        // then drains while delega is still alive, held by its full standard
        // error.
        wait_for_ack(
            &stop_requested,
            &mut child,
            "the signal asked the result write to stop",
        );
        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = std::io::Read::read_to_end(&mut { stdout }, &mut bytes);
            bytes
        });
        let status = wait_exit_only(&mut child, Duration::from_secs(90));
        let delivered = reader.join().expect("stdout reader");
        drop(child);
        let stderr = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = std::io::Read::read_to_end(&mut { stderr_reader }, &mut bytes);
            bytes
        });
        let stderr = stderr.join().expect("stderr reader");
        let result: Option<delega_core::contract::result::RunResult> =
            serde_json::from_slice(&delivered).ok();
        match (status.code(), result) {
            (Some(0), Some(result)) => assert_eq!(result.outcome, Outcome::Completed, "{provider}"),
            (Some(130), None) => {}
            (code, result) => panic!(
                "{provider}: exit {code:?} with {} bytes, a whole result: {}; stderr ends: {}",
                delivered.len(),
                result.is_some(),
                text(&stderr[stderr.len().saturating_sub(500)..]).trim_start_matches('.')
            ),
        }
        rig.assert_nothing_left();
    }
}

#[test]
fn a_blocked_standard_error_does_not_delay_stopping_the_worker() {
    for provider in PROVIDERS {
        let rig = Rig::new();
        // The fake writes about 10000 progress events, far more than a pipe
        // holds, and nobody reads standard error.
        let mut child = rig.spawn_run(provider, "chatty-hang", &["--progress", "--output", "json"]);
        rig.wait_started();
        std::thread::sleep(Duration::from_millis(500));
        signal(child.id(), "TERM");
        let status = wait_exit_only(&mut child, Duration::from_secs(90));
        assert_eq!(status.code(), Some(130), "{provider}");
        rig.assert_nothing_left();
        // The result went to standard output without waiting for standard error.
        let mut stdout = String::new();
        std::io::Read::read_to_string(&mut child.stdout.take().expect("stdout"), &mut stdout)
            .expect("read stdout");
        let result: delega_core::contract::result::RunResult =
            serde_json::from_str(&stdout).expect("a result document");
        assert_eq!(result.outcome, Outcome::Interrupted);
        assert_stop_cleanup(&result, "stop");
    }
}

#[test]
fn text_mode_prints_an_answer_only_for_a_completed_run() {
    for provider in PROVIDERS {
        for (prompt, extra, code) in [
            ("provider-error", &[][..], 1),
            ("denied-refused", &[][..], 2),
            ("hang", &["--timeout", "1"][..], 124),
        ] {
            let rig = Rig::new();
            let out = rig.run(provider, prompt, extra);
            assert_eq!(out.status.code(), Some(code), "{provider} {prompt}");
            assert_eq!(
                text(&out.stdout),
                "",
                "{provider} {prompt}: {}",
                text(&out.stdout)
            );
            assert!(!text(&out.stderr).is_empty(), "{provider} {prompt}");
            rig.assert_nothing_left();
        }
        let rig = Rig::new();
        let child = rig.spawn_run(provider, "hang", &[]);
        rig.wait_started();
        signal(child.id(), "INT");
        let out = wait_bounded(child);
        assert_eq!(out.status.code(), Some(130));
        assert_eq!(text(&out.stdout), "", "{provider} interrupted");
        rig.assert_nothing_left();
    }
}

#[test]
fn partial_text_stays_available_in_json_for_a_run_that_did_not_complete() {
    let rig = Rig::new();
    let out = rig.run("claude", "provider-error", &["--output", "json"]);
    let result = result_of(&out);
    assert_eq!(result.outcome, Outcome::Failed);
    assert_eq!(result.text, "working");
}

#[test]
fn an_unwritable_standard_output_is_a_write_failure_not_a_silent_success() {
    for format in ["text", "json"] {
        for provider in PROVIDERS {
            let rig = Rig::new();
            // `1</dev/null` makes descriptor 1 a read-only file, so every write to it
            // fails with EBADF. (A closed descriptor 1 would be reopened as
            // /dev/null by the Rust runtime before delega's code runs.)
            let mut command = Command::new("/bin/sh");
            rig.configure(&mut command);
            let out = command
                .args([
                    "-c",
                    "exec \"$0\" \"$@\" 1</dev/null",
                    env!("CARGO_BIN_EXE_delega"),
                ])
                .args(rig.run_args(provider, "success", &["--output", format]))
                .stderr(Stdio::piped())
                .guarded_output()
                .expect("run delega");
            let err = text(&out.stderr);
            assert!(!err.contains("panicked"), "{provider} {format}: {err}");
            assert_eq!(out.status.code(), Some(1), "{provider} {format}: {err}");
            assert!(
                err.contains("cannot write to stdout"),
                "{provider} {format}: {err}"
            );
            rig.assert_nothing_left();
        }
    }
}

#[test]
fn a_signal_ends_delega_that_is_only_waiting_for_a_reader_of_a_large_result() {
    for provider in PROVIDERS {
        let rig = Rig::new();
        // The fake answers with 3 MB; nobody reads standard output, so delega
        // blocks writing the result after the worker is gone.
        let blocked = rig.path("result-blocked");
        let mut child = rig
            .command()
            .args(rig.run_args(provider, "large", &["--output", "json"]))
            .env("DELEGA_TEST_ACK_RESULT_BLOCKED", &blocked)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .guarded_spawn()
            .expect("spawn delega");
        // The result is written only once the worker is gone.
        wait_for_ack(
            &blocked,
            &mut child,
            "writing the result waits for a reader",
        );
        rig.assert_nothing_left();
        assert!(
            child.try_wait().expect("poll").is_none(),
            "{provider}: delega should still be waiting to write"
        );
        signal(child.id(), "TERM");
        let status = wait_exit_only(&mut child, Duration::from_secs(90));
        assert_eq!(status.code(), Some(130), "{provider}");
        rig.assert_nothing_left();
    }
}

#[test]
fn every_terminal_signal_ends_a_delega_blocked_writing_its_result() {
    for name in ["INT", "HUP", "QUIT"] {
        let rig = Rig::new();
        let blocked = rig.path("result-blocked");
        let mut child = rig
            .command()
            .args(rig.run_args("claude", "large", &["--output", "json"]))
            .env("DELEGA_TEST_ACK_RESULT_BLOCKED", &blocked)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .guarded_spawn()
            .expect("spawn delega");
        wait_for_ack(
            &blocked,
            &mut child,
            "writing the result waits for a reader",
        );
        signal(child.id(), name);
        let status = wait_exit_only(&mut child, Duration::from_secs(90));
        assert_eq!(status.code(), Some(130), "{name}");
        rig.assert_nothing_left();
    }
}

#[test]
fn dropped_progress_is_counted_in_a_final_line_and_in_the_result_notes() {
    for provider in PROVIDERS {
        let (rig, out) =
            Rig::run_with_timely_listing(provider, "chatty", &["--progress", "--output", "json"]);
        assert_eq!(out.status.code(), Some(0), "{provider}");
        let result = result_of(&out);
        let err = text(&out.stderr);
        let shown = err
            .lines()
            .filter(|l| l.contains(": message: 1 chars"))
            .count();
        let notice = err.lines().find(|l| l.contains("were dropped"));
        let noted = result
            .provider_notes
            .iter()
            .any(|n| n.contains("chunks of provider output were not reported"));
        // 20000 events plus one the fake's own opening line: whatever did not
        // arrive as a line is announced, by the last line on stderr, and when
        // the loss was at the output tap also in the result.
        if shown < 20_000 {
            let notice = notice.unwrap_or_else(|| panic!("{provider}: {shown} lines, no notice"));
            assert!(notice.starts_with("progress: "), "{notice}");
            assert_eq!(err.lines().last(), Some(notice), "the notice comes last");
        } else {
            assert!(notice.is_none(), "{provider}: nothing was lost");
            assert!(!noted);
        }
        rig.assert_nothing_left();
    }
}

#[test]
fn a_resume_that_continues_another_session_fails_with_session_mismatch() {
    for provider in PROVIDERS {
        let rig = Rig::new();
        let out = rig
            .command()
            .env("FAKE_RESUMED_SESSION_ID", "other-session")
            .args(rig.run_args(
                provider,
                "success",
                &[
                    "--resume",
                    "asked-session",
                    "--output",
                    "json",
                    "--env",
                    "FAKE_RESUMED_SESSION_ID",
                ],
            ))
            .guarded_output()
            .expect("run delega");
        assert_eq!(out.status.code(), Some(1), "{provider}");
        let result = result_of(&out);
        assert_eq!(result.outcome, Outcome::Failed, "{provider}");
        assert_eq!(result.error.expect("error").kind, "session_mismatch");
        assert_eq!(result.text, "fake final answer", "{provider}");
        assert_eq!(result.session_id.as_deref(), Some("other-session"));
        rig.assert_nothing_left();
    }
}

#[test]
fn a_resume_of_a_session_the_provider_cannot_find_fails_with_session_not_found() {
    for provider in PROVIDERS {
        let rig = Rig::new();
        let out = rig.run(
            provider,
            "unknown-session",
            &["--resume", "missing-session", "--output", "json"],
        );
        assert_eq!(out.status.code(), Some(1), "{provider}");
        let result = result_of(&out);
        assert_eq!(result.outcome, Outcome::Failed, "{provider}");
        let error = result.error.expect("error");
        assert_eq!(error.kind, "session_not_found", "{provider}");
        assert!(
            error.message.contains("missing-session"),
            "{}",
            error.message
        );
        rig.assert_nothing_left();
    }
}

#[test]
fn an_unknown_model_fails_with_provider_model_unavailable_and_exits_one() {
    for (provider, mode) in [
        ("claude", "model-not-found"),
        ("codex", "model-unsupported"),
    ] {
        let rig = Rig::new();
        let out = rig.run(provider, mode, &["--output", "json"]);
        assert_eq!(out.status.code(), Some(1), "{provider}");
        let result = result_of(&out);
        assert_eq!(result.outcome, Outcome::Failed, "{provider}");
        let error = result.error.expect("error");
        assert_eq!(error.kind, "provider_model_unavailable", "{provider}");
        assert!(error.message.contains("no-such-model"), "{}", error.message);
        rig.assert_nothing_left();
    }
}
