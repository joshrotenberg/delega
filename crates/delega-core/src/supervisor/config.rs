//! What a supervisor is built from.

use std::time::Duration;

use crate::provider::ProviderOptions;

use super::limits;

/// The limits a supervisor enforces. Every field has a default named in
/// [`limits`](super::limits).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    /// Most workers live at once. A closed worker frees its slot at once.
    /// At least 1; a smaller value is raised to 1.
    pub max_live_workers: usize,
    /// Most runs running at once. A run beyond it stays queued. At least 1.
    pub max_running_runs: usize,
    /// Most runs waiting behind a worker's running run. A follow-up beyond it
    /// is refused. 0 means a worker takes a follow-up only when it is idle.
    pub max_queued_per_worker: usize,
    /// Most finished results kept readable, across the supervisor. At least 1.
    pub retained_results: usize,
    /// How long a finished result stays readable.
    pub retention: Duration,
    /// The longest one `wait` blocks.
    pub wait_ceiling: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_live_workers: limits::MAX_LIVE_WORKERS,
            max_running_runs: limits::MAX_RUNNING_RUNS,
            max_queued_per_worker: limits::MAX_QUEUED_PER_WORKER,
            retained_results: limits::RETAINED_RESULTS,
            retention: limits::RETENTION,
            wait_ceiling: limits::WAIT_CEILING,
        }
    }
}

impl Limits {
    /// The limits with every lower bound applied.
    pub(super) fn normalized(mut self) -> Self {
        self.max_live_workers = self.max_live_workers.max(1);
        self.max_running_runs = self.max_running_runs.max(1);
        self.retained_results = self.retained_results.max(1);
        self
    }
}

/// The configuration of a [`Supervisor`](super::Supervisor).
///
/// `provider` is the base [`ProviderOptions`] every run inherits: the
/// executable overrides, the hook command and the environment options. For each
/// run the supervisor replaces `run_id`, `worker` and `progress`, which it owns.
#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    /// Admission, queue, retention and wait limits.
    pub limits: Limits,
    /// What every run inherits.
    pub provider: ProviderOptions,
    /// The longest `shutdown` waits for runs to settle.
    pub shutdown_bound: Duration,
    /// The longest `interrupt` or `close` waits for a running run to settle.
    pub interrupt_bound: Duration,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            provider: ProviderOptions::default(),
            shutdown_bound: limits::SHUTDOWN_BOUND,
            interrupt_bound: limits::INTERRUPT_BOUND,
        }
    }
}
