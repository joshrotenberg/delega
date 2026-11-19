//! The filesystem and schema work of admitting a request, done off the
//! supervisor's task.
//!
//! Each check runs on an OS thread of its own. A check on a filesystem that does
//! not answer keeps its thread until it does, so the number of checks that have
//! started and not ended is capped at
//! [`MAX_PENDING_PREFLIGHTS`](super::limits::MAX_PENDING_PREFLIGHTS). A check
//! that has timed out still counts, because its thread is still there.

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

/// Runs request checks, at most [`limits::MAX_PENDING_PREFLIGHTS`] at a time.
/// Cheap to clone; every clone shares the slots.
#[derive(Clone)]
pub(super) struct Preflight {
    check: Check,
    bound: Duration,
    /// One permit per outstanding check. The permit moves into the check's
    /// thread and is released when the thread ends, however it ends.
    slots: Arc<Semaphore>,
    /// Cancelled when the supervisor shuts down or is dropped, so a `start`
    /// waiting for a slot or an answer is released at once.
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
        let deadline = Instant::now() + self.bound;
        let acquire = self.slots.clone().acquire_owned();
        let permit = tokio::select! {
            biased;
            () = self.closed.cancelled() => return None,
            got = tokio::time::timeout_at(deadline, acquire) => match got {
                Ok(Ok(permit)) => permit,
                Ok(Err(_)) => return Some(internal("checking the request failed unexpectedly")),
                Err(_) => return Some(self.timed_out(
                    "request checks are not completing, so none was free",
                )),
            },
        };
        let (request, options, check) = (request.clone(), options.clone(), self.check.clone());
        let (tx, rx) = oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name("delega-preflight".to_string())
            .spawn(move || {
                let refusal = check(&request, &options);
                // Free the slot before the answer is sent, so a caller that has
                // the answer never finds the slot still taken. A panic in the
                // check drops the permit on the way out.
                drop(permit);
                let _ = tx.send(refusal);
            });
        if spawned.is_err() {
            return Some(internal("checking the request could not be started"));
        }
        tokio::select! {
            biased;
            () = self.closed.cancelled() => None,
            got = tokio::time::timeout_at(deadline, rx) => match got {
                Ok(Ok(refusal)) => refusal,
                Ok(Err(_)) => Some(internal("checking the request failed unexpectedly")),
                Err(_) => Some(self.timed_out("the check did not finish")),
            },
        }
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

fn internal(message: &str) -> Refusal {
    ("internal_error".to_string(), message.to_string())
}
