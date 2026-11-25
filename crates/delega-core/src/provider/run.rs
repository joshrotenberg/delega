//! Running a request: validate, prepare, start the provider, wait, settle.
//!
//! Every path ends in exactly one [`RunResult`]. Preparation may start two
//! short probes of the provider executable (its version, and for Codex the MCP
//! servers it lists); their time counts against the run's time limit, and the
//! cleanup verdict of a probe is part of the result's cleanup.

#[cfg(not(test))]
use std::time::Instant;
use std::time::{Duration, SystemTime};

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::cleanup_reason::{self, RunLost};
use crate::contract::request::{Provider, Request, Session};
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

/// The note a result carries when the provider ran without a keeper.
pub const NO_KEEPER_NOTE: &str = "cleanup ran without a keeper: a descendant that detaches \
     between listings, a fork relay, or a SIGKILL of the caller can leave a process running";

/// The note a kept run on macOS carries: the scope of cleanup once a child
/// started (see "What a macOS verdict covers" in [`crate::process`]).
pub const MACOS_KEEPER_NOTE: &str = "on macOS, cleanup after a provider process starts finds \
     its descendants through its process group, its session, earlier listings and the run \
     marker; a descendant none of these reach can be missed";

/// The cleanup note a run carries, if any, given whether it has a keeper and
/// whether it runs on macOS.
pub(super) fn cleanup_note(kept: bool, macos: bool) -> Option<&'static str> {
    match (kept, macos) {
        (false, _) => Some(NO_KEEPER_NOTE),
        (true, true) => Some(MACOS_KEEPER_NOTE),
        (true, false) => None,
    }
}

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
    /// interrupts the run: the join handle stays in the handle until the task
    /// has ended, so a dropped future still leaves `Drop` something to act on.
    pub async fn wait(mut self) -> RunResult {
        if let Some(result) = self.ready.take() {
            return *result;
        }
        let Some(join) = self.join.as_mut() else {
            return self.abnormal(RunLost::ResultTaken);
        };
        let joined = join.await;
        self.join = None;
        match joined {
            Ok(result) => result,
            Err(error) => {
                let why = if error.is_panic() {
                    RunLost::TaskPanicked
                } else {
                    RunLost::RuntimeShutDown
                };
                self.abnormal(why)
            }
        }
    }

    fn abnormal(&self, why: RunLost) -> RunResult {
        let cleanup = Cleanup::Unconfirmed {
            pids: Vec::new(),
            process_groups: Vec::new(),
            reason: cleanup_reason::run_lost(why),
        };
        let timing = self.facts.timing_unstarted(SystemTime::now());
        let ctx = self.facts.run_context(cleanup, timing, Vec::new());
        let mut result = RunResult::failed(
            ctx,
            ErrorDetail {
                kind: "internal_error".to_string(),
                message: why.text().to_string(),
                provider_exit_code: None,
            },
            "",
        );
        result.applied = self.facts.result_applied();
        result
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
    prepare(&request, &options).launch(CancellationToken::new())
}

/// A run whose preparation is done: refused already, or planned and ready to
/// start. Made by [`prepare`], which needs no runtime and may block on the
/// filesystem; [`launch`](Self::launch) starts it on the runtime. Dropping it
/// starts nothing and reports nothing.
pub(crate) struct Prepared {
    facts: Facts,
    progress: Progress,
    ready: Ready,
}

enum Ready {
    /// Refused before start. The progress sink has not been told yet.
    Refused(Box<RunResult>),
    Planned {
        plan: Box<Plan>,
        session: Session,
    },
}

/// The synchronous part of [`start`]: resolve the request into a plan, which
/// canonicalizes paths and reads settings files, managed settings included. It
/// touches neither the runtime nor the progress sink, so it can run on a thread
/// of its own and its result can be discarded.
pub(crate) fn prepare(request: &Request, options: &ProviderOptions) -> Prepared {
    let queued_at = SystemTime::now();
    let mut facts = Facts::new(request, options, queued_at);
    let progress = Progress::new(options.progress.clone(), options.progress_text);
    let plan = if request.provider == Provider::Claude
        && request.permissions.on_request == crate::contract::permissions::OnRequest::Ask
        && options.asks.0.is_none()
        && request.validate().is_ok()
    {
        Err(Refusal::new(
            "permissions_not_supported",
            "This run cannot answer live permission requests. Use delega mcp or the Rust supervisor API, or set permissions.on_request to deny.",
        ))
    } else {
        plan::build(request, options)
    };
    let ready = match plan {
        Err(refusal) => {
            let mut result = RunResult::refused_before_start(
                facts.refusal_context(),
                &refusal.kind,
                &refusal.message,
            );
            result.applied = facts.result_applied();
            Ready::Refused(Box::new(result))
        }
        Ok(plan) => {
            facts.provenance.provider_executable = Some(plan.program.display().to_string());
            facts.applied = plan.applied.clone();
            Ready::Planned {
                plan: Box::new(plan),
                session: request.session.clone(),
            }
        }
    };
    Prepared {
        facts,
        progress,
        ready,
    }
}

