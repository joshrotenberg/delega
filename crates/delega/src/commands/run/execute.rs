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

/// How long dropping the runtime waits for blocking helper threads. The run
/// has already ended by then, so this only bounds a stuck helper.
const RUNTIME_SHUTDOWN_WAIT: Duration = Duration::from_secs(5);
/// How long writing the result may take once a signal has already been
/// received during the run. That signal was used up stopping the worker, so a
/// reader that never reads would otherwise hold `delega` with no signal left
/// to end it.
const WRITE_WAIT_AFTER_SIGNAL: Duration = Duration::from_secs(3);
/// How long a signal or the write deadline waits for the print thread to
/// report before exiting. A write already under way when the signal came may
/// finish in that time, and its result is then delivered, so the status must
/// follow it. A write held by a reader that does not read is given up on.
const PRINT_REPORT_GRACE: Duration = Duration::from_millis(250);
/// Stands for "no limit" when no signal has been received: a year.
const WRITE_WAIT_UNLIMITED: Duration = Duration::from_secs(365 * 24 * 3600);

/// Run `request`, give the result to `print` and return the exit status.
///
/// `print` runs on a thread of its own, while the signal handlers stay
/// installed, because writing the result waits on whoever reads standard
/// output. A signal during that wait, or the end of the write deadline, ends
/// `delega` within [`PRINT_REPORT_GRACE`]; the worker is already gone by then.
/// The exit status is the one `print` returned when `print` returns within
/// that grace or had already returned, because the result was then written and
/// a status must not contradict it. Otherwise it is the outcome's status, or
/// 130 for a completed run, because the result was not delivered. A failure to
/// set up the runtime or the
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
        // Set by the print thread once `print` has returned, so before it sends
        // the same code: from then on the result has been written, and that is
        // the status to exit with, whatever else happens first.
        let printed: Arc<OnceLock<ExitCode>> = Arc::new(OnceLock::new());
        let printer = Arc::clone(&printed);
        std::thread::spawn(move || {
            let code = print(result);
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
        tokio::select! {
            // A print result that is ready wins over a signal ready at once.
            biased;
            code = &mut receiver => code.unwrap_or(ExitCode::from(1)),
            () = tokio::time::sleep(patience) => {
                log.line("delega run: the result could not be written in time after the signal; exiting");
                cut_short(receiver, &printed, outcome_status).await
            }
            // A handler that stopped delivering leaves only the first branch.
            Some(()) = signals.next() => {
                log.line("delega run: signal received while writing the result; exiting");
                cut_short(receiver, &printed, outcome_status).await
            }
        }
    });
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_WAIT);
    code
}

/// The status to exit with when a signal or the write deadline ends the wait
/// for the print thread. The print thread's own status when it reports within
/// [`PRINT_REPORT_GRACE`] or had already returned from `print`, because the
/// result was then written. Otherwise the cut-short status, because the result
/// was not delivered.
async fn cut_short(
    receiver: std::pin::Pin<&mut tokio::sync::oneshot::Receiver<ExitCode>>,
    printed: &OnceLock<ExitCode>,
    outcome_status: u8,
) -> ExitCode {
    if let Ok(Ok(code)) = tokio::time::timeout(PRINT_REPORT_GRACE, receiver).await {
        return code;
    }
    printed
        .get()
        .copied()
        .unwrap_or_else(|| ExitCode::from(exit_status_when_cut_short(outcome_status)))
}

/// Test seam, in debug builds only: with `DELEGA_TEST_HOLD_AFTER_PRINT` naming a
/// file, the print thread creates that file once the result is written and then
/// waits up to 30 seconds before reporting its exit code. A test can signal
/// `delega` inside that window without depending on timing.
#[cfg(debug_assertions)]
fn hold_after_print() {
    if let Some(path) = std::env::var_os("DELEGA_TEST_HOLD_AFTER_PRINT")
        && std::fs::write(&path, b"").is_ok()
    {
        std::thread::sleep(Duration::from_secs(30));
    }
}

#[cfg(not(debug_assertions))]
const fn hold_after_print() {}
