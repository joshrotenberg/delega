//! The supervisor's task: receive a command, apply it, repeat.

use std::time::Duration;

use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::Instant;

use super::command::Command;
use super::core::Core;

/// Apply commands until told to quit, then until every run has settled or the
/// bound passes. Whatever has not settled by then is left to the provider layer,
/// which stops the child of a run that is no longer followed.
pub(super) async fn run(
    mut core: Core,
    mut commands: UnboundedReceiver<Command>,
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
}
