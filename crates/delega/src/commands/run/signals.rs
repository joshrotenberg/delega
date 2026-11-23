//! Stopping a run when `delega` itself is signalled.
//!
//! The provider process leads a session of its own, so a Ctrl-C at the terminal
//! or a hangup when the terminal closes reaches only `delega`, never the
//! worker. `delega` therefore turns SIGINT, SIGTERM, SIGHUP and SIGQUIT into a
//! stop of the worker, all the same way.
//!
//! The first signal interrupts the run through its handle, before anything
//! else happens: the path from the signal to the interrupt writes nothing and
//! takes no lock on standard output or standard error. The run then stops the
//! worker and its descendants, which the process layer bounds in time (its
//! cleanup deadline), and the result carries the cleanup verdict. Any further
//! signal while that goes on is acknowledged once, through the log, and
//! otherwise ignored, so an impatient second Ctrl-C cannot end `delega` while
//! processes are still being stopped. Nothing waits without a bound: the
//! result arrives when cleanup finishes or its deadline passes.
//!
//! SIGKILL cannot be handled by any process. If `delega` is killed that way no
//! result is written, and the worker's keeper (`delega process-keeper`) stops
//! the worker's tree by itself when `delega`'s end of its socket closes.

use std::io;

use delega_core::contract::result::RunResult;
use delega_core::provider::RunHandle;
use tokio::signal::unix::{Signal, SignalKind, signal};

use super::log::Log;

/// Said once when the first signal starts the stop.
const STOPPING: &str = "delega run: signal received, stopping the worker";
/// Said once when a signal arrives during the stop.
const STILL_STOPPING: &str = "delega run: still stopping the worker; waiting for cleanup to finish";

/// Handlers for the signals that stop a run.
pub struct Signals {
    interrupt: Signal,
    terminate: Signal,
    hangup: Signal,
    quit: Signal,
}

impl Signals {
    /// Install the handlers. Done before the worker starts, so no signal can
    /// arrive while a worker runs and `delega` has no handler for it.
    pub fn install() -> io::Result<Self> {
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
            hangup: signal(SignalKind::hangup())?,
            quit: signal(SignalKind::quit())?,
        })
    }

    /// Wait for any of them. `None` means a handler stopped delivering.
    pub async fn next(&mut self) -> Option<()> {
        tokio::select! {
            got = self.interrupt.recv() => got,
            got = self.terminate.recv() => got,
            got = self.hangup.recv() => got,
            got = self.quit.recv() => got,
        }
    }
}

/// Wait for the run's result, stopping the run on the first signal.
/// Returns the result and whether a signal was received while waiting.
pub async fn follow(handle: RunHandle, signals: &mut Signals, log: &Log) -> (RunResult, bool) {
    let interrupter = handle.interrupter();
    let wait = handle.wait();
    tokio::pin!(wait);
    let mut stopping = false;
    let mut told = false;
    loop {
        tokio::select! {
            result = &mut wait => return (result, stopping),
            got = signals.next() => {
                if got.is_none() {
                    // The handlers are gone; stop the run and take its result.
                    interrupter.interrupt();
                    return (wait.await, true);
                }
                if stopping {
                    // Said once; later signals are ignored without a word.
                    if !told {
                        told = true;
                        log.line(STILL_STOPPING);
                    }
                } else {
                    stopping = true;
                    // Interrupt first; saying so can wait.
                    interrupter.interrupt();
                    log.line(STOPPING);
                }
            }
        }
    }
}

/// What a signal that arrives while the result is being written ends with: the
/// outcome's status, or 130 when that is 0, because the result was not
/// delivered.
pub const fn exit_status_when_cut_short(outcome_status: u8) -> u8 {
    if outcome_status == 0 {
        130
    } else {
        outcome_status
    }
}
