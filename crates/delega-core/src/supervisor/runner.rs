//! The task that follows one run, and the watcher that notices if it dies.
//!
//! The supervisor's own task never waits on a provider. Each run gets a task
//! that prepares the run on a thread of its own, starts the provider through
//! the provider layer, waits for its one result, stops it when asked, and
//! reports the result back. A second small task joins the first, so a panic in
//! a run's task becomes a `failed` result for that run and touches nothing
//! else.

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::cleanup_reason::RunLost;
use crate::contract::request::Request;
use crate::provider::{self, ProviderOptions, RunHandle};

use super::command::Command;
use super::preflight::Preflight;

/// What starts a prepared run, called on the runtime with a token that stops
/// it: a child of the run's stop token.
pub(super) type Go = Box<dyn FnOnce(CancellationToken) -> RunHandle + Send>;

/// How a run is prepared and started. The call itself is the preparation: it
/// may block on the filesystem, so it runs on a thread of its own. The closure
/// it returns starts the run, on the runtime. Dropping that closure starts
/// nothing. The supervisor uses the provider layer; tests replace it to hold a
/// preparation open or to make a run's task fail.
pub(super) type Launch = Arc<dyn Fn(Request, ProviderOptions) -> Go + Send + Sync>;

/// The launcher that prepares and starts the provider layer's own run.
pub(super) fn provider_launch() -> Launch {
    Arc::new(|request, options| {
        let prepared = provider::prepare(&request, &options);
        Box::new(move |cancel| prepared.launch(cancel))
    })
}

/// Where a dispatched run is prepared, and by when.
pub(super) struct Preparation {
    pub preflight: Preflight,
    /// Taken when the supervisor dispatched the run, not when its task first
    /// ran.
    pub deadline: tokio::time::Instant,
}

/// Prepare the run, start it, follow it to its result and report it.
/// Interrupts the provider when `stop` is cancelled. A run that `stop` or the
/// deadline releases while it is being prepared is reported unprepared and
/// starts nothing.
pub(super) async fn drive(
    launch: Launch,
    preparation: Preparation,
    request: Request,
    options: ProviderOptions,
    stop: CancellationToken,
    tx: UnboundedSender<Command>,
    run: String,
) {
    let Preparation {
        preflight,
        deadline,
    } = preparation;
    let go = match preflight
        .prepare(move || launch(request, options), &stop, deadline)
        .await
    {
        Ok(go) => go,
        Err(how) => {
            let _ = tx.send(Command::Unprepared { run, how });
            return;
        }
    };
    // On a multi-threaded runtime a stop can land on another thread after the
    // answer was taken; nothing has started yet, so the run ends unprepared.
    if stop.is_cancelled() {
        drop(go);
        let _ = tx.send(Command::Unprepared {
            run,
            how: super::preflight::Unprepared::Stopped,
        });
        return;
    }
    // The provider gets a child of the run's stop from the outset, so a stop
    // that lands from here on reaches it at once, without waiting for this
    // task to forward it; the run then ends through its own result.
    let handle = go(stop.child_token());
    let interrupter = handle.interrupter();
    let mut wait = Box::pin(handle.wait());
    let result = tokio::select! {
        result = &mut wait => result,
        () = stop.cancelled() => {
            interrupter.interrupt();
            wait.await
        }
    };
    let _ = tx.send(Command::Finished {
        run,
        result: Box::new(result),
    });
}

/// Join a run's task. If it ended without reporting, say so.
pub(super) async fn watch(join: JoinHandle<()>, tx: UnboundedSender<Command>, run: String) {
    if let Err(error) = join.await {
        let reason = if error.is_panic() {
            RunLost::FollowerPanicked
        } else {
            RunLost::FollowerCancelled
        };
        let _ = tx.send(Command::Crashed { run, reason });
    }
}
