//! The supervisor's task: receive a command, apply it, repeat.

use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::Instant;

use super::command::Command;
use super::core::Core;

/// Set when the task has ended, so a `Drop` that blocks can stop waiting.
#[derive(Debug, Default)]
pub(super) struct Done {
    ended: Mutex<bool>,
    changed: Condvar,
}

impl Done {
    fn set(&self) {
        *self.ended.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.changed.notify_all();
    }

    /// Block the calling thread until the task has ended or `limit` passes.
    pub fn wait(&self, limit: Duration) {
        let guard = self.ended.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = self
            .changed
            .wait_timeout_while(guard, limit, |ended| !*ended)
            .unwrap_or_else(PoisonError::into_inner);
    }
}

/// Apply commands until told to quit, then until every run has settled or the
/// bound passes. Whatever has not settled by then is left to the provider layer,
/// which stops the child of a run that is no longer followed.
pub(super) async fn run(
    mut core: Core,
    mut commands: UnboundedReceiver<Command>,
    done: Arc<Done>,
    quit_bound: Duration,
) {
    let mut deadline: Option<Instant> = None;
    loop {
        let command = match deadline {
            Some(at) => tokio::select! {
                command = commands.recv() => command,
                () = tokio::time::sleep_until(at) => break,
            },
            None => commands.recv().await,
        };
        let Some(command) = command else {
            break;
        };
        if matches!(command, Command::Quit) {
            deadline.get_or_insert_with(|| Instant::now() + quit_bound);
        }
        core.handle(command);
        if deadline.is_some() && core.running() == 0 {
            break;
        }
    }
    core.abort_all();
    done.set();
}
