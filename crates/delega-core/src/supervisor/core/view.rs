//! Reading: `read`, `list` and the briefs the other responses share.

use std::time::Instant;

use super::super::error::Error;
use super::super::limits::{COMPACT_ACTIVITY, COMPACT_PARTIAL_TEXT_CHARS, MAX_ACTIVITY};
use super::super::record::{Run, Worker};
use super::super::state::RunState;
use super::super::views::{
    Detail, LimitsView, ListResponse, ProgressView, ReadResponse, ResultView, RunDiagnostics,
    Target, WorkerBrief, WorkerView,
};
use super::Core;

impl Core {
    /// The run a worker is on: its running run, else the next queued one, else
    /// its latest finished run if that is still kept.
    pub(super) fn current_run_of(&self, worker: &Worker) -> Option<String> {
        worker
            .current
            .clone()
            .or_else(|| worker.queue.front().cloned())
            .or_else(|| {
                worker
                    .last_run
                    .clone()
                    .filter(|r| self.runs.contains_key(r))
            })
    }

    pub(super) fn worker_brief(&self, worker: &Worker) -> WorkerBrief {
        let last = worker.last_run.as_ref().and_then(|r| self.runs.get(r));
        let pending_interactions = worker
            .current
            .as_ref()
            .and_then(|id| self.runs.get(id))
            .map_or_else(Vec::new, |run| run.pending());
        WorkerBrief {
            state: permission_state(worker.state, &pending_interactions),
            pending_interactions,
            worker: worker.id.clone(),
            current_run: worker
                .current
                .clone()
                .or_else(|| worker.queue.front().cloned()),
            queue_length: worker.queue.len(),
            last_run: last.map(|r| r.id.clone()),
            last_outcome: last.and_then(|r| r.result.as_ref().map(|r| r.outcome)),
            changed: None,
        }
    }

    /// Read a run, or a worker's current run.
    pub(super) fn read(
        &self,
        target: &Target,
        detail: Detail,
        after: Option<u64>,
        text_offset: Option<usize>,
    ) -> Result<ReadResponse, Error> {
        let (run_id, worker) = match target {
            Target::Run(id) => {
                let Some(run) = self.runs.get(id) else {
                    return Err(Error::UnknownRun(id.clone()));
                };
                (
                    Some(id.clone()),
                    run.worker.as_ref().and_then(|w| self.workers.get(w)),
                )
            }
            Target::Worker(id) => {
                let Some(worker) = self.workers.get(id) else {
                    return Err(Error::UnknownWorker(id.clone()));
                };
                (self.current_run_of(worker), Some(worker))
            }
        };
        let worker_state = match target {
            Target::Worker(_) => worker.map(|w| self.worker_brief(w).state),
            Target::Run(_) => None,
        };
        let worker_id = worker.map(|w| w.id.clone());
        let cursor = self.current_cursor();
        let Some(run) = run_id.as_ref().and_then(|id| self.runs.get(id)) else {
            return Ok(ReadResponse {
                pending_interactions: Vec::new(),
                worker: worker_id,
                worker_state,
                run: None,
                state: None,
                outcome: None,
                cursor,
                result: None,
                text_window: None,
                progress: None,
                note: Some("no run of this worker is kept: its results have aged out".to_string()),
            });
        };
        let finished = run.state == RunState::Finished;
        let progress =
            (!finished || detail == Detail::Full).then(|| self.progress_view(run, detail, after));
        let windowed = run
            .result
            .as_ref()
            .map(|r| ResultView::windowed(r, detail, text_offset));
        Ok(ReadResponse {
            pending_interactions: run.pending(),
            worker: worker_id.or_else(|| run.worker.clone()),
            worker_state,
            run: Some(run.id.clone()),
            state: Some(run.state),
            outcome: run.result.as_ref().map(|r| r.outcome),
            cursor,
            result: windowed.as_ref().map(|(view, _)| view.clone()),
            text_window: windowed.and_then(|(_, window)| window),
            progress,
            note: None,
        })
    }

