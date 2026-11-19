//! Running a request: validate, prepare, start the provider, wait, settle.
//!
//! Every path ends in exactly one [`RunResult`]. Preparation may start two
//! short probes of the provider executable (its version, and for Codex the MCP
//! servers it lists); their time counts against the run's time limit, and the
//! cleanup verdict of a probe is part of the result's cleanup.

use std::time::{Duration, Instant, SystemTime};

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::contract::request::{Provider, Request};
use crate::contract::result::{Cleanup, ErrorDetail, RunResult};
use crate::process::{CaptureSpec, Ending, Keep, ProcessHandle, ProcessReport, ProcessSpec};

use super::facts::{Facts, merge_cleanup};
use super::limits::{
    MAX_STDERR_BYTES, MCP_PROBE_TIMEOUT, MIN_CLAUDE_HOOK_VERSION, PROBE_STDERR_BYTES,
    PROBE_STDOUT_BYTES, PROBE_STOP_GRACE, VERSION_PROBE_TIMEOUT,
};
use super::options::ProviderOptions;
use super::plan::{self, Plan};
use super::progress::{Progress, ProgressEvent};
use super::refusal::Refusal;
use super::settle::{Settle, settle};
use super::{claude, codex, program};

/// A cloneable way to stop a run from another task or thread.
#[derive(Debug, Clone)]
pub struct RunInterrupter(CancellationToken);

impl RunInterrupter {
    /// Ask the run to stop. Returns at once; any number of calls is one.
    pub fn interrupt(&self) {
        self.0.cancel();
    }
}

/// A run in progress. [`wait`](Self::wait) returns its one result.
///
/// Dropping the handle before `wait` has returned interrupts the run: the
/// provider is stopped in the background and the result is discarded.
#[derive(Debug)]
pub struct RunHandle {
    cancel: CancellationToken,
    facts: Facts,
    ready: Option<Box<RunResult>>,
    join: Option<JoinHandle<RunResult>>,
}

impl RunHandle {
    /// The run id the result will carry.
    pub fn run_id(&self) -> &str {
        &self.facts.run
    }

    /// Ask the run to stop. The result is `interrupted`, unless the provider
    /// had already finished.
    pub fn interrupt(&self) {
        self.cancel.cancel();
    }

    /// A cloneable interrupter for another task or thread.
    pub fn interrupter(&self) -> RunInterrupter {
        RunInterrupter(self.cancel.clone())
    }

