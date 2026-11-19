//! `wait`: registering interest in runs and workers, and answering.
//!
//! A wait is two commands. The first registers it, or says it is ready now; the
//! caller then waits for the registration to fire or for its own timeout, off
//! the supervisor's task. The second builds the answer from the states at that
//! moment, so the states and the cursor are always current.
//!
//! A run finishing is the event. Without `after_cursor` a wait is ready when
//! any named run is finished, or when a named worker's current run, the one it
//! had when the wait was made, is finished. With `after_cursor` it is ready when
//! any named run, or any run of a named worker, finished after that cursor, so
//! a caller that passes back the cursor it got never misses a finish and never
//! sees the same one twice.

use tokio::sync::oneshot;

use super::super::command::WaitStart;
use super::super::error::Error;
use super::super::limits::MAX_WAIT_TARGETS;
use super::super::state::RunState;
use super::super::views::{RunBrief, WaitResponse, WaitTargets};
use super::Core;

/// A registered wait.
pub(super) struct Waiter {
    runs: Vec<String>,
    /// Each named worker, and the run it was on when the wait was made.
    workers: Vec<(String, Option<String>)>,
    after: Option<u64>,
    wake: oneshot::Sender<()>,
}

impl Core {
    /// Register a wait, or say that it is ready now.
    pub(super) fn wait(
        &mut self,
        targets: &WaitTargets,
        after: Option<u64>,
    ) -> Result<WaitStart, Error> {
        if targets.runs.is_empty() && targets.workers.is_empty() {
            return Err(Error::InvalidArguments(
                "name at least one run or worker to wait for".to_string(),
            ));
        }
        if targets.runs.len() + targets.workers.len() > MAX_WAIT_TARGETS {
            return Err(Error::InvalidArguments(format!(
                "wait takes at most {MAX_WAIT_TARGETS} ids"
            )));
        }
        let runs: Vec<String> = targets
            .runs
            .iter()
            .filter(|id| self.runs.contains_key(*id))
            .cloned()
            .collect();
        let workers: Vec<(String, Option<String>)> = targets
            .workers
            .iter()
            .filter_map(|id| {
                let worker = self.workers.get(id)?;
                let baseline = self.current_run_of(worker).filter(|r| {
                    self.runs
                        .get(r)
                        .is_some_and(|r| r.state != RunState::Finished)
                });
                Some((id.clone(), baseline))
            })
            .collect();
        if runs.is_empty() && workers.is_empty() {
            return Ok(WaitStart::Ready);
        }
        // Once shutdown has begun, a wait with a cursor answers at once, with the
        // states at that moment and a changed flag for each. A call that arrives
        // as shutdown begins is then released like one that was already waiting,
        // whatever its cursor. A wait without a cursor still waits for the runs
        // to settle, which is how a caller follows a shutdown that ran out of its
        // bound.
        if after.is_some() && self.mode != super::Mode::Active {
            return Ok(WaitStart::Ready);
        }
        let (wake, rx) = oneshot::channel();
        let waiter = Waiter {
            runs,
            workers,
            after,
            wake,
        };
        if self.waiter_ready(&waiter) {
            return Ok(WaitStart::Ready);
        }
        self.waiters.retain(|_, w| !w.wake.is_closed());
        self.next_waiter += 1;
        let id = self.next_waiter;
        self.waiters.insert(id, waiter);
        Ok(WaitStart::Pending { id, rx })
    }

    fn waiter_ready(&self, waiter: &Waiter) -> bool {
        let run_ready = |id: &String| match self.runs.get(id) {
            // Gone: it finished and its result has aged out.
            None => true,
            Some(run) => match waiter.after {
                Some(after) => run.finished_seq.is_some_and(|seq| seq > after),
                None => run.state == RunState::Finished,
            },
        };
        let worker_ready = |(id, baseline): &(String, Option<String>)| match self.workers.get(id) {
            None => true,
            Some(worker) => match waiter.after {
                Some(after) => worker.last_finish_seq > after,
                None => baseline.as_ref().is_none_or(|r| {
                    self.runs
                        .get(r)
                        .is_none_or(|r| r.state == RunState::Finished)
                }),
            },
        };
        waiter.runs.iter().any(run_ready) || waiter.workers.iter().any(worker_ready)
    }

    /// Fire every registered wait that is ready, and forget those whose caller
    /// has gone.
    pub(super) fn wake_waiters(&mut self) {
        if self.waiters.is_empty() {
            return;
        }
        let ready: Vec<u64> = self
            .waiters
            .iter()
            .filter(|(_, w)| w.wake.is_closed() || self.waiter_ready(w))
            .map(|(id, _)| *id)
            .collect();
        for id in ready {
            if let Some(waiter) = self.waiters.remove(&id) {
                let _ = waiter.wake.send(());
            }
        }
    }

    /// Fire every registered wait, ready or not. Shutdown does this, so a call
    /// that waits on a cursor that no run will pass is released with the rest.
    pub(super) fn wake_all_waiters(&mut self) {
        for (_, waiter) in self.waiters.drain() {
            let _ = waiter.wake.send(());
        }
    }

    /// The answer to a wait: the current state of everything named.
    pub(super) fn wait_view(
        &mut self,
        targets: &WaitTargets,
        after: Option<u64>,
        waiter: Option<u64>,
        timed_out: bool,
        timeout_secs: u64,
    ) -> WaitResponse {
        // A registration that is already gone fired before the caller gave up.
        let ready_now = waiter.is_some_and(|id| match self.waiters.remove(&id) {
            Some(w) => self.waiter_ready(&w),
            None => true,
        });
        let mut unknown = Vec::new();
        let mut runs = Vec::new();
        for id in &targets.runs {
            match self.runs.get(id) {
                Some(run) => runs.push(RunBrief {
                    run: id.clone(),
                    worker: run.worker.clone(),
                    state: run.state,
                    outcome: run.result.as_ref().map(|r| r.outcome),
                    changed: after.map(|a| run.finished_seq.is_some_and(|seq| seq > a)),
                }),
                None => unknown.push(id.clone()),
            }
        }
        let mut workers = Vec::new();
        for id in &targets.workers {
            match self.workers.get(id) {
                Some(worker) => {
                    let mut brief = self.worker_brief(worker);
                    brief.changed = after.map(|a| worker.last_finish_seq > a);
                    workers.push(brief);
                }
                None => unknown.push(id.clone()),
            }
        }
        WaitResponse {
            cursor: self.current_cursor(),
            timed_out: timed_out && !ready_now,
            timeout_secs,
            runs,
            workers,
            unknown,
        }
    }
}
