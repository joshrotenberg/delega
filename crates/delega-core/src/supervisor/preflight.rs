//! The filesystem and schema work of admitting a request, done off the
//! supervisor's task.
//!
//! Each check runs on an OS thread of its own. A check on a filesystem that does
//! not answer keeps its thread until it does, so the number of checks that have
//! started and not ended is capped at
//! [`MAX_PENDING_PREFLIGHTS`](super::limits::MAX_PENDING_PREFLIGHTS). A check
//! that has timed out still counts, because its thread is still there.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::oneshot;

use crate::contract::request::Request;
use crate::provider::{self, ProviderOptions};

use super::limits;

/// The refusal a check found: its kind and message.
pub(super) type Refusal = (String, String);

/// What one check does. The supervisor uses the provider layer's dry run; the
/// tests replace it to hold checks open.
type Check = Arc<dyn Fn(&Request, &ProviderOptions) -> Option<Refusal> + Send + Sync>;

/// Releases the slot of one outstanding check when its thread ends, however it
/// ends.
struct Slot(Arc<AtomicUsize>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Runs request checks, at most [`limits::MAX_PENDING_PREFLIGHTS`] at a time.
/// Cheap to clone; every clone shares the count.
#[derive(Clone)]
pub(super) struct Preflight {
    check: Check,
    bound: Duration,
    pending: Arc<AtomicUsize>,
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

    /// Checks done by `check`, each waited for at most `bound`. Reachable only
    /// from the crate, so a caller of the library cannot replace the checks.
    pub(super) fn with(check: Check, bound: Duration) -> Self {
        Self {
            check,
            bound,
            pending: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Take a slot, or `None` when the cap is reached.
    fn reserve(&self) -> Option<Slot> {
        let mut n = self.pending.load(Ordering::Acquire);
        let taken = loop {
            if n >= limits::MAX_PENDING_PREFLIGHTS {
                break false;
            }
            match self
                .pending
                .compare_exchange_weak(n, n + 1, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => break true,
                Err(now) => n = now,
            }
        };
        taken.then(|| Slot(self.pending.clone()))
    }

    /// Check `request` on a thread of its own and wait for it, bounded. Returns
    /// the refusal, if any. A request that fails validation is left for the
    /// supervisor to refuse.
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
        let Some(slot) = self.reserve() else {
            return Some((
                "limit_reached".to_string(),
                format!(
                    "the limit of {} outstanding request checks is reached; earlier checks have \
                     not finished, so the filesystem or the provider executable may be \
                     unreachable",
                    limits::MAX_PENDING_PREFLIGHTS
                ),
            ));
        };
        let (request, options, check) = (request.clone(), options.clone(), self.check.clone());
        let (tx, rx) = oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name("delega-preflight".to_string())
            .spawn(move || {
                let refusal = check(&request, &options);
                // Free the slot before the answer is sent, so a caller that has
                // the answer never finds the slot still taken.
                drop(slot);
                let _ = tx.send(refusal);
            });
        if spawned.is_err() {
            return Some((
                "internal_error".to_string(),
                "checking the request could not be started".to_string(),
            ));
        }
        match tokio::time::timeout(self.bound, rx).await {
            Ok(Ok(refusal)) => refusal,
            Ok(Err(_)) => Some((
                "internal_error".to_string(),
                "checking the request failed unexpectedly".to_string(),
            )),
            Err(_) => Some((
                "preflight_timeout".to_string(),
                format!(
                    "checking the request did not finish within {} s; the filesystem or the \
                     provider executable may be unreachable",
                    self.bound.as_secs()
                ),
            )),
        }
    }
}
