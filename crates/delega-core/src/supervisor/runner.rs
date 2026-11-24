//! The task that follows one run, and the watcher that notices if it dies.
//!
//! The supervisor's own task never waits on a provider. Each run gets a task
//! that starts the provider through the provider layer, waits for its one
//! result, stops it when asked, and reports the result back. A second small
//! task joins the first, so a panic in a run's task becomes a `failed` result
//! for that run and touches nothing else.

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::cleanup_reason::RunLost;
use crate::contract::request::Request;
use crate::provider::{self, ProviderOptions, RunHandle};

use super::command::Command;

/// How a run is started. The supervisor uses the provider layer; tests that
/// need a run's task to fail replace it.
pub(super) type Launch = Arc<dyn Fn(Request, ProviderOptions) -> RunHandle + Send + Sync>;

/// The launcher that starts the provider layer's own run.
pub(super) fn provider_launch() -> Launch {
    Arc::new(provider::start)
}

/// Start the run, follow it to its result and report it. Interrupts the
/// provider when `stop` is cancelled.
pub(super) async fn drive(
    launch: Launch,
    request: Request,
    options: ProviderOptions,
    stop: CancellationToken,
    tx: UnboundedSender<Command>,
    run: String,
) {
    let handle = launch(request, options);
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
