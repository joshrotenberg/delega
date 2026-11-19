//! The queue: which run starts next, starting it, and finishing it.
//!
//! A worker takes one run at a time, in the order the runs were given. Among
//! workers, the run that became eligible first starts first, so one busy worker
//! cannot starve another. At most `max_running_runs` run at once.

use std::sync::Arc;
use std::time::{Instant, SystemTime};

use crate::contract::result::RunResult;
use crate::process::rfc3339_utc;
use crate::provider::ProgressSink;

use super::super::record::Run;
use super::super::results;
use super::super::runner;
use super::super::state::{RunState, WorkerState};
use super::{Core, Mode};

impl Core {
    /// Put the next queued run of a worker in line to start, when the worker
    /// has none running and none in line.
    pub(super) fn promote(&mut self, worker_id: &str) {
        let Some(worker) = self.workers.get(worker_id) else {
            return;
        };
        if worker.closed || worker.current.is_some() {
            return;
        }
        let Some(head) = worker.queue.front().cloned() else {
            return;
        };
        if let Some(run) = self.runs.get_mut(&head)
            && !run.in_ready
        {
            run.in_ready = true;
            self.ready.push_back(head);
        }
    }

    /// Bring a worker's state in line with what it holds.
    pub(super) fn refresh_worker(&mut self, worker_id: &str) {
        let Some(worker) = self.workers.get_mut(worker_id) else {
            return;
        };
        let to = WorkerState::derive(worker.closed, worker.current.is_some(), worker.queue.len());
        if to == worker.state {
            return;
        }
        match worker.state.transition(to) {
            Ok(state) => worker.state = state,
            Err(illegal) => {
                debug_assert!(false, "{illegal}");
                worker.state = to;
            }
        }
    }

    /// Start every run that is in line while a running slot is free.
    pub(super) fn dispatch(&mut self) {
        while self.mode == Mode::Active && self.running < self.config.limits.max_running_runs {
            let Some(id) = self.ready.pop_front() else {
                break;
            };
            let Some(run) = self.runs.get_mut(&id) else {
                continue;
            };
            run.in_ready = false;
            if run.state != RunState::Queued {
                continue;
            }
            self.start_run(&id);
        }
    }

    /// Start one queued run: finish its request, make it running and hand it to
    /// a task of its own.
    fn start_run(&mut self, id: &str) {
        let Some((worker_id, kind, request)) = self
            .runs
            .get_mut(id)
            .and_then(|r| Some((r.worker.clone()?, r.kind, r.request.take()?)))
        else {
            self.finish_unstarted_failed(id, "the run has no request to start");
            return;
        };
        let Some(worker) = self.workers.get(&worker_id) else {
            self.finish_unstarted_failed(id, "the run has no worker");
            return;
        };
        let mut request = request;
        if kind == super::super::record::Kind::FollowUp {
            let Some(session) = worker.session_id.clone() else {
                self.finish_unstarted_refused(
                    id,
                    "no_session",
                    "no earlier run of this worker reported a session, so there is nothing to \
                     continue",
                );
                return;
            };
            request.session.resume = Some(session);
            request.session.ephemeral = false;
        }
        let wref = worker.worker_ref();

        let stop = self.root.child_token();
        let mut options = self.config.provider.clone();
        options.run_id = Some(id.to_string());
        options.worker = Some(wref);
        options.progress_text = true;
        let Some(run) = self.runs.get_mut(id) else {
            return;
        };
        options.origin = run.origin.clone();
        let activity = run.activity.clone();
        let cursor = self.cursor.clone();
        let sink: ProgressSink = Arc::new(move |event| activity.record(event, &cursor));
        options.progress = Some(sink);

        match run.state.transition(RunState::Running) {
            Ok(state) => run.state = state,
            Err(illegal) => {
                debug_assert!(false, "{illegal}");
                run.state = RunState::Running;
            }
        }
        run.started_at = Some(SystemTime::now());
        run.started = Some(Instant::now());
        run.stop = Some(stop.clone());
        let tx = self.tx.clone();
        let join = tokio::spawn(runner::drive(
            self.launch.clone(),
            request,
            options,
            stop,
            tx.clone(),
            id.to_string(),
        ));
        run.abort = Some(join.abort_handle());
        tokio::spawn(runner::watch(join, tx, id.to_string()));
        self.running += 1;

        if let Some(worker) = self.workers.get_mut(&worker_id) {
            worker.queue.retain(|r| r != id);
            worker.current = Some(id.to_string());
        }
        self.refresh_worker(&worker_id);
    }

