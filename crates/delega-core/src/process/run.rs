//! The owner loop: start one child, follow it to an ending, settle cleanup,
//! and build the single report.

use std::panic::AssertUnwindSafe;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime};

use futures_util::FutureExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::Instant;

use crate::contract::result::Cleanup;

use super::capture::{Bounded, Captured};
use super::dialogue::End;
use super::handle::Stop;
use super::limits::{FAR_FUTURE, STDERR_READ_CHUNK, STDOUT_READ_CHUNK};
use super::owner::{OwnedProcess, Registry, Settled, Shared, SpawnFailure};
use super::report::{Ending, LeaderExit, ProcessReport, ProcessTiming};
use super::spec::ProcessSpec;
use super::sys::Signal;
use super::tap::TapWorker;

const MAX_DIALOGUE_FRAME_BYTES: usize = 4 * 1024 * 1024;
const DIALOGUE_STOP_WRITE_BUDGET: Duration = Duration::from_millis(50);

/// Warning recorded when the owner closed pipes that something still held.
pub const PIPE_WARNING: &str =
    "Output pipes remained open after process-group cleanup; the owner closed its ends";

pub(super) fn command(spec: &ProcessSpec) -> tokio::process::Command {
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

/// What the owner loop concluded, before cleanup.
struct Driven {
    ending: Option<Ending>,
    stdout: Captured,
    stderr: Captured,
    term_at: Option<Instant>,
    /// Kill at once instead of the polite stop first.
    hard_stop: bool,
}

/// Run one child to its single terminal report. Owns the child throughout. The
/// loop runs inside a panic boundary and the owner lives outside it, so a panic
/// in the loop is followed by a hard stop and a report, as an error would be. If
/// this future is dropped instead, the owner's `Drop` sends SIGKILL to the
/// child's group and to the descendants seen at the latest scan.
pub(super) async fn run_owned(
    spec: ProcessSpec,
    stop: Stop,
    registry: Arc<Registry>,
) -> ProcessReport {
    let started_wall = SystemTime::now();
    let started = Instant::now();
    let mut owner = match OwnedProcess::spawn(&spec, &registry).await {
        Ok(owner) => owner,
        Err(SpawnFailure::Program(error)) => {
            let program = spec.program.to_string_lossy();
            return failed_to_start(&spec, format!("could not start {program}: {error}"));
        }
        Err(SpawnFailure::Keeper(why)) => return failed_to_start(&spec, why),
    };
    let shared = owner.shared.clone();
    registry
        .pid
        .store(u32::try_from(shared.pid).unwrap_or(0), Ordering::SeqCst);
    let cleanup_deadline = spec.effective_cleanup_deadline();
    if cleanup_deadline > spec.cleanup_deadline {
        shared.warn(format!(
            "cleanup_deadline {:?} is shorter than stop_grace plus drain_bound plus the margin; \
             {cleanup_deadline:?} was used",
            spec.cleanup_deadline
        ));
    }

    let tap = spec.stdout_tap.clone().map(|tap| {
        TapWorker::start(
            tap,
            spec.stdout_tap_dropped.clone(),
            spec.timings.tap_queue_chunks,
        )
    });
    let driven = AssertUnwindSafe(drive(
        &mut owner,
        &spec,
        &stop,
        &shared,
        started,
        cleanup_deadline,
        tap.as_ref(),
    ))
    .catch_unwind()
    .await;
    let driven = match driven {
        Ok(driven) => driven,
        Err(_) => Driven {
            ending: Some(Ending::WaitFailed {
                message: "the owner loop panicked; the child was stopped".into(),
            }),
            stdout: Captured::default(),
            stderr: Captured::default(),
            term_at: None,
            hard_stop: true,
        },
    };
    let deadline = driven
        .term_at
        .map_or_else(Instant::now, |t| t + cleanup_deadline);
    let deadline =
        deadline.max(Instant::now() + spec.timings.final_cleanup_pass.min(cleanup_deadline));
    let settled = owner
        .cleanup(
            spec.stop_grace,
            deadline,
            driven.hard_stop,
            driven.term_at,
            spec.timings.stop_listing,
        )
        .await;
    if let Some(tap) = tap {
        tap.finish(&shared).await;
    }
    build_report(
        &spec,
        Some(shared.pid),
        driven.ending,
        settled,
        driven.stdout,
        driven.stderr,
        started_wall,
        started,
        &shared,
    )
}

/// Follow the child to an ending: its output, the time limit, a stop request
/// and its exit. Returns before cleanup.
async fn drive(
    owner: &mut OwnedProcess,
    spec: &ProcessSpec,
    stop: &Stop,
    shared: &Arc<Shared>,
    started: Instant,
    cleanup_deadline: Duration,
    tap: Option<&TapWorker>,
) -> Driven {
    let Some(mut exit_rx) = owner.exit_rx.take() else {
        // The exit watcher could not start. Stop the child and say so.
        let message = format!(
            "the exit watcher could not be started: {}",
            owner.watch_error.clone().unwrap_or_default()
        );
        return Driven {
            ending: Some(Ending::WaitFailed { message }),
            stdout: Captured::default(),
            stderr: Captured::default(),
            term_at: None,
            hard_stop: true,
        };
    };

    let io = owner
        .io
        .take()
        .expect("a new owner holds its child's streams");
    let mut stdout = io.stdout;
    let mut stderr = io.stderr;
    let dialogue = spec.dialogue.as_ref();
    let limit_at = started + spec.time_limit;
    if let Some(dialogue) = dialogue {
        dialogue.started(limit_at);
    }
    let mut pending_line = Vec::new();
    if let (Some(content), Some(mut stdin)) = (spec.stdin.clone(), io.stdin) {
        let task_shared = shared.clone();
        let dialogue = dialogue.cloned();
        owner.stdin_task = Some(tokio::spawn(async move {
            let result = async {
                stdin.write_all(&content).await?;
                if let Some(dialogue) = &dialogue {
                    loop {
                        let notified = dialogue.changed().notified();
                        let (responses, close) = dialogue.responses();
                        for (key, bytes) in responses {
                            stdin.write_all(&bytes).await?;
                            dialogue.written(key);
                        }
                        if close {
                            break;
                        }
                        notified.await;
                    }
                }
                stdin.shutdown().await
            }
            .await;
            if result.is_err()
                && let Some(dialogue) = dialogue
            {
                dialogue.end(End::ProviderClosed);
            }
            if let Err(error) = result
                && error.kind() != std::io::ErrorKind::BrokenPipe
            {
                task_shared.warn(format!("Standard input write failed: {error}"));
            }
        }));
    }
    #[cfg(test)]
    if spec.panic_in_loop {
        panic!("injected owner loop panic");
    }

    let mut out = Bounded::new(spec.stdout);
    let mut err = Bounded::new(spec.stderr);
    let mut out_buf = vec![0u8; STDOUT_READ_CHUNK];
    let mut err_buf = vec![0u8; STDERR_READ_CHUNK];
    let far = || Instant::now() + FAR_FUTURE;

    let mut ending: Option<Ending> = None;
    let mut exited = false;
    let mut stop_seen = false;
    let mut limit_seen = false;
    let mut kill_sent = false;
    let mut term_at: Option<Instant> = None;
    let mut drain_deadline: Option<Instant> = None;
    let mut watch_failed = false;
    // The listing that must precede the first signal after a natural exit. It
    // runs beside the reads so a wait for it never stalls the output.
    let mut exit_listing: Option<std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>> =
        None;
    // The TERM sweep of the stop when its first attempt ran out of budget. It
    // runs beside the reads, so a slow listing never stalls the output, and it
    // is dropped before the KILL so it holds no lock the KILL sweep needs.
    let mut term_retry: Option<std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>> =
        None;
    // The same for the KILL sweep after the grace.
    let mut kill_retry: Option<std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>> =
        None;

    loop {
        if exited && stdout.is_none() && stderr.is_none() {
            break;
        }
        tokio::select! {
            _ = stop.stop.cancelled(), if !stop_seen => {
                stop_seen = true;
                if let Some(dialogue) = dialogue {
                    dialogue.end(End::Interrupted);
                    if !exited && term_at.is_none() && !shared.leader_has_exited() {
                        ending = Some(Ending::Interrupted);
                    }
                    finish_dialogue_write(owner).await;
                }
                if !exited && term_at.is_none() && !shared.leader_has_exited() {
                    let listed = shared.before_stop(spec.timings.stop_listing, &stop.hurry).await;
                    // The child may have exited while the scan ran. Then it
                    // ended on its own and is reported as it exited.
                    if !shared.leader_has_exited() {
                        if !listed {
                            shared.note_listing_missed();
                        }
                        ending = Some(Ending::Interrupted);
                        term_at = Some(Instant::now());
                        term_retry = retry_for(shared, Signal::TERM, shared.terminate().await);
                    }
                }
            },
            _ = tokio::time::sleep_until(limit_at), if !limit_seen && !exited => {
                limit_seen = true;
                if let Some(dialogue) = dialogue {
                    dialogue.end(End::TimedOut);
                    if !exited && term_at.is_none() && !shared.leader_has_exited() {
                        ending = Some(Ending::TimedOut { limit: spec.time_limit });
                    }
                    finish_dialogue_write(owner).await;
                }
                if term_at.is_none() && !shared.leader_has_exited() {
                    let listed = shared.before_stop(spec.timings.stop_listing, &stop.hurry).await;
                    if !shared.leader_has_exited() {
                        if !listed {
                            shared.note_listing_missed();
                        }
                        ending = Some(Ending::TimedOut { limit: spec.time_limit });
                        term_at = Some(Instant::now());
                        term_retry = retry_for(shared, Signal::TERM, shared.terminate().await);
                    }
                }
            },
            _ = tokio::time::sleep_until(term_at.map(|t| t + spec.stop_grace).unwrap_or_else(far)),
                if term_at.is_some() && !kill_sent => {
                term_retry = None;
                kill_retry = retry_for(shared, Signal::KILL, shared.kill_all().await);
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
                // without changing how the run is reported. One listing comes
                // first: the TERM that follows can end a member that has a
                // descendant in a session of its own, and that descendant is
                // then orphaned and cannot be told from any other process.
                if term_at.is_none() {
                    let listing = shared.clone();
                    let hurry = stop.hurry.clone();
                    let budget = spec.timings.exit_listing;
                    exit_listing = Some(Box::pin(async move {
                        if !listing.before_stop(budget, &hurry).await {
                            listing.note_listing_missed();
                        }
                    }));
                }
            },
            _ = async { exit_listing.as_mut().expect("guarded").await },
                if exit_listing.is_some() => {
                exit_listing = None;
                term_at = Some(Instant::now());
                term_retry = retry_for(shared, Signal::TERM, shared.terminate().await);
            },
            _ = async { term_retry.as_mut().expect("guarded").await },
                if term_retry.is_some() => {
                term_retry = None;
            },
            _ = async { kill_retry.as_mut().expect("guarded").await },
                if kill_retry.is_some() => {
                kill_retry = None;
            },
            n = async { stdout.as_mut().expect("guarded").read(&mut out_buf).await },
                if stdout.is_some() => match n {
                Ok(0) | Err(_) => {
                    stdout = None;
                    if let Some(dialogue) = dialogue {
                        if !pending_line.is_empty() && dialogue.line(&pending_line) {
                            out.push(&pending_line);
                            if let Some(tap) = tap {
                                tap.offer(&pending_line);
                            }
                        }
                        dialogue.end(End::ProviderClosed);
                    }
                },
                Ok(n) => {
                    let mut keep = |bytes: &[u8]| {
                        out.push(bytes);
                        if let Some(tap) = tap {
                            tap.offer(bytes);
                        }
                    };
                    if let Some(dialogue) = dialogue {
                        for part in out_buf[..n].split_inclusive(|byte| *byte == b'\n') {
                            if pending_line.len() + part.len() > MAX_DIALOGUE_FRAME_BYTES {
                                dialogue.end(End::ProtocolError);
                                ending = Some(Ending::WaitFailed {
                                    message: "Claude Code stream frame exceeded the size limit".into(),
                                });
                                stdout = None;
                                break;
                            }
                            pending_line.extend_from_slice(part);
                            if part.ends_with(b"\n") {
                                if dialogue.line(&pending_line) {
                                    keep(&pending_line);
                                }
                                pending_line.clear();
                            }
                        }
                    } else {
                        keep(&out_buf[..n]);
                    }
                }
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
                    term_at.map(|t| t + cleanup_deadline).unwrap_or_else(far)),
                if term_at.is_some() => {
                // Nothing may keep the owner here without bound: report what
                // is known, with cleanup unconfirmed if it is.
                break;
            },
        }
    }
    if let Some(listing) = exit_listing.take() {
        // The pipes closed before the listing finished; the signals still wait.
        listing.await;
        term_at = Some(Instant::now());
        let _ = shared.terminate().await;
    }
    if let Some(dialogue) = dialogue {
        dialogue.end(End::ProviderClosed);
    }
    drop(term_retry);
    drop(kill_retry);
    if !exited {
        owner.exit_rx = Some(exit_rx);
    }
    if watch_failed && ending.is_none() {
        ending = Some(Ending::WaitFailed {
            message: "the leader's exit could not be observed".into(),
        });
    }
    Driven {
        ending,
        stdout: out.finish(),
        stderr: err.finish(),
        term_at,
        hard_stop: false,
    }
}

/// The repeat of a stop sweep, when the stop's own bounded sweep did not
/// complete.
fn retry_for(
    shared: &Arc<Shared>,
    signal: Signal,
    completed: bool,
) -> Option<std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>> {
    if completed {
        return None;
    }
    let shared = shared.clone();
    Some(Box::pin(async move { shared.retry_sweeps(signal).await }))
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
    shared: &Shared,
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

fn failed_to_start(spec: &ProcessSpec, message: String) -> ProcessReport {
    let now = SystemTime::now();
    ProcessReport {
        pid: None,
        ending: Ending::FailedToStart { message },
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
            session_members_swept: Vec::new(),
            keeper: spec
                .keeper
                .as_ref()
                .map(|_| super::report::KeeperDiagnostics::unstarted()),
        },
    }
}

async fn finish_dialogue_write(owner: &mut OwnedProcess) {
    if let Some(task) = &mut owner.stdin_task {
        if tokio::time::timeout(DIALOGUE_STOP_WRITE_BUDGET, &mut *task)
            .await
            .is_err()
        {
            task.abort();
        } else {
            owner.stdin_task = None;
        }
    }
}