impl Prepared {
    /// Start the prepared run and return its handle. A refused run's handle has
    /// its result already.
    ///
    /// `cancel` stops the run, exactly as the handle's interrupt does. A caller
    /// that has a stop token of its own passes it, or a child of it, so a stop
    /// that lands while the run is being launched reaches it at once: for a
    /// plan ready to execute, a token already cancelled ends the run
    /// `interrupted` before any probe or provider process starts. A plan that
    /// was refused returns its refusal whatever the token.
    ///
    /// # Panics
    ///
    /// Panics if called outside a Tokio runtime and the run was not refused.
    pub(crate) fn launch(self, cancel: CancellationToken) -> RunHandle {
        let Self {
            facts,
            progress,
            ready,
        } = self;
        match ready {
            Ready::Refused(mut result) => {
                progress.finished(&mut result);
                RunHandle {
                    cancel,
                    facts,
                    ready: Some(result),
                    join: None,
                }
            }
            Ready::Planned { plan, session } => {
                let join = tokio::spawn(execute(
                    *plan,
                    facts.clone(),
                    cancel.clone(),
                    session,
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

pub(super) async fn probe(
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
    spec.keeper = plan.keeper.clone();
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
    result.applied = facts.result_applied();
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
    session: Session,
    progress: Progress,
) -> RunResult {
    let mut result = execute_steps(plan, facts, cancel, &session, &progress).await;
    progress.finished(&mut result);
    result
}

async fn execute_steps(
    plan: Plan,
    mut facts: Facts,
    cancel: CancellationToken,
    session: &Session,
    progress: &Progress,
) -> RunResult {
    // Unit tests drive the preparation budget and the process timer together.
    #[cfg(test)]
    let began = tokio::time::Instant::now();
    #[cfg(not(test))]
    let began = Instant::now();
    let mut notes = plan.notes.clone();
    if let Some(note) = cleanup_note(plan.keeper.is_some(), cfg!(target_os = "macos")) {
        notes.push(note.to_string());
    }
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
    let stdout = match version_report.ending {
        Ending::Exited { code: 0 } => Some(version_report.stdout.text()),
        _ => None,
    };
    // The version is parsed from the line as the provider printed it and
    // stored with credentials removed, like other provider text.
    let version = stdout.as_deref().and_then(program::version_line);
    facts.provenance.provider_version = stdout
        .as_deref()
        .and_then(|s| program::version_text(s, &plan.redactor));
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
    if let Some(asks) = &plan.asks {
        asks.redact_with(plan.redactor.clone());
        spec.dialogue = Some(asks.clone());
    }
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
    spec.keeper = plan.keeper.clone();
    #[cfg(test)]
    if let Some(chunks) = plan.tap_queue_chunks {
        spec.timings.tap_queue_chunks = chunks;
    }
    spec.stdout_tap = progress.tap(plan.provider, plan.redactor.clone());
    spec.stdout_tap_dropped = Some(progress.drop_counter());
    progress.emit(ProgressEvent::Started {
        provider: plan.provider,
        program: plan.program.display().to_string(),
    });
    let mut report = drive(spec, &cancel).await;
    if let Some(asks) = &plan.asks {
        if asks.failed() {
            report.ending = Ending::WaitFailed {
                message: "Claude Code permission protocol failed".into(),
            };
        } else {
            // The child can finish on the denial or EOF before the process
            // loop observes a stop that had already closed the dialogue.
            match asks.closed_by() {
                Some("interrupt" | "close" | "shutdown" | "connection_closed") => {
                    report.ending = Ending::Interrupted;
                }
                Some("run_timeout") => {
                    report.ending = Ending::TimedOut { limit: remaining };
                }
                _ => {}
            }
        }
    }

    let mut parsed = match plan.provider {
        Provider::Claude => claude::parse_with(&report.stdout.bytes, &plan.redactor),
        Provider::Codex => codex::parse(&report.stdout.bytes),
    };
    if plan.asks.is_some() {
        for denial in &mut parsed.denied {
            let tool = denial.tool.split([':', ' ']).next().unwrap_or_default();
            denial.tool = if super::claude::BUILTINS.contains(&tool) {
                tool.to_owned()
            } else {
                "unknown tool".to_owned()
            };
            denial.target = None;
            denial.reason = None;
        }
    }
    let cleanup = merge_cleanup(cleanup, report.cleanup.clone());
    let mut result = settle(
        Settle {
            facts: &facts,
            validator: plan.validator.as_ref(),
            ephemeral: session.ephemeral,
            resume: session.resume.as_deref(),
            notes,
            max_output_bytes: plan.max_output_bytes,
            redactor: &plan.redactor,
        },
        &report,
        &parsed,
        cleanup,
    );
    if let Some(asks) = plan.asks {
        result.permissions.asked = asks.audit();
    }
    result
}
