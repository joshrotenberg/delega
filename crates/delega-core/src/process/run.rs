//! The owner loop: start one child, follow it to an ending, settle cleanup,
//! and build the single report.

use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::contract::result::Cleanup;

use super::capture::{Bounded, Captured};
use super::limits::{FINAL_CLEANUP_PASS, STDERR_READ_CHUNK, STDOUT_READ_CHUNK};
use super::owner::{OwnedProcess, Settled};
use super::report::{Ending, LeaderExit, ProcessReport, ProcessTiming};
use super::spec::ProcessSpec;
use super::sys::Signal;

/// Warning recorded when the owner closed pipes that something still held.
pub const PIPE_WARNING: &str =
    "Output pipes remained open after process-group cleanup; the owner closed its ends";

fn command(spec: &ProcessSpec) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(&spec.program);
    command.args(&spec.args);
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    command.env_clear();
    command.envs(spec.env.resolve(std::env::vars_os()));
    command
        .stdin(if spec.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// Run one child to its single terminal report. Owns the child throughout; if
/// this future is dropped or panics, the owner's `Drop` sends SIGKILL to the
/// child's group.
pub(super) async fn run_owned(
    spec: ProcessSpec,
    stop: CancellationToken,
    pid_slot: Arc<AtomicU32>,
) -> ProcessReport {
    let started_wall = SystemTime::now();
    let started = Instant::now();
    let mut command = command(&spec);
    let mut owner = match OwnedProcess::spawn(&mut command, &spec) {
        Ok(owner) => owner,
        Err(error) => return failed_to_start(&spec, error),
    };
    let shared = owner.shared.clone();
    pid_slot.store(u32::try_from(shared.pid).unwrap_or(0), Ordering::SeqCst);

    let Some(mut exit_rx) = owner.exit_rx.take() else {
        // The exit watcher could not start. Stop the child and say so.
        let message = format!(
            "the exit watcher could not be started: {}",
            owner.watch_error.clone().unwrap_or_default()
        );
        let settled = owner
            .cleanup(
                spec.stop_grace,
                Instant::now() + spec.cleanup_deadline,
                true,
                None,
            )
            .await;
        return build_report(
            &spec,
            Some(shared.pid),
            Some(Ending::WaitFailed { message }),
            settled,
            Captured::default(),
            Captured::default(),
            started_wall,
            started,
            &shared,
        );
    };

    let child = owner.child.as_mut().expect("a new owner holds its child");
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    if let (Some(content), Some(mut stdin)) = (spec.stdin.clone(), child.stdin.take()) {
        let task_shared = shared.clone();
        owner.stdin_task = Some(tokio::spawn(async move {
            let result = async {
                stdin.write_all(&content).await?;
                stdin.shutdown().await
            }
            .await;
            if let Err(error) = result
                && error.kind() != std::io::ErrorKind::BrokenPipe
            {
                task_shared.warn(format!("Standard input write failed: {error}"));
            }
        }));
    }

    let mut out = Bounded::new(spec.stdout);
    let mut err = Bounded::new(spec.stderr);
    let mut out_buf = vec![0u8; STDOUT_READ_CHUNK];
    let mut err_buf = vec![0u8; STDERR_READ_CHUNK];
    let limit_at = started + spec.time_limit;
    let far = || Instant::now() + Duration::from_secs(86_400);

    let mut ending: Option<Ending> = None;
    let mut exited = false;
    let mut stop_seen = false;
    let mut limit_seen = false;
    let mut kill_sent = false;
    let mut term_at: Option<Instant> = None;
    let mut drain_deadline: Option<Instant> = None;
    let mut watch_failed = false;

    loop {
        if exited && stdout.is_none() && stderr.is_none() {
            break;
        }
        tokio::select! {
            _ = stop.cancelled(), if !stop_seen => {
                stop_seen = true;
                if !exited && term_at.is_none() && !shared.leader_has_exited() {
                    ending = Some(Ending::Interrupted);
                    shared.before_stop().await;
                    term_at = Some(Instant::now());
                    shared.signal_group(Signal::TERM);
                }
            },
            _ = tokio::time::sleep_until(limit_at), if !limit_seen && !exited => {
                limit_seen = true;
                if term_at.is_none() && !shared.leader_has_exited() {
                    ending = Some(Ending::TimedOut { limit: spec.time_limit });
                    shared.before_stop().await;
                    term_at = Some(Instant::now());
                    shared.signal_group(Signal::TERM);
                }
            },
            _ = tokio::time::sleep_until(term_at.map(|t| t + spec.stop_grace).unwrap_or_else(far)),
                if term_at.is_some() && !kill_sent => {
                shared.signal_group(Signal::KILL);
                kill_sent = true;
                drain_deadline = Some(Instant::now() + spec.drain_bound);
            },
            result = &mut exit_rx, if !exited => {
                exited = true;
                if !matches!(result, Ok(Ok(()))) {
                    watch_failed = true;
                    shared.warn("Leader exit observation failed".into());
                }
                // A natural exit begins cleanup of the rest of the group
                // without changing how the run is reported.
                if term_at.is_none() {
                    term_at = Some(Instant::now());
                    shared.signal_group(Signal::TERM);
                }
            },
            n = async { stdout.as_mut().expect("guarded").read(&mut out_buf).await },
                if stdout.is_some() => match n {
                Ok(0) | Err(_) => stdout = None,
                Ok(n) => out.push(&out_buf[..n]),
            },
            n = async { stderr.as_mut().expect("guarded").read(&mut err_buf).await },
                if stderr.is_some() => match n {
                Ok(0) | Err(_) => stderr = None,
                Ok(n) => err.push(&err_buf[..n]),
            },
            _ = tokio::time::sleep_until(drain_deadline.unwrap_or_else(far)),
                if drain_deadline.is_some() && exited => {
                stdout = None;
                stderr = None;
                shared.note_pipes_closed();
                shared.warn(PIPE_WARNING.into());
            },
            _ = tokio::time::sleep_until(
                    term_at.map(|t| t + spec.cleanup_deadline).unwrap_or_else(far)),
                if term_at.is_some() => {
                // Nothing may keep the owner here without bound: report what
                // is known, with cleanup unconfirmed if it is.
                break;
            },
        }
    }
    if !exited {
        owner.exit_rx = Some(exit_rx);
    }
    drop(stdout);
    drop(stderr);
    let deadline = term_at.map_or_else(Instant::now, |t| t + spec.cleanup_deadline);
    let deadline = deadline.max(Instant::now() + FINAL_CLEANUP_PASS.min(spec.cleanup_deadline));
    let settled = owner
        .cleanup(spec.stop_grace, deadline, false, term_at)
        .await;
    let ending = if watch_failed && ending.is_none() {
        Some(Ending::WaitFailed {
            message: "the leader's exit could not be observed".into(),
        })
    } else {
        ending
    };
    build_report(
        &spec,
        Some(shared.pid),
        ending,
        settled,
        out.finish(),
        err.finish(),
        started_wall,
        started,
        &shared,
    )
}

#[allow(clippy::too_many_arguments)]
fn build_report(
    spec: &ProcessSpec,
    pid: Option<i32>,
    ending: Option<Ending>,
    settled: Settled,
    stdout: Captured,
    stderr: Captured,
    started_wall: SystemTime,
    started: Instant,
    shared: &super::owner::Shared,
) -> ProcessReport {
    let Settled { exit, cleanup } = settled;
    let ending = ending.unwrap_or_else(|| match &exit {
        Ok(LeaderExit {
            code: Some(code), ..
        }) => Ending::Exited { code: *code },
        Ok(LeaderExit {
            signal: Some(signal),
            signal_name,
            ..
        }) => Ending::Signaled {
            signal: *signal,
            name: signal_name.clone().unwrap_or_default(),
        },
        Ok(_) => Ending::WaitFailed {
            message: "the exit status carried neither a code nor a signal".into(),
        },
        Err(message) => Ending::WaitFailed {
            message: message.clone(),
        },
    });
    ProcessReport {
        pid: pid.and_then(|p| u32::try_from(p).ok()),
        ending,
        exit: exit.ok(),
        stdout,
        stderr,
        timing: ProcessTiming {
            started_at: Some(started_wall),
            finished_at: SystemTime::now(),
            duration: started.elapsed(),
            time_limit: spec.time_limit,
        },
        cleanup,
        diagnostics: shared.diagnostics(),
    }
}

fn failed_to_start(spec: &ProcessSpec, error: std::io::Error) -> ProcessReport {
    let now = SystemTime::now();
    let program = spec.program.to_string_lossy();
    ProcessReport {
        pid: None,
        ending: Ending::FailedToStart {
            message: format!("could not start {program}: {error}"),
        },
        exit: None,
        stdout: Captured::default(),
        stderr: Captured::default(),
        timing: ProcessTiming {
            started_at: None,
            finished_at: now,
            duration: Duration::ZERO,
            time_limit: spec.time_limit,
        },
        // Nothing was started, so nothing can remain.
        cleanup: Cleanup::Confirmed,
        diagnostics: super::report::Diagnostics {
            warnings: Vec::new(),
            signals: Vec::new(),
            signals_omitted: 0,
            tracking_enabled: spec.track_descendants,
            inspection: Default::default(),
            capture_gap_observed: false,
            post_exit_group_signal: false,
            pipes_closed_by_owner: false,
            unobserved_descendants_possible: false,
        },
    }
}