    fn progress_view(&self, run: &Run, detail: Detail, after: Option<u64>) -> ProgressView {
        let snapshot = run.activity.snapshot();
        let full = detail == Detail::Full;
        let now = Instant::now();
        let queued_ms = match run.started {
            Some(started) => millis(started.saturating_duration_since(run.queued)),
            None => millis(now.saturating_duration_since(run.queued)),
        };
        let running_ms = run.started.map(|s| {
            let end = run.finished_at.unwrap_or(now);
            millis(end.saturating_duration_since(s))
        });
        let (partial_text, cut) = if full {
            (snapshot.text.clone(), false)
        } else {
            tail_chars(&snapshot.text, COMPACT_PARTIAL_TEXT_CHARS)
        };
        let after_cursor = after.unwrap_or(0);
        let mut entries: Vec<_> = snapshot
            .entries
            .iter()
            .filter(|e| e.cursor > after_cursor)
            .cloned()
            .collect();
        if !full && entries.len() > COMPACT_ACTIVITY {
            entries.drain(..entries.len() - COMPACT_ACTIVITY);
        }
        let activity_omitted = snapshot.total - entries.len() as u64;
        let diagnostics = full.then(|| RunDiagnostics {
            provider: run.provider,
            mode: run.mode,
            queued_at: crate::process::rfc3339_utc(run.queued_at),
            started_at: run.started_at.map(crate::process::rfc3339_utc),
            stop_requested: run.stop_reason.map(super::super::record::StopReason::word),
            activity_dropped: snapshot.omitted,
        });
        debug_assert!(entries.len() <= MAX_ACTIVITY);
        ProgressView {
            queued_ms,
            running_ms,
            timeout_secs: run.timeout_secs,
            partial_text,
            partial_text_truncated: snapshot.text_truncated || cut,
            activity: entries,
            activity_total: snapshot.total,
            activity_omitted,
            diagnostics,
        }
    }

    /// Every worker, with the limits and what is in use.
    pub(super) fn list(&self) -> ListResponse {
        let limits = &self.config.limits;
        let mut workers: Vec<&Worker> = self.workers.values().collect();
        workers.sort_by_key(|w| w.seq);
        ListResponse {
            cursor: self.current_cursor(),
            limits: LimitsView {
                max_live_workers: limits.max_live_workers,
                live_workers: self.live_workers(),
                max_running_runs: limits.max_running_runs,
                running_runs: self.running,
                max_queued_per_worker: limits.max_queued_per_worker,
                retained_results: limits.retained_results,
                retained_now: self.finished.len(),
                retained_bytes: limits.retained_bytes,
                retained_bytes_now: self.retained_bytes,
                retention_secs: limits.retention.as_secs(),
                wait_ceiling_secs: limits.wait_ceiling.as_secs(),
                default_wait_secs: limits.wait_default.as_secs(),
                max_waits_in_flight: limits.max_waits_in_flight,
                waits_in_flight: self.waits_in_flight(),
            },
            workers: workers
                .into_iter()
                .map(|w| WorkerView {
                    brief: self.worker_brief(w),
                    name: w.name.clone(),
                    provider: w.provider,
                    session_id: w.session_id.clone(),
                    can_follow_up: !w.closed
                        && !w.template.session.ephemeral
                        && (w.session_id.is_some() || w.current.is_some() || !w.queue.is_empty()),
                    runs: w.runs.len(),
                })
                .collect(),
        }
    }
}

fn millis(d: std::time::Duration) -> u64 {
    d.as_millis().min(u128::from(u64::MAX)) as u64
}

/// The last `max` characters of `text`, and whether anything was cut.
fn tail_chars(text: &str, max: usize) -> (String, bool) {
    let chars = text.chars().count();
    if chars <= max {
        return (text.to_string(), false);
    }
    let cut = text.char_indices().nth(chars - max).map_or(0, |(i, _)| i);
    (text[cut..].to_string(), true)
}

fn permission_state(
    state: crate::supervisor::WorkerState,
    pending: &[crate::contract::result::PermissionInteraction],
) -> crate::supervisor::WorkerState {
    use crate::supervisor::WorkerState;
    if state == WorkerState::Running && !pending.is_empty() {
        WorkerState::WaitingForAnswer
    } else {
        state
    }
}
