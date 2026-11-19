//! Running the request on a Tokio runtime until its one result, and handing the
//! result to the writer.

use std::process::ExitCode;
use std::time::Duration;

use delega_core::contract::request::Request;
use delega_core::contract::result::RunResult;
use delega_core::provider::{ProviderOptions, start};
use tokio::runtime::Builder;

use super::log::Log;
use super::output::refused_result;
use super::request::Facts;
use super::signals::{Signals, exit_status_when_cut_short, follow};

/// How long dropping the runtime waits for blocking helper threads. The run
/// has already ended by then, so this only bounds a stuck helper.
const RUNTIME_SHUTDOWN_WAIT: Duration = Duration::from_secs(5);
/// How long writing the result may take once a signal has already been
/// received during the run. That signal was used up stopping the worker, so a
/// reader that never reads would otherwise hold `delega` with no signal left
/// to end it.
const WRITE_WAIT_AFTER_SIGNAL: Duration = Duration::from_secs(3);
/// Stands for "no limit" when no signal has been received: a year.
const WRITE_WAIT_UNLIMITED: Duration = Duration::from_secs(365 * 24 * 3600);

/// Run `request`, give the result to `print` and return the exit status.
///
/// `print` runs on a thread of its own, while the signal handlers stay
/// installed, because writing the result waits on whoever reads standard
/// output. A signal during that wait ends `delega` at once, with the outcome's
/// exit status (130 for a completed run, whose result was not delivered); the
/// worker is already gone by then. A failure to set up the runtime or the
/// signal handlers is a refused result: nothing was started, and starting a
/// worker `delega` could not stop would be worse.
pub fn run<F>(
    request: Request,
    options: ProviderOptions,
    facts: &Facts,
    log: &Log,
    print: F,
) -> ExitCode
where
    F: FnOnce(RunResult) -> ExitCode + Send + 'static,
{
    let runtime = match Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(err) => {
            return print(refused_result(
                facts,
                "runtime_unavailable",
                &format!("cannot start the async runtime: {err}"),
            ));
        }
    };
    let code = runtime.block_on(async {
        let mut signals = match Signals::install() {
            Ok(signals) => signals,
            Err(err) => {
                return print(refused_result(
                    facts,
                    "signals_unavailable",
                    &format!("cannot install the signal handlers that stop the worker: {err}"),
                ));
            }
        };
        let (result, signalled) = follow(start(request, options), &mut signals, log).await;
        let outcome_status = result.outcome.exit_status();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let _ = sender.send(print(result));
        });
        tokio::pin!(receiver);
        let patience = if signalled {
            WRITE_WAIT_AFTER_SIGNAL
        } else {
            WRITE_WAIT_UNLIMITED
        };
        tokio::select! {
            code = &mut receiver => code.unwrap_or(ExitCode::from(1)),
            () = tokio::time::sleep(patience) => {
                log.line("delega run: the result could not be written in time after the signal; exiting");
                ExitCode::from(exit_status_when_cut_short(outcome_status))
            }
            // A handler that stopped delivering leaves only the first branch.
            Some(()) = signals.next() => {
                log.line("delega run: signal received while writing the result; exiting");
                ExitCode::from(exit_status_when_cut_short(outcome_status))
            }
        }
    });
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_WAIT);
    code
}
