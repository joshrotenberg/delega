//! Retention: finished results stay readable for a bounded count and time.
//!
//! Pruning runs after every command, so no timer is needed. It only ever
//! removes finished runs, oldest first, and a closed worker once none of its
//! runs is kept.

use std::time::Instant;

use super::Core;

impl Core {
    /// Drop finished runs beyond the count or older than the retention time.
    pub(super) fn prune(&mut self, now: Instant) {
        let limits = &self.config.limits;
        let (max, age) = (limits.retained_results, limits.retention);
        while let Some(front) = self.finished.front() {
            let expired = self
                .runs
                .get(front)
                .and_then(|r| r.finished_at)
                .is_none_or(|at| now.saturating_duration_since(at) >= age);
            if self.finished.len() <= max && !expired {
                break;
            }
            if let Some(id) = self.finished.pop_front() {
                self.evict(&id);
            }
        }
    }

    fn evict(&mut self, id: &str) {
        let Some(run) = self.runs.remove(id) else {
            return;
        };
        let Some(worker_id) = run.worker else {
            return;
        };
        let gone = match self.workers.get_mut(&worker_id) {
            Some(worker) => {
                worker.runs.retain(|r| r != id);
                worker.closed && worker.runs.is_empty()
            }
            None => false,
        };
        if gone {
            self.workers.remove(&worker_id);
        }
    }
}
