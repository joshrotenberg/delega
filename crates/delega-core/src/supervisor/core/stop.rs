//! Stopping runs: `interrupt`, `close` and shutdown.
//!
//! Each returns at once with what it did and, when a running run was asked to
//! stop, something to wait on. The waiting happens in the caller's task, bounded
//! by the process layer's own cleanup deadline and by a bound of the
//! supervisor's.

use tokio::sync::oneshot;

use super::super::command::{Closing, ShutdownStart, Stopping};
use super::super::error::Error;
use super::super::record::StopReason;
use super::super::results;
use super::super::state::{RunState, WorkerState};
use super::super::views::{CloseResponse, Detail, InterruptResponse, ResultView, Target};
use super::{Core, Mode};

impl Core {
    /// Stop a run: remove it from the queue, or ask its provider to stop, or
    /// leave a finished run as it is.
    pub(super) fn interrupt(&mut self, target: &Target) -> Result<Stopping, Error> {
        let run_id = match target {
            Target::Run(id) => {
                if !self.runs.contains_key(id) {
                    return Err(Error::UnknownRun(id.clone()));
                }
                Some(id.clone())
            }
            Target::Worker(id) => {
                let Some(worker) = self.workers.get(id) else {
                    return Err(Error::UnknownWorker(id.clone()));
                };
                self.current_run_of(worker)
            }
        };
        let Some(run_id) = run_id else {
            return Ok(Stopping::Done(Box::new(InterruptResponse {
                worker: match target {
                    Target::Worker(id) => Some(id.clone()),
                    Target::Run(_) => None,
                },
                run: None,
                state: None,
                settled: true,
                was_queued: false,
                result: None,
            })));
        };
        let state = self.runs[&run_id].state;
        match state {
            RunState::Finished => Ok(Stopping::Done(Box::new(
                self.interrupt_view(&run_id, false),
            ))),
            RunState::Queued => {
                let result = {
                    let run = &self.runs[&run_id];
                    results::interrupted_unstarted(
                        run,
                        self.worker_ref_of(run),
                        "interrupted before it started: the run was removed from the queue",
                    )
                };
                self.finish_run(&run_id, result);
                Ok(Stopping::Done(Box::new(self.interrupt_view(&run_id, true))))
            }
            RunState::Running => {
                self.request_stop(&run_id, StopReason::Interrupt);
                let (tx, rx) = oneshot::channel();
                if let Some(run) = self.runs.get_mut(&run_id) {
                    run.settle.push(tx);
                }
                Ok(Stopping::Pending {
                    base: Box::new(self.interrupt_view(&run_id, false)),
                    run: run_id,
                    rx,
                })
            }
        }
    }

    fn interrupt_view(&self, run_id: &str, was_queued: bool) -> InterruptResponse {
        let run = &self.runs[run_id];
        InterruptResponse {
            worker: run.worker.clone(),
            run: Some(run_id.to_string()),
            state: Some(run.state),
            settled: run.state == RunState::Finished,
            was_queued,
            result: run
                .result
                .as_ref()
                .map(|r| ResultView::of(r, Detail::Compact)),
        }
    }

    /// Close a worker: discard what is queued, stop what is running, free the
    /// slot. Closing again changes nothing and answers the same way.
    pub(super) fn close(&mut self, worker_id: &str) -> Result<Closing, Error> {
        let Some(worker) = self.workers.get(worker_id) else {
            return Err(Error::UnknownWorker(worker_id.to_string()));
        };
        let already_closed = worker.closed;
        let (discarded, _) = if already_closed {
            (Vec::new(), None)
        } else {
            self.close_worker(worker_id, StopReason::Close)
        };
        let running = self.workers.get(worker_id).and_then(|w| w.current.clone());
        let mut base = CloseResponse {
            worker: worker_id.to_string(),
            state: WorkerState::Closed,
            already_closed,
            discarded,
            stopped_run: running.clone(),
            settled: running.is_none(),
            result: None,
        };
        let Some(run) = running else {
            return Ok(Closing::Done(Box::new(base)));
        };
        base.settled = false;
        let (tx, rx) = oneshot::channel();
        if let Some(r) = self.runs.get_mut(&run) {
            r.settle.push(tx);
        }
        Ok(Closing::Pending {
            base: Box::new(base),
            run,
            rx,
        })
    }

    /// Mark a worker closed, finish its queued runs as interrupted and ask its
    /// running run to stop. Returns the discarded run ids and the running one.
    pub(super) fn close_worker(
        &mut self,
        worker_id: &str,
        reason: StopReason,
    ) -> (Vec<String>, Option<String>) {
        let Some(worker) = self.workers.get_mut(worker_id) else {
            return (Vec::new(), None);
        };
        worker.closed = true;
        let queued: Vec<String> = worker.queue.drain(..).collect();
        let running = worker.current.clone();
        let by = match reason {
            StopReason::Shutdown => "shutdown",
            _ => "close",
        };
        for id in &queued {
            let result = self.runs.get(id).map(|run| {
                results::interrupted_unstarted(
                    run,
                    self.worker_ref_of(run),
                    &format!("discarded by {by}: the worker was closed before this run started"),
                )
            });
            if let Some(result) = result {
                self.finish_run(id, result);
            }
        }
        if let Some(run) = &running {
            self.request_stop(run, reason);
        }
        self.refresh_worker(worker_id);
        (queued, running)
    }

    /// Ask a running run's task to stop its provider. Asking again changes
    /// nothing.
    fn request_stop(&mut self, run_id: &str, reason: StopReason) {
        if let Some(run) = self.runs.get_mut(run_id) {
            run.stop_reason.get_or_insert(reason);
            if let Some(stop) = &run.stop {
                stop.cancel();
            }
        }
    }

    /// Close every live worker and refuse new work from now on.
    pub(super) fn shutdown(&mut self) -> ShutdownStart {
        if self.mode == Mode::Active {
            self.mode = Mode::ShuttingDown;
        }
        let live: Vec<String> = self
            .workers
            .values()
            .filter(|w| !w.closed)
            .map(|w| w.id.clone())
            .collect();
        let mut start = ShutdownStart {
            workers_closed: live.len(),
            runs_stopped: 0,
            runs_discarded: 0,
            rx: None,
        };
        for id in live {
            let (discarded, running) = self.close_worker(&id, StopReason::Shutdown);
            start.runs_discarded += discarded.len();
            start.runs_stopped += usize::from(running.is_some());
        }
        self.wake_all_waiters();
        if self.running > 0 {
            let (tx, rx) = oneshot::channel();
            self.shutdown_waiters.push(tx);
            start.rx = Some(rx);
        }
        start
    }

    /// Settle a shutdown once nothing is running.
    pub(super) fn check_shutdown(&mut self) {
        if self.mode == Mode::ShuttingDown && self.running == 0 {
            self.mode = Mode::ShutDown;
        }
        if self.running == 0 {
            for waiter in self.shutdown_waiters.drain(..) {
                let _ = waiter.send(());
            }
        }
    }

    /// The runs that are still running.
    pub(super) fn unsettled(&self) -> Vec<String> {
        let mut ids: Vec<&String> = self
            .runs
            .values()
            .filter(|r| r.state == RunState::Running)
            .map(|r| &r.id)
            .collect();
        ids.sort();
        ids.into_iter().cloned().collect()
    }

    /// The compact result of a run, if it has finished.
    pub(super) fn settled(&self, run_id: &str) -> Option<ResultView> {
        self.runs
            .get(run_id)
            .and_then(|r| r.result.as_ref())
            .map(|r| ResultView::of(r, Detail::Compact))
    }
}
