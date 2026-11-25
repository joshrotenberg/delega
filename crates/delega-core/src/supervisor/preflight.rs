//! The filesystem and schema work of admitting a request, and of preparing a
//! run when it is dispatched, done off the supervisor's task and off the
//! runtime.
//!
//! Each check and each preparation runs on an OS thread of its own. One on a
//! filesystem that does not answer keeps its thread until it does, so the
//! number that have started and not ended is capped at
//! [`MAX_PENDING_PREFLIGHTS`](super::limits::MAX_PENDING_PREFLIGHTS), checks and
//! preparations together. One that has timed out still counts, because its
//! thread is still there.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Semaphore, oneshot};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::contract::request::Request;
use crate::provider::{self, ProviderOptions};

use super::limits;

/// The refusal a check found: its kind and message.
pub(super) type Refusal = (String, String);

/// What one check does. The supervisor uses the provider layer's dry run; the
/// tests replace it to hold checks open.
type Check = Arc<dyn Fn(&Request, &ProviderOptions) -> Option<Refusal> + Send + Sync>;

/// Runs request checks and run preparations, at most
/// [`limits::MAX_PENDING_PREFLIGHTS`] at a time. Cheap to clone; every clone
/// shares the slots.
#[derive(Clone)]
pub(super) struct Preflight {
    check: Check,
    bound: Duration,
    /// One permit per outstanding check or preparation. The permit moves into
    /// its thread and is released when the thread ends, however it ends.
    slots: Arc<Semaphore>,
    /// Cancelled when the supervisor shuts down or is dropped, so a `start` or
    /// a preparation waiting for a slot or an answer is released at once.
    closed: CancellationToken,
}

impl Preflight {
    /// The checks the supervisor runs: the provider layer's dry run, bounded by
    /// [`limits::PREFLIGHT_BOUND`].
    pub(super) fn provider() -> Self {
        Self::with(
            Arc::new(|request, options| {
                provider::dry_run(request, options)
                    .err()
                    .map(|refusal| (refusal.kind, refusal.message))
            }),
            limits::PREFLIGHT_BOUND,
        )
    }

    /// Checks done by `check`, each given at most `bound` from the start of the
    /// `start` that asked for it, waiting for a slot included. Reachable only
    /// from the crate, so a caller of the library cannot replace the checks.
    pub(super) fn with(check: Check, bound: Duration) -> Self {
        Self {
            check,
            bound,
            slots: Arc::new(Semaphore::new(limits::MAX_PENDING_PREFLIGHTS)),
            closed: CancellationToken::new(),
        }
    }

    /// Release every `start` that is waiting, and every later one. They are
    /// answered as if nothing was refused; `Supervisor::start` sees
    /// [`is_closed`](Self::is_closed) and answers `Error::ShutDown`.
    pub(super) fn close(&self) {
        self.closed.cancel();
    }

    /// Whether [`close`](Self::close) has been called.
    pub(super) fn is_closed(&self) -> bool {
        self.closed.is_cancelled()
    }

    /// Check `request` on a thread of its own and wait for it. Returns the
    /// refusal, if any. A request that fails validation is left for the
    /// supervisor to refuse.
    ///
    /// When every slot is taken the call waits for one, asynchronously and
    /// without a thread, so a burst of concurrent starts is served in turn and
    /// not refused. Waiting for a slot and for the answer share one bound,
    /// measured from the start of the call: a start is never held longer than
    /// the bound, and one that gets no slot or no answer in time is refused
    /// `preflight_timeout`.
    ///
    /// The thread is detached: nothing waits for it. If the bound passes, or the
    /// runtime or the supervisor goes away first, its late result is discarded
    /// and the thread ends on its own when the filesystem answers, so a hung
    /// filesystem cannot hold up a shutdown or a runtime drop. Its slot is held
    /// until then.
    pub(super) async fn run(
        &self,
        request: &Request,
        options: &ProviderOptions,
    ) -> Option<Refusal> {
        if request.validate().is_err() {
            return None;
        }
        let (request, options, check) = (request.clone(), options.clone(), self.check.clone());
        let what = Work {
            name: "delega-preflight",
            no_slot: "no request check slot was free",
            not_done: "the check did not finish",
            failed: "checking the request failed unexpectedly",
            not_started: "checking the request could not be started",
        };
        match self
            .on_thread(what, move || check(&request, &options), None)
            .await
        {
            Ok(refusal) => refusal,
            Err(Ended::Released) => None,
            Err(Ended::Refused(refusal)) => Some(refusal),
        }
    }

