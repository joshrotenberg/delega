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

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::oneshot;

use super::super::command::{WaitStart, Wake};
use super::super::error::Error;
use super::super::limits::MAX_WAIT_TARGETS;
use super::super::state::RunState;
use super::super::views::{RunBrief, WaitLimit, WaitResponse, WaitTargets};
use super::Core;

/// A registered wait.
pub(super) struct Waiter {
    runs: Vec<String>,
    /// Each named worker, and the run it was on when the wait was made.
    workers: Vec<(String, Option<String>)>,
    after: Option<u64>,
    wake: oneshot::Sender<Wake>,
    /// True while the caller waits for `wake`. The caller clears it when its
    /// time has passed; the registration then stays only until the caller has
    /// its view, and does not count as a wait in flight.
    blocked: Arc<AtomicBool>,
    #[cfg(any(test, feature = "test-support"))]
    removal_ack: Option<super::super::wait_test_support::RemovalAck>,
}

impl Waiter {
    /// Whether a caller is blocked on this registration.
    fn in_flight(&self) -> bool {
        !self.wake.is_closed() && self.blocked.load(Ordering::SeqCst)
    }
}

impl Core {
    /// Register a wait, or say that it is ready now.
    pub(super) fn wait(
        &mut self,
        targets: &WaitTargets,
        after: Option<u64>,
        applied: Duration,
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
        let released_now = after.is_some() && self.mode != super::Mode::Active;
        let (wake, rx) = oneshot::channel();
        let blocked = Arc::new(AtomicBool::new(true));
        let waiter = Waiter {
            runs,
            workers,
            after,
            wake,
            blocked: Arc::clone(&blocked),
            #[cfg(any(test, feature = "test-support"))]
            removal_ack: None,
        };
        if self.waiter_ready(&waiter) {
            return Ok(WaitStart::Ready);
        }
        if released_now {
            return Ok(WaitStart::Released);
        }
        // A wait given no time is a poll. Nothing named is ready, which is its
        // whole answer: it never blocks, so it is not registered, takes no
        // slot and is never turned away for the limit.
        if applied.is_zero() {
            let view = self.wait_view(targets, after, None, true, false, 0);
            return Ok(WaitStart::Answered(Box::new(view)));
        }
        self.waiters.retain(|_, w| !w.wake.is_closed());
        if self.waits_in_flight() >= self.config.limits.max_waits_in_flight {
            // The same answer as a poll, marked so the caller can tell.
            let mut view = self.wait_view(targets, after, None, true, false, 0);
            view.limit_reached = Some(WaitLimit::MaxWaitsInFlight);
            return Ok(WaitStart::Answered(Box::new(view)));
        }
        self.next_waiter += 1;
        let id = self.next_waiter;
        self.waiters.insert(id, waiter);
        #[cfg(any(test, feature = "test-support"))]
        if let Some(observer) = &self.wait_observer {
            self.waiters
                .get_mut(&id)
                .expect("just registered")
                .removal_ack = super::super::wait_test_support::acknowledge_registration(observer);
        }
        Ok(WaitStart::Pending { id, rx, blocked })
    }

    /// How many waits are blocked now: the registrations whose caller is
    /// still waiting to be woken. It is counted from the registrations, so it
    /// cannot drift from them. A registration is removed when it fires and
    /// when pruning removes what it named, and it stops counting, before it is
    /// removed, when its caller's time has passed (the caller keeps it until
    /// it has its view) and when its caller has gone.
    pub(super) fn waits_in_flight(&self) -> usize {
        self.waiters.values().filter(|w| w.in_flight()).count()
    }

    fn waiter_ready(&self, waiter: &Waiter) -> bool {
        let run_ready = |id: &String| match self.runs.get(id) {
            // Gone: it finished and its result has aged out.
            None => true,
            Some(run) => match waiter.after {
                Some(after) => {
                    run.ask_changed(after) || run.finished_seq.is_some_and(|seq| seq > after)
                }
                None => run.state == RunState::Finished || !run.pending().is_empty(),
            },
        };
        let worker_ready = |(id, baseline): &(String, Option<String>)| match self.workers.get(id) {
            None => true,
            Some(worker) => match waiter.after {
                Some(after) => {
                    worker.last_finish_seq > after
                        || worker
                            .current
                            .as_ref()
                            .and_then(|id| self.runs.get(id))
                            .is_some_and(|run| run.ask_changed(after))
                }
                None => baseline.as_ref().is_none_or(|r| {
                    self.runs
                        .get(r)
                        .is_none_or(|r| r.state == RunState::Finished || !r.pending().is_empty())
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
                let _ = waiter.wake.send(Wake::Finished);
            }
        }
    }

    /// Fire every registered wait, ready or not. Shutdown does this, so a call
    /// that waits on a cursor that no run will pass is released with the rest.
    pub(super) fn wake_all_waiters(&mut self) {
        let waiters: Vec<Waiter> = self.waiters.drain().map(|(_, w)| w).collect();
        for waiter in waiters {
            // A wait whose runs finished in the same command as the shutdown, a
            // queued run discarded for one, was ready on its own account.
            let why = if self.waiter_ready(&waiter) {
                Wake::Finished
            } else {
                Wake::Shutdown
            };
            let _ = waiter.wake.send(why);
        }
    }

    /// The answer to a wait: the current state of everything named.
    pub(super) fn wait_view(
        &mut self,
        targets: &WaitTargets,
        after: Option<u64>,
        waiter: Option<u64>,
        timed_out: bool,
        released: bool,
        timeout_secs: u64,
    ) -> WaitResponse {
        // A registration that is already gone fired before the caller gave up.
        let ready_now = waiter.is_some_and(|id| match self.waiters.remove(&id) {
            Some(w) => self.waiter_ready(&w),
            None => true,
        });
        let cursor = self.current_cursor();
        let mut unknown = Vec::new();
        let mut runs = Vec::new();
        for id in &targets.runs {
            match self.runs.get(id) {
                Some(run) => runs.push(RunBrief {
                    pending_interactions: run.pending(),
                    run: id.clone(),
                    worker: run.worker.clone(),
                    state: run.state,
                    outcome: run.result.as_ref().map(|r| r.outcome),
                    changed: after
                        .map(|a| run.ask_changed(a) || run.finished_seq.is_some_and(|seq| seq > a)),
                }),
                None => unknown.push(id.clone()),
            }
        }
        let mut workers = Vec::new();
        for id in &targets.workers {
            match self.workers.get(id) {
                Some(worker) => {
                    let mut brief = self.worker_brief(worker);
                    brief.changed = after.map(|a| {
                        worker.last_finish_seq > a
                            || worker
                                .current
                                .as_ref()
                                .and_then(|id| self.runs.get(id))
                                .is_some_and(|run| run.ask_changed(a))
                    });
                    workers.push(brief);
                }
                None => unknown.push(id.clone()),
            }
        }
        WaitResponse {
            cursor,
            timed_out: timed_out && !ready_now,
            released_by_shutdown: released,
            limit_reached: None,
            timeout_secs,
            runs,
            workers,
            unknown,
        }
    }
}
