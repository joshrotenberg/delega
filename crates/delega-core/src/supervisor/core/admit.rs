//! Admitting a run: `start` and `follow_up`.
//!
//! Neither waits on anything. A request that cannot go ahead is still a run:
//! it is finished at once with a `refused` result that `read` returns. A
//! refused `start` creates no worker, because `docs/api.md` gives a refused
//! result no worker when none was created.

use std::time::Instant;

use crate::contract::applied::{Applied, Layer};
use crate::contract::request::Request;
use crate::contract::result::RunResult;

use super::super::error::Error;
use super::super::record::{Kind, Run, Worker};
use super::super::results;
use super::super::state::{RunState, WorkerState};
use super::super::views::StartResponse;
use super::{Core, Mode};

impl Core {
    /// Validate a request, then admit it as a new worker with its first run,
    /// or refuse it.
    pub(super) fn start(
        &mut self,
        request: Request,
        preflight: Option<(String, String)>,
        origin: Applied,
    ) -> Result<StartResponse, Error> {
        if self.mode != Mode::Active {
            return Err(Error::ShutDown);
        }
        let run_id = self.ids.run();
        let mut run = Run::new(run_id.clone(), Kind::First, &request);
        run.origin = origin.clone();

        if let Err(problems) = request.validate() {
            let result = results::refused_invalid(&run, None, &problems);
            return Ok(self.admit_refused(run, result));
        }
        if let Some(name) = &request.name
            && self.name_in_use(name)
        {
            let message = format!("a live worker is already named {name}; names are unique");
            let result = results::refused(&run, None, "name_in_use", &message);
            return Ok(self.admit_refused(run, result));
        }
        let max = self.config.limits.max_live_workers;
        if self.live_workers() >= max {
            let message = format!(
                "the limit of {max} live workers is reached; close a finished worker to free \
                 a slot"
            );
            let result = results::refused(&run, None, "limit_reached", &message);
            return Ok(self.admit_refused(run, result));
        }
        // The paths, executable and schema were checked off this task.
        if let Some((kind, message)) = preflight {
            let result = results::refused(&run, None, &kind, &message);
            return Ok(self.admit_refused(run, result));
        }

        let worker_id = self.ids.worker();
        let mut template = request.clone();
        template.prompt.clear();
        let session_id = template.session.resume.take();
        let seq = self.next_seq();
        let worker = Worker {
            id: worker_id.clone(),
            seq,
            name: request.name.clone(),
            provider: request.provider,
            template,
            origin,
            state: WorkerState::Idle,
            session_id,
            session_usage: None,
            current: None,
            queue: [run_id.clone()].into(),
            runs: vec![run_id.clone()],
            last_run: None,
            last_finish_seq: 0,
            closed: false,
        };
        let mut run = run;
        run.worker = Some(worker_id.clone());
        run.request = Some(request);
        self.workers.insert(worker_id.clone(), worker);
        self.runs.insert(run_id.clone(), run);
        self.refresh_worker(&worker_id);
        self.promote(&worker_id);
        self.dispatch();
        Ok(self.accepted(&run_id))
    }

