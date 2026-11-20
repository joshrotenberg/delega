//! Running the request on a Tokio runtime until its one result, and handing the
//! result to the writer.

use std::process::ExitCode;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use delega_core::contract::request::Request;
use delega_core::contract::result::RunResult;
use delega_core::provider::{ProviderOptions, start};
use tokio::runtime::Builder;

use super::log::Log;
use super::output::refused_result;
use super::request::Facts;
use super::signals::{Signals, exit_status_when_cut_short, follow};
use crate::commands::output::Stop;

/// How long dropping the runtime waits for blocking helper threads. The run
/// has already ended by then, so this only bounds a stuck helper.
const RUNTIME_SHUTDOWN_WAIT: Duration = Duration::from_secs(5);
/// How long writing the result may take once a signal has already been
/// received during the run. That signal was used up stopping the worker, so a
/// reader that never reads would otherwise hold `delega` with no signal left
/// to end it.
const WRITE_WAIT_AFTER_SIGNAL: Duration = Duration::from_secs(3);
/// How long a signal or the write deadline waits, after asking the write to
/// stop, for the print thread to say how it ended. When no piece blocks, a
/// write in progress stops within one poll interval and one piece (see
/// [`write_unless_stopped`](crate::commands::output::write_unless_stopped));
/// the rest of the bound covers building the document before the write begins,
/// and a piece that blocks after `poll`.
const PRINT_STOP_WAIT: Duration = Duration::from_secs(1);
/// Stands for "no limit" when no signal has been received: a year.
const WRITE_WAIT_UNLIMITED: Duration = Duration::from_secs(365 * 24 * 3600);

/// Run `request`, give the result to `print` and return the exit status.
///
/// `print` runs on a thread of its own, while the signal handlers stay
/// installed, because writing the result waits on whoever reads standard
/// output. It writes through the [`Stop`] it is given. A signal during that
/// wait, or the end of the write deadline, asks the write to stop and waits up
/// to [`PRINT_STOP_WAIT`] for the print thread to say how it ended; the worker
/// is already gone by then. The status follows how the write ended, as the
/// print thread reports it: a write that finished, the whole result accepted by the kernel,
/// exits with the status `print` returned; a write that stopped with bytes left
/// unwritten exits with the outcome's status, or 130 for a completed run,
/// because the result was not delivered. Standard output may then hold the
/// first part of the document. A print thread that does not answer within
/// `PRINT_STOP_WAIT` also gets the cut-short status.
///
/// The status is exact except in two cases. The last piece can reach standard
/// output before the print thread records that the write finished; if that
/// thread is then not scheduled for longer than `PRINT_STOP_WAIT` (the
/// process is stopped or paused, for example), a complete result is followed
/// by the cut-short status. And a piece can block after `poll` reported
/// standard output writable (see
/// [`write_unless_stopped`](crate::commands::output::write_unless_stopped));
/// the stop then waits `PRINT_STOP_WAIT` and gives the cut-short status, which
/// is wrong only if that piece completes the result as `delega` exits.
///
/// A failure to set up the runtime or the signal handlers is a refused result:
/// nothing was started, and starting a worker `delega` could not stop would be
/// worse.
pub fn run<F>(
    request: Request,
    options: ProviderOptions,
    facts: &Facts,
    log: &Log,
    print: F,
) -> ExitCode
where
    F: FnOnce(RunResult, &Stop) -> ExitCode + Send + 'static,
{
    let runtime = match Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(err) => {
            return print(
                refused_result(
                    facts,
                    "runtime_unavailable",
                    &format!("cannot start the async runtime: {err}"),
                ),
                &Stop::default(),
            );
        }
    };
    let code = runtime.block_on(async {
        let mut signals = match Signals::install() {
            Ok(signals) => signals,
            Err(err) => {
                return print(
                    refused_result(
                        facts,
                        "signals_unavailable",
                        &format!("cannot install the signal handlers that stop the worker: {err}"),
                    ),
                    &Stop::default(),
                );
            }
        };
        let (result, signalled) = follow(start(request, options), &mut signals, log).await;
        let outcome_status = result.outcome.exit_status();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let stop = Arc::new(Stop::default());
        // Set by the print thread once `print` has returned, before it sends the
        // same code, so the code is known even if the send is late.
        let printed: Arc<OnceLock<ExitCode>> = Arc::new(OnceLock::new());
        let (writer_stop, printer) = (Arc::clone(&stop), Arc::clone(&printed));
        std::thread::spawn(move || {
            let code = print(result, &writer_stop);
            let _ = printer.set(code);
            hold_after_print();
            let _ = sender.send(code);
        });
        tokio::pin!(receiver);
        let patience = if signalled {
            WRITE_WAIT_AFTER_SIGNAL
        } else {
            WRITE_WAIT_UNLIMITED
        };
        let ended = tokio::select! {
            // A print result that is ready wins over a signal ready at once.
            biased;
            // A print thread that panicked delivered nothing it could vouch for.
            code = &mut receiver => Some(code.unwrap_or(ExitCode::from(1))),
            () = tokio::time::sleep(patience) => {
                log.line("delega run: the result could not be written in time after the signal; exiting");
                stop_writing(receiver, &stop, &printed).await
            }
            // A handler that stopped delivering leaves only the first branch.
            Some(()) = signals.next() => {
                log.line("delega run: signal received while writing the result; exiting");
                stop_writing(receiver, &stop, &printed).await
            }
        };
        match ended {
            Some(code) if !stop.stopped() => code,
            // The write stopped with bytes left unwritten, or the print thread
            // did not say how it ended in time (it never returned from `print`).
            _ => ExitCode::from(exit_status_when_cut_short(outcome_status)),
        }
    });
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_WAIT);
    code
}

/// Ask the write to stop and wait, up to [`PRINT_STOP_WAIT`], for the code
/// `print` returned. `None` when `print` has not returned by then. Whether the
/// write finished or stopped is then in `stop`.
async fn stop_writing(
    receiver: std::pin::Pin<&mut tokio::sync::oneshot::Receiver<ExitCode>>,
    stop: &Stop,
    printed: &OnceLock<ExitCode>,
) -> Option<ExitCode> {
    stop.request();
    match tokio::time::timeout(PRINT_STOP_WAIT, receiver).await {
        Ok(Ok(code)) => Some(code),
        _ => printed.get().copied(),
    }
}

/// Test seam, in debug builds only: with `DELEGA_TEST_HOLD_AFTER_PRINT` naming a
/// file that does not exist, the print thread creates it once the result is
/// written and then waits up to 30 seconds before reporting its exit code. A
/// test can signal `delega` inside that window without depending on timing.
/// The file is created new, so an existing path, a FIFO included, is never
/// opened and cannot block the thread.
#[cfg(debug_assertions)]
fn hold_after_print() {
    if let Some(path) = std::env::var_os("DELEGA_TEST_HOLD_AFTER_PRINT")
        && std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .is_ok()
    {
        std::thread::sleep(Duration::from_secs(30));
    }
}

#[cfg(not(debug_assertions))]
const fn hold_after_print() {}