    /// Do `work`, the preparation of a run being dispatched, on a thread of its
    /// own and wait for it, as [`run`](Self::run) does a check: in one of the
    /// same slots and within the same bound, measured from the start of this
    /// call. `stop` and [`close`](Self::close) release the wait at once.
    ///
    /// Returns what `work` returned, or why it did not: a refusal, with kind
    /// `preflight_timeout` when the bound passed, or [`Unprepared::Stopped`]. A
    /// preparation that returns after the wait has ended is dropped on its
    /// thread, so it starts nothing.
    pub(super) async fn prepare<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
        stop: &CancellationToken,
    ) -> Result<T, Unprepared> {
        let what = Work {
            name: "delega-prepare",
            no_slot: "no slot was free to prepare the run",
            not_done: "preparing the run did not finish",
            failed: "preparing the run failed unexpectedly",
            not_started: "preparing the run could not be started",
        };
        self.on_thread(what, work, Some(stop))
            .await
            .map_err(|ended| match ended {
                Ended::Released => Unprepared::Stopped,
                Ended::Refused(refusal) => Unprepared::Refused(refusal),
            })
    }

    /// Run `work` on a thread of its own in one slot, within the bound, unless
    /// [`close`](Self::close) or `stop` releases the wait first.
    async fn on_thread<T: Send + 'static>(
        &self,
        what: Work,
        work: impl FnOnce() -> T + Send + 'static,
        stop: Option<&CancellationToken>,
    ) -> Result<T, Ended> {
        let released = async {
            match stop {
                Some(stop) => {
                    tokio::select! {
                        () = self.closed.cancelled() => {}
                        () = stop.cancelled() => {}
                    }
                }
                None => self.closed.cancelled().await,
            }
        };
        tokio::pin!(released);
        let deadline = Instant::now() + self.bound;
        let acquire = self.slots.clone().acquire_owned();
        let permit = tokio::select! {
            biased;
            () = &mut released => return Err(Ended::Released),
            got = tokio::time::timeout_at(deadline, acquire) => match got {
                Ok(Ok(permit)) => permit,
                Ok(Err(_)) => return Err(Ended::Refused(internal(what.failed))),
                Err(_) => return Err(Ended::Refused(self.timed_out(what.no_slot))),
            },
        };
        let (tx, rx) = oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name(what.name.to_string())
            .spawn(move || {
                let answer = work();
                // Free the slot before the answer is sent, so a caller that has
                // the answer never finds the slot still taken. A panic in the
                // work drops the permit on the way out.
                drop(permit);
                // A caller that stopped waiting has dropped the receiver; the
                // answer comes back and is dropped here, on this thread.
                let _ = tx.send(answer);
            });
        if spawned.is_err() {
            return Err(Ended::Refused(internal(what.not_started)));
        }
        tokio::select! {
            biased;
            () = &mut released => Err(Ended::Released),
            got = tokio::time::timeout_at(deadline, rx) => match got {
                Ok(Ok(answer)) => Ok(answer),
                Ok(Err(_)) => Err(Ended::Refused(internal(what.failed))),
                Err(_) => Err(Ended::Refused(self.timed_out(what.not_done))),
            },
        }
    }

    /// The slots, shared with every clone, so a test can take and free them
    /// itself and decide exactly when a waiting `start` gets one.
    #[cfg(test)]
    pub(super) fn slots(&self) -> Arc<Semaphore> {
        self.slots.clone()
    }

    fn timed_out(&self, what: &str) -> Refusal {
        (
            "preflight_timeout".to_string(),
            format!(
                "{what} within {} s; request checks are not completing, and the filesystem or \
                 the provider executable may be unreachable",
                self.bound.as_secs()
            ),
        )
    }
}

/// Why a dispatched run's preparation gave no answer.
#[derive(Debug)]
pub(super) enum Unprepared {
    /// The run is refused before start: the bound passed (`preflight_timeout`)
    /// or the preparation failed.
    Refused(Refusal),
    /// An interrupt, close or shutdown released the run while it was being
    /// prepared.
    Stopped,
}

/// How [`Preflight::on_thread`] ended without an answer.
enum Ended {
    /// The supervisor closed, or the caller's stop token fired.
    Released,
    Refused(Refusal),
}

/// The thread name and the messages of one kind of work.
struct Work {
    name: &'static str,
    no_slot: &'static str,
    not_done: &'static str,
    failed: &'static str,
    not_started: &'static str,
}

fn internal(message: &str) -> Refusal {
    ("internal_error".to_string(), message.to_string())
}