    /// Whether the result is ready.
    pub fn is_finished(&self) -> bool {
        self.ready.is_some() || self.join.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Wait for the result. Cancelling this future drops the handle, which
    /// interrupts the run.
    pub async fn wait(mut self) -> RunResult {
        if let Some(result) = self.ready.take() {
            return *result;
        }
        let Some(join) = self.join.take() else {
            return self.abnormal("the result was already taken");
        };
        match join.await {
            Ok(result) => result,
            Err(error) => {
                let why = if error.is_panic() {
                    "the run task panicked"
                } else {
                    "the runtime shut down before the run finished"
                };
                self.abnormal(why)
            }
        }
    }

    fn abnormal(&self, why: &str) -> RunResult {
        let cleanup = Cleanup::Unconfirmed {
            pids: Vec::new(),
            process_groups: Vec::new(),
            reason: format!(
                "{why}; the provider process, if one started, was sent an interrupt but nothing \
                 confirmed it stopped"
            ),
        };
        let timing = self.facts.timing_unstarted(SystemTime::now());
        let ctx = self.facts.run_context(cleanup, timing, Vec::new());
        RunResult::failed(
            ctx,
            ErrorDetail {
                kind: "internal_error".to_string(),
                message: why.to_string(),
                provider_exit_code: None,
            },
            "",
        )
    }
}

impl Drop for RunHandle {
    fn drop(&mut self) {
        if self.join.is_some() {
            self.cancel.cancel();
        }
    }
}

/// Run one request and return its result.
///
/// Validates the request, starts the provider, and always returns exactly one
/// result. Dropping the returned future interrupts the run.
pub async fn run(request: Request, options: ProviderOptions) -> RunResult {
    start(request, options).wait().await
}

/// Start one request and return a handle that can interrupt it.
///
/// A request that is refused before the provider starts returns a handle whose
/// result is already available.
///
/// # Panics
///
/// Panics if called outside a Tokio runtime and the request is not refused
/// first.
pub fn start(request: Request, options: ProviderOptions) -> RunHandle {
    let queued_at = SystemTime::now();
    let mut facts = Facts::new(&request, &options, queued_at);
    let progress = Progress::new(options.progress.clone());
    let cancel = CancellationToken::new();
    match plan::build(&request, &options) {
        Err(refusal) => {
            let mut result = RunResult::refused_before_start(
                facts.refusal_context(),
                &refusal.kind,
                &refusal.message,
            );
            progress.finished(&mut result);
            RunHandle {
                cancel,
                facts,
                ready: Some(Box::new(result)),
                join: None,
            }
        }
        Ok(plan) => {
            facts.provenance.provider_executable = Some(plan.program.display().to_string());
            let ephemeral = request.session.ephemeral;
            let join = tokio::spawn(execute(
                plan,
                facts.clone(),
                cancel.clone(),
                ephemeral,
                progress,
            ));
            RunHandle {
                cancel,
                facts,
                ready: None,
                join: Some(join),
            }
        }
    }
}

/// Start a process and wait for its report, stopping it if `cancel` fires.
async fn drive(spec: ProcessSpec, cancel: &CancellationToken) -> ProcessReport {
    let handle = ProcessHandle::start(spec);
    let interrupter = handle.interrupter();
    let mut wait = Box::pin(handle.wait());
    tokio::select! {
        report = &mut wait => report,
        () = cancel.cancelled() => {
            interrupter.interrupt();
            wait.await
        }
    }
}

async fn probe(
    plan: &Plan,
    args: Vec<String>,
    limit: Duration,
    cancel: &CancellationToken,
) -> ProcessReport {
    let mut spec = ProcessSpec::new(plan.program.clone(), limit)
        .args(args)
        .env(plan.env.to_policy())
        .cwd(plan.scope.cwd.clone());
    spec.stdout = CaptureSpec {
        limit: PROBE_STDOUT_BYTES,
        keep: Keep::Head,
    };
    spec.stderr = CaptureSpec {
        limit: PROBE_STDERR_BYTES,
        keep: Keep::Tail,
    };
    spec.stop_grace = PROBE_STOP_GRACE;
    drive(spec, cancel).await
}

/// How a run that never started its provider ends.
enum Unstarted {
    Refused(Refusal),
    Failed(Refusal),
    Interrupted,
    TimedOut,
}

fn unstarted(facts: &Facts, cleanup: Cleanup, notes: Vec<String>, how: Unstarted) -> RunResult {
    let finished = SystemTime::now();
    let mut result = match how {
        // `refused_before_start` reports a confirmed cleanup, so it is only
        // right when the probes left nothing behind.
        Unstarted::Refused(r) if cleanup == Cleanup::Confirmed => {
            RunResult::refused_before_start(facts.refusal_context(), &r.kind, &r.message)
        }
        Unstarted::Refused(r) | Unstarted::Failed(r) => {
            let ctx = facts.run_context(cleanup, facts.timing_unstarted(finished), Vec::new());
            RunResult::failed(
                ctx,
                ErrorDetail {
                    kind: r.kind,
                    message: r.message,
                    provider_exit_code: None,
                },
                "",
            )
        }
        Unstarted::Interrupted => {
            let ctx = facts.run_context(cleanup, facts.timing_unstarted(finished), Vec::new());
            RunResult::interrupted(ctx, "the run was stopped before the provider started", "")
        }
        Unstarted::TimedOut => {
            let ctx = facts.run_context(cleanup, facts.timing_unstarted(finished), Vec::new());
            RunResult::timed_out(
                ctx,
                format!(
                    "the {} s time limit passed before the provider started",
                    facts.timeout_secs
                ),
                "",
            )
        }
    };
    result.provider_notes = notes;
    result
}

fn probe_failure(what: &str, report: &ProcessReport) -> Refusal {
    let detail = match &report.ending {
        Ending::Exited { code } => format!("it exited with code {code}"),
        Ending::TimedOut { .. } => "it did not answer in time".to_string(),
        Ending::FailedToStart { .. } => "it could not be started".to_string(),
        _ => "it ended abnormally".to_string(),
    };
    Refusal::new(
        "provider_preparation_failed",
        format!("{what} failed: {detail}"),
    )
}

/// Follow one run to its result and report its end to the progress sink.
async fn execute(
    plan: Plan,
    facts: Facts,
    cancel: CancellationToken,
    ephemeral: bool,
    progress: Progress,
) -> RunResult {
    let mut result = execute_steps(plan, facts, cancel, ephemeral, &progress).await;
    progress.finished(&mut result);
    result
}

async fn execute_steps(
    plan: Plan,
    mut facts: Facts,
    cancel: CancellationToken,
    ephemeral: bool,
    progress: &Progress,
) -> RunResult {
    let began = Instant::now();
    let notes = plan.notes.clone();
    let mut cleanup = Cleanup::Confirmed;
    if cancel.is_cancelled() {
        return unstarted(&facts, cleanup, notes, Unstarted::Interrupted);
    }

    // What is left of the run's time limit. Probes count against it.
    let budget = || plan.time_limit.saturating_sub(began.elapsed());
    if budget().is_zero() {
        return unstarted(&facts, cleanup, notes, Unstarted::TimedOut);
    }

    let version_report = probe(
        &plan,
        vec!["--version".to_string()],
        VERSION_PROBE_TIMEOUT.min(budget()),
        &cancel,
    )
    .await;
    cleanup = merge_cleanup(cleanup, version_report.cleanup.clone());
    if version_report.ending == Ending::Interrupted {
        return unstarted(&facts, cleanup, notes, Unstarted::Interrupted);
    }
    if budget().is_zero() {
        return unstarted(&facts, cleanup, notes, Unstarted::TimedOut);
    }
    let version = match version_report.ending {
        Ending::Exited { code: 0 } => program::version_text(&version_report.stdout.text()),
        _ => None,
    };
    facts.provenance.provider_version = version.clone();
    if plan.needs_hook_version {
        let supported = version
            .as_deref()
            .and_then(program::parse_version)
            .is_some_and(|v| v >= MIN_CLAUDE_HOOK_VERSION);
        if !supported {
            let (a, b, c) = MIN_CLAUDE_HOOK_VERSION;
            let refusal = Refusal::new(
                "unsupported_provider_version",
                format!(
                    "exact command grants need Claude Code {a}.{b}.{c} or later for the blocking \
                     hook interface, and the installed version could not be confirmed to be that"
                ),
            );
            return unstarted(&facts, cleanup, notes, Unstarted::Refused(refusal));
        }
    }

    let mut args = plan.args.clone();
    if let Some(index) = plan.mcp_index {
        let report = probe(
            &plan,
            codex::discovery_args(),
            MCP_PROBE_TIMEOUT.min(budget()),
            &cancel,
        )
        .await;
        cleanup = merge_cleanup(cleanup, report.cleanup.clone());
        if report.ending == Ending::Interrupted {
            return unstarted(&facts, cleanup, notes, Unstarted::Interrupted);
        }
        if budget().is_zero() {
            return unstarted(&facts, cleanup, notes, Unstarted::TimedOut);
        }
        let overridden = match report.ending {
            Ending::Exited { code: 0 } => codex::disabled_servers(&report.stdout.text())
                .map_err(|m| Refusal::new("provider_preparation_failed", m)),
            _ => Err(probe_failure("listing Codex MCP servers", &report)),
        };
        match overridden {
            Ok(text) => args[index] = text,
            Err(refusal) => return unstarted(&facts, cleanup, notes, Unstarted::Failed(refusal)),
        }
    }

    let remaining = budget();
    if remaining.is_zero() {
        return unstarted(&facts, cleanup, notes, Unstarted::TimedOut);
    }
    if cancel.is_cancelled() {
        return unstarted(&facts, cleanup, notes, Unstarted::Interrupted);
    }

    let mut spec = ProcessSpec::new(plan.program.clone(), remaining)
        .args(args)
        .env(plan.env.to_policy())
        .cwd(plan.scope.cwd.clone())
        .stdin(plan.stdin.clone());
    spec.stdout = CaptureSpec {
        limit: plan.max_output_bytes,
        keep: Keep::Head,
    };
    spec.stderr = CaptureSpec {
        limit: MAX_STDERR_BYTES,
        keep: Keep::Tail,
    };
    if let Some(grace) = plan.stop_grace {
        spec.stop_grace = grace;
    }
    spec.stdout_tap = progress.tap(plan.provider, plan.redactor.clone());
    spec.stdout_tap_dropped = Some(progress.drop_counter());
    progress.emit(ProgressEvent::Started {
        provider: plan.provider,
        program: plan.program.display().to_string(),
    });
    let report = drive(spec, &cancel).await;

    let parsed = match plan.provider {
        Provider::Claude => claude::parse(&report.stdout.bytes),
        Provider::Codex => codex::parse(&report.stdout.bytes),
    };
    let cleanup = merge_cleanup(cleanup, report.cleanup.clone());
    settle(
        Settle {
            facts: &facts,
            validator: plan.validator.as_ref(),
            ephemeral,
            notes,
            max_output_bytes: plan.max_output_bytes,
            redactor: &plan.redactor,
        },
        &report,
        &parsed,
        cleanup,
    )
}