    /// Queue another assignment on a worker, to continue its provider session.
    pub(super) fn follow_up(
        &mut self,
        worker_id: &str,
        prompt: String,
        timeout_secs: Option<u64>,
    ) -> Result<StartResponse, Error> {
        if self.mode != Mode::Active {
            return Err(Error::ShutDown);
        }
        let Some(worker) = self.workers.get(worker_id) else {
            return Err(Error::UnknownWorker(worker_id.to_string()));
        };
        let mut request = worker.template.clone();
        request.prompt = prompt;
        if let Some(timeout) = timeout_secs {
            request.timeout_secs = timeout;
        }
        let wref = worker.worker_ref();
        let run_id = self.ids.run();
        let mut run = Run::new(run_id.clone(), Kind::FollowUp, &request);
        run.worker = Some(worker_id.to_string());
        // A follow-up reports what its worker was started with, except that its
        // prompt, and its time limit when it gives one, come from this call.
        run.origin = worker.origin.clone();
        if !run.origin.sources.is_empty() {
            run.origin
                .sources
                .insert("prompt".to_string(), Layer::Argument);
            if timeout_secs.is_some() {
                run.origin
                    .sources
                    .insert("timeout_secs".to_string(), Layer::Argument);
            }
        }

        let worker = &self.workers[worker_id];
        let max = self.config.limits.max_queued_per_worker;
        let refusal: Option<(&str, String)> = if worker.closed {
            Some((
                "worker_closed",
                "the worker is closed and takes no more work; start a new worker".to_string(),
            ))
        } else if worker.template.session.ephemeral {
            Some((
                "no_session",
                "the worker was started ephemeral, so it has no session to continue; start a \
                 new worker"
                    .to_string(),
            ))
        } else if worker.queue.len() >= max
            && (worker.current.is_some() || !worker.queue.is_empty())
        {
            Some((
                "queue_full",
                format!(
                    "the worker already has {max} runs queued behind its running run; wait for \
                     one to finish"
                ),
            ))
        } else {
            None
        };
        if let Some((kind, message)) = refusal {
            let result = results::refused(&run, Some(wref), kind, &message);
            return Ok(self.admit_refused(run, result));
        }
        if let Err(problems) = request.validate() {
            let result = results::refused_invalid(&run, Some(wref), &problems);
            return Ok(self.admit_refused(run, result));
        }

        run.request = Some(request);
        self.runs.insert(run_id.clone(), run);
        if let Some(worker) = self.workers.get_mut(worker_id) {
            worker.queue.push_back(run_id.clone());
            worker.runs.push(run_id.clone());
        }
        self.refresh_worker(worker_id);
        self.promote(worker_id);
        self.dispatch();
        Ok(self.accepted(&run_id))
    }

    fn name_in_use(&self, name: &str) -> bool {
        self.workers
            .values()
            .any(|w| !w.closed && w.name.as_deref() == Some(name))
    }

    /// Record a run that was refused before it was queued, as finished.
    fn admit_refused(&mut self, mut run: Run, result: RunResult) -> StartResponse {
        let worker = run.worker.clone();
        let outcome = result.outcome;
        let error = result.error.clone();
        let id = run.id.clone();
        if let Ok(state) = run.state.transition(RunState::Finished) {
            run.state = state;
        }
        let seq = self.next_cursor();
        run.finished_seq = Some(seq);
        run.finished_at = Some(Instant::now());
        self.note_cleanup(&id, &result);
        run.bytes = super::retention::weigh(&result, &run.activity);
        self.retained_bytes += run.bytes;
        run.result = Some(result);
        self.finished.push_back(id.clone());
        if let Some(worker) = worker.as_ref().and_then(|w| self.workers.get_mut(w)) {
            worker.runs.push(id.clone());
            worker.last_finish_seq = seq;
        }
        self.runs.insert(id.clone(), run);
        StartResponse {
            worker,
            run: id,
            state: RunState::Finished,
            ahead: None,
            outcome: Some(outcome),
            error,
        }
    }

    /// The response for a run that was queued or started. A run that was
    /// dispatched at once and ended there, such as a follow-up with no session
    /// to continue, says how it ended.
    fn accepted(&self, run_id: &str) -> StartResponse {
        let run = &self.runs[run_id];
        let worker = run.worker.as_ref().and_then(|w| self.workers.get(w));
        let ahead = (run.state == RunState::Queued)
            .then(|| worker.map(|w| self.ahead_of(w, run_id)))
            .flatten();
        let finished = run.result.as_ref();
        StartResponse {
            worker: run.worker.clone(),
            run: run_id.to_string(),
            state: run.state,
            ahead,
            outcome: finished.map(|r| r.outcome),
            error: finished.and_then(|r| r.error.clone()),
        }
    }

    /// How many runs on `worker` come before `run_id`, the running one
    /// included.
    pub(super) fn ahead_of(&self, worker: &Worker, run_id: &str) -> usize {
        let queued = worker.queue.iter().position(|r| r == run_id).unwrap_or(0);
        queued + usize::from(worker.current.is_some())
    }
}
