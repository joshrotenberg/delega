//! Results for runs the provider layer never saw, built with the contract's
//! constructors.
//!
//! A run that never started, one that was stopped in the queue, one refused at
//! admission and one whose task failed all still end in exactly one valid
//! result. The queued time counts: `timing.queued_at` is when the supervisor
//! admitted the run.

use std::time::SystemTime;

use crate::VERSION;
use crate::contract::result::{
    AppliedPermissions, Cleanup, ErrorDetail, Provenance, RefusalContext, RunContext, RunResult,
    Timing, WorkerRef,
};
use crate::contract::validate::Problem;
use crate::process::rfc3339_utc;

use super::record::Run;

pub(super) fn provenance() -> Provenance {
    Provenance {
        delega_version: VERSION.to_string(),
        build: None,
        provider_executable: None,
        provider_version: None,
    }
}

fn permissions(run: &Run) -> AppliedPermissions {
    AppliedPermissions {
        mode: run.mode,
        denied: Vec::new(),
    }
}

/// The timing of a run that ends now, whether or not it started.
pub(super) fn timing(run: &Run, finished: SystemTime) -> Timing {
    Timing {
        queued_at: rfc3339_utc(run.queued_at),
        started_at: run.started_at.map(rfc3339_utc),
        finished_at: rfc3339_utc(finished),
        duration_ms: run.started.map_or(0, |s| {
            s.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
        }),
        timeout_secs: run.timeout_secs,
    }
}

fn refusal_context(run: &Run, worker: Option<WorkerRef>) -> RefusalContext {
    RefusalContext {
        run: run.id.clone(),
        worker,
        provider: Some(run.provider),
        permissions: permissions(run),
        timing: timing(run, SystemTime::now()),
        provenance: provenance(),
    }
}

fn context(run: &Run, worker: Option<WorkerRef>, cleanup: Cleanup) -> RunContext {
    RunContext {
        run: run.id.clone(),
        worker,
        provider: run.provider,
        permissions: permissions(run),
        cleanup,
        timing: timing(run, SystemTime::now()),
        provenance: provenance(),
    }
}

/// Check a result the supervisor built itself. One that fails
/// `RunResult::validate` is a bug here. It is replaced, in every build, by a
/// `failed` result whose error and a provider note name the validation problems
/// (fixed text about fields, never the user's), so a run still ends in exactly
/// one valid result and the fault is visible to whoever reads it.
fn seal(run: &Run, worker: Option<WorkerRef>, mut result: RunResult) -> RunResult {
    // The supervisor knows only what the caller said: the configuration file and
    // the layer of each field. The rest of `applied` is decided when a run is
    // prepared, which most of these runs never were; a panicked task and a result
    // replaced for failing validation drop it too (docs/api.md, "`applied`").
    result.applied = run.origin.clone().or_absent();
    #[cfg(test)]
    let result = corrupt_for_test(result);
    match result.validate() {
        Ok(()) => result,
        Err(problems) => {
            let message = problems
                .iter()
                .map(Problem::to_string)
                .collect::<Vec<_>>()
                .join("; ");
            let mut fallback = RunResult::failed(
                context(run, worker, result.cleanup.clone()),
                ErrorDetail {
                    kind: "internal_error".to_string(),
                    message: format!("the supervisor built an inconsistent result: {message}"),
                    provider_exit_code: None,
                },
                result.text,
            );
            fallback.applied = run.origin.clone().or_absent();
            fallback.provider_notes.push(format!(
                "the supervisor replaced a result it built because it failed validation: \
                 {message}"
            ));
            fallback
        }
    }
}

#[cfg(test)]
thread_local! {
    static CORRUPT_NEXT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Make the next result sealed on this thread invalid. Test only; reaches the
/// supervisor's task only when the test runs on a current-thread runtime.
#[cfg(test)]
pub(super) fn corrupt_next_seal_for_test() {
    CORRUPT_NEXT.with(|flag| flag.set(true));
}

#[cfg(test)]
fn corrupt_for_test(mut result: RunResult) -> RunResult {
    if CORRUPT_NEXT.with(std::cell::Cell::take) {
        result.error = None;
    }
    result
}

/// The run did not go ahead, for a stated reason.
pub(super) fn refused(
    run: &Run,
    worker: Option<WorkerRef>,
    kind: &str,
    message: &str,
) -> RunResult {
    let result =
        RunResult::refused_before_start(refusal_context(run, worker.clone()), kind, message);
    seal(run, worker, result)
}

/// The request failed validation. `problems` is never empty here.
pub(super) fn refused_invalid(
    run: &Run,
    worker: Option<WorkerRef>,
    problems: &[Problem],
) -> RunResult {
    let result = RunResult::refused_invalid(refusal_context(run, worker.clone()), problems)
        .unwrap_or_else(|_| {
            refused(
                run,
                worker.clone(),
                "invalid_request",
                "the request is not valid",
            )
        });
    seal(run, worker, result)
}

/// The run was stopped before its provider started.
pub(super) fn interrupted_unstarted(run: &Run, worker: Option<WorkerRef>, why: &str) -> RunResult {
    let result = RunResult::interrupted(context(run, worker.clone(), Cleanup::Confirmed), why, "");
    seal(run, worker, result)
}

/// The task that followed the run ended without a result. Whatever it started
/// was sent an interrupt, and nothing confirmed it stopped.
pub(super) fn crashed(
    run: &Run,
    worker: Option<WorkerRef>,
    why: &str,
    partial_text: String,
) -> RunResult {
    let cleanup = Cleanup::Unconfirmed {
        pids: Vec::new(),
        process_groups: Vec::new(),
        reason: format!(
            "{why}; the provider process, if one started, was sent an interrupt but nothing \
             confirmed it stopped"
        ),
    };
    let result = RunResult::failed(
        context(run, worker.clone(), cleanup),
        ErrorDetail {
            kind: "internal_error".to_string(),
            message: why.to_string(),
            provider_exit_code: None,
        },
        partial_text,
    );
    seal(run, worker, result)
}

/// A result that failed `RunResult::validate`, replaced by one that does not.
pub(super) fn invalid_result(
    run: &Run,
    worker: Option<WorkerRef>,
    problems: &[Problem],
    cleanup: Cleanup,
    partial_text: String,
) -> RunResult {
    let message = problems
        .iter()
        .map(Problem::to_string)
        .collect::<Vec<_>>()
        .join("; ");
    let result = RunResult::failed(
        context(run, worker.clone(), cleanup),
        ErrorDetail {
            kind: "internal_error".to_string(),
            message: format!("the provider layer returned an inconsistent result: {message}"),
            provider_exit_code: None,
        },
        partial_text,
    );
    seal(run, worker, result)
}