    fn finish_unstarted_refused(&mut self, id: &str, kind: &str, message: &str) {
        let Some(result) = self.runs.get(id).map(|run| {
            let wref = self.worker_ref_of(run);
            results::refused(run, wref, kind, message)
        }) else {
            return;
        };
        self.finish_run(id, result);
    }

    fn finish_unstarted_failed(&mut self, id: &str, why: &str) {
        let Some(result) = self.runs.get(id).map(|run| {
            let wref = self.worker_ref_of(run);
            results::crashed(run, wref, why, String::new())
        }) else {
            return;
        };
        self.finish_run(id, result);
    }

    pub(super) fn worker_ref_of(&self, run: &Run) -> Option<crate::contract::result::WorkerRef> {
        run.worker
            .as_ref()
            .and_then(|w| self.workers.get(w))
            .map(super::super::record::Worker::worker_ref)
    }

    /// A run's task reported its result.
    pub(super) fn run_finished(&mut self, id: &str, result: RunResult) {
        if !self
            .runs
            .get(id)
            .is_some_and(|r| r.state != RunState::Finished)
        {
            return;
        }
        let result = self.checked(id, result);
        self.finish_run(id, result);
    }

    /// A run's task ended without a result.
    pub(super) fn run_crashed(&mut self, id: &str, reason: &str) {
        let Some(result) = self
            .runs
            .get(id)
            .filter(|r| r.state != RunState::Finished)
            .map(|run| {
                let wref = self.worker_ref_of(run);
                results::crashed(run, wref, reason, run.activity.text())
            })
        else {
            return;
        };
        self.finish_run(id, result);
    }

    /// The result with `timing.queued_at` set to when the supervisor admitted
    /// the run, or a failed result if the provider layer's was inconsistent.
    fn checked(&self, id: &str, mut result: RunResult) -> RunResult {
        let Some(run) = self.runs.get(id) else {
            return result;
        };
        result.timing.queued_at = rfc3339_utc(run.queued_at);
        match result.validate() {
            Ok(()) => result,
            Err(problems) => results::invalid_result(
                run,
                self.worker_ref_of(run),
                &problems,
                result.cleanup.clone(),
                result.text.clone(),
            ),
        }
    }

    /// Make a run finished with `result`: the one place a run ends. Frees its
    /// running slot, tells whoever waits on it, records the session, and lines
    /// up the worker's next run.
    pub(super) fn finish_run(&mut self, id: &str, result: RunResult) {
        let seq = self.next_cursor();
        let Some(run) = self.runs.get_mut(id) else {
            return;
        };
        let was = run.state;
        match run.state.transition(RunState::Finished) {
            Ok(state) => run.state = state,
            Err(illegal) => {
                debug_assert!(false, "{illegal}");
                return;
            }
        }
        if was == RunState::Running {
            self.running = self.running.saturating_sub(1);
        }
        run.stop = None;
        run.abort = None;
        run.request = None;
        run.in_ready = false;
        run.finished_seq = Some(seq);
        run.finished_at = Some(Instant::now());
        let session = result.session_id.clone();
        run.bytes = super::retention::weigh(&result, &run.activity);
        self.retained_bytes += run.bytes;
        run.result = Some(result);
        for waiter in run.settle.drain(..) {
            let _ = waiter.send(());
        }
        let worker_id = run.worker.clone();
        self.ready.retain(|r| r != id);
        self.finished.push_back(id.to_string());

        if let Some(worker_id) = worker_id {
            if let Some(worker) = self.workers.get_mut(&worker_id) {
                if worker.current.as_deref() == Some(id) {
                    worker.current = None;
                }
                worker.queue.retain(|r| r != id);
                worker.last_run = Some(id.to_string());
                worker.last_finish_seq = seq;
                if session.is_some() {
                    worker.session_id = session;
                }
            }
            self.promote(&worker_id);
            self.refresh_worker(&worker_id);
        }
    }
}
