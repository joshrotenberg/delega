//! What every result of a run shares, and the contexts the contract
//! constructors need.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::VERSION;
use crate::contract::permissions::Mode;
use crate::contract::request::{Provider, Request};
use crate::contract::result::{
    AppliedPermissions, Cleanup, DeniedRequest, Provenance, RefusalContext, RunContext, Timing,
    WorkerRef,
};
use crate::process::rfc3339_utc;

use super::options::ProviderOptions;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn generated_run_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("run-{nanos:x}-{n:x}")
}

/// The facts a result needs that do not depend on how the run went.
#[derive(Debug, Clone)]
pub(super) struct Facts {
    pub run: String,
    pub worker: Option<WorkerRef>,
    pub provider: Provider,
    pub mode: Mode,
    pub queued_at: SystemTime,
    pub timeout_secs: u64,
    pub provenance: Provenance,
}

impl Facts {
    pub fn new(request: &Request, options: &ProviderOptions, queued_at: SystemTime) -> Self {
        Self {
            run: options.run_id.clone().unwrap_or_else(generated_run_id),
            worker: options.worker.clone(),
            provider: request.provider,
            mode: request.permissions.mode,
            queued_at,
            timeout_secs: request.timeout_secs,
            provenance: Provenance {
                delega_version: VERSION.to_string(),
                build: None,
                provider_executable: None,
                provider_version: None,
            },
        }
    }

    pub fn applied(&self, denied: Vec<DeniedRequest>) -> AppliedPermissions {
        AppliedPermissions {
            mode: self.mode,
            denied,
        }
    }

    /// A timing for a run that never started a provider.
    pub fn timing_unstarted(&self, finished: SystemTime) -> Timing {
        Timing {
            queued_at: rfc3339_utc(self.queued_at),
            started_at: None,
            finished_at: rfc3339_utc(finished),
            duration_ms: 0,
            timeout_secs: self.timeout_secs,
        }
    }

    pub fn refusal_context(&self) -> RefusalContext {
        RefusalContext {
            run: self.run.clone(),
            worker: self.worker.clone(),
            provider: Some(self.provider),
            permissions: self.applied(Vec::new()),
            timing: self.timing_unstarted(SystemTime::now()),
            provenance: self.provenance.clone(),
        }
    }

    pub fn run_context(
        &self,
        cleanup: Cleanup,
        timing: Timing,
        denied: Vec<DeniedRequest>,
    ) -> RunContext {
        RunContext {
            run: self.run.clone(),
            worker: self.worker.clone(),
            provider: self.provider,
            permissions: self.applied(denied),
            cleanup,
            timing,
            provenance: self.provenance.clone(),
        }
    }
}

/// Combine two cleanup verdicts: confirmed only when both are.
pub(super) fn merge_cleanup(first: Cleanup, second: Cleanup) -> Cleanup {
    match (first, second) {
        (Cleanup::Confirmed, other) | (other, Cleanup::Confirmed) => other,
        (
            Cleanup::Unconfirmed {
                mut pids,
                mut process_groups,
                reason,
            },
            Cleanup::Unconfirmed {
                pids: more_pids,
                process_groups: more_groups,
                reason: more_reason,
            },
        ) => {
            pids.extend(more_pids);
            process_groups.extend(more_groups);
            Cleanup::Unconfirmed {
                pids,
                process_groups,
                reason: format!("{reason}; {more_reason}"),
            }
        }
    }
}
