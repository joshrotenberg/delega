//! Retention: finished results stay readable for a bounded count, size and time.
//!
//! Pruning runs before every command, so no timer is needed. It only ever
//! removes finished runs, oldest first, and a closed worker once none of its
//! runs is kept.

use std::time::Instant;

use crate::contract::result::RunResult;

use super::super::activity::Activity;
use super::Core;

/// What a kept result costs, estimated from the parts that can be large: the
/// answer, the structured answer, the error and notes, and the partial text
/// kept beside it. A fixed amount stands for the rest.
pub(super) fn weigh(result: &RunResult, activity: &Activity) -> usize {
    const FIXED: usize = 2048;
    let structured = result
        .structured
        .as_ref()
        .map_or(0, |value| value.to_string().len());
    let error = result
        .error
        .as_ref()
        .map_or(0, |e| e.kind.len() + e.message.len());
    let notes: usize = result.provider_notes.iter().map(String::len).sum();
    let denied: usize = result
        .permissions
        .denied
        .iter()
        .map(|d| d.request.len())
        .sum();
    FIXED + result.text.len() + structured + error + notes + denied + activity.text().len()
}

impl Core {
    /// Drop finished runs beyond the count or the byte bound, or older than
    /// the retention time. The newest result is never dropped for size.
    pub(super) fn prune(&mut self, now: Instant) {
        let limits = &self.config.limits;
        let (max, max_bytes, age) = (
            limits.retained_results,
            limits.retained_bytes,
            limits.retention,
        );
        while let Some(front) = self.finished.front() {
            let expired = self
                .runs
                .get(front)
                .and_then(|r| r.finished_at)
                .is_none_or(|at| now.saturating_duration_since(at) >= age);
            let too_many = self.finished.len() > max;
            let too_big = self.finished.len() > 1 && self.retained_bytes > max_bytes;
            if !(too_many || too_big || expired) {
                break;
            }
            if let Some(id) = self.finished.pop_front() {
                self.evict(&id);
            }
        }
        // A closed worker with nothing kept is gone, however it got there.
        self.workers.retain(|_, w| {
            !(w.closed && w.runs.is_empty() && w.current.is_none() && w.queue.is_empty())
        });
    }

    fn evict(&mut self, id: &str) {
        let Some(run) = self.runs.remove(id) else {
            return;
        };
        self.retained_bytes = self.retained_bytes.saturating_sub(run.bytes);
        let Some(worker_id) = run.worker else {
            return;
        };
        if let Some(worker) = self.workers.get_mut(&worker_id) {
            worker.runs.retain(|r| r != id);
        }
    }
}
