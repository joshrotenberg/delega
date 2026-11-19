//! Stopping a run when `delega` itself is signalled.
//!
//! The provider process leads a session of its own, so a Ctrl-C at the terminal
//! reaches only `delega`, never the worker. `delega` therefore has to turn
//! SIGINT and SIGTERM into a stop of the worker.
//!
//! The first signal interrupts the run through its handle. The run then stops
//! the worker and its descendants, which the process layer bounds in time
//! (its cleanup deadline), and the result carries the cleanup verdict. Any
//! further signal while that goes on is acknowledged once on stderr and
//! otherwise ignored, so an impatient second Ctrl-C cannot end `delega` while
//! processes are still being stopped. Nothing waits without a bound: the
//! result arrives when cleanup finishes or its deadline passes.

use std::io;

use delega_core::contract::result::RunResult;
use delega_core::provider::RunHandle;
use tokio::signal::unix::{Signal, SignalKind, signal};

use crate::commands::output::report;

/// Handlers for the two signals that stop a run.
pub struct Signals {
    interrupt: Signal,
    terminate: Signal,
}

impl Signals {
    /// Install the handlers. Done before the worker starts, so no signal can
    /// arrive while a worker runs and `delega` has no handler for it.
    pub fn install() -> io::Result<Self> {
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
        })
    }

    /// Wait for either signal. `None` means a handler stopped delivering.
    async fn next(&mut self) -> Option<()> {
        tokio::select! {
            got = self.interrupt.recv() => got,
            got = self.terminate.recv() => got,
        }
    }
}

/// Wait for the run's result, stopping the run on the first signal.
pub async fn follow(handle: RunHandle, mut signals: Signals) -> RunResult {
    let interrupter = handle.interrupter();
    let wait = handle.wait();
    tokio::pin!(wait);
    let mut stopping = false;
    let mut told = false;
    loop {
        tokio::select! {
            result = &mut wait => return result,
            got = signals.next() => {
                if got.is_none() {
                    // The handlers are gone; stop the run and take its result.
                    interrupter.interrupt();
                    return wait.await;
                }
                if stopping {
                    // Said once; later signals are ignored without a word.
                    if !told {
                        told = true;
                        report("delega run: still stopping the worker; waiting for cleanup to finish");
                    }
                } else {
                    stopping = true;
                    interrupter.interrupt();
                    report("delega run: signal received, stopping the worker");
                }
            }
        }
    }
}
