//! Run one child process and always end with one report.
//!
//! This layer starts a program from a neutral [`ProcessSpec`], keeps its
//! output within bounds, stops it on a time limit or on request, and returns a
//! single [`ProcessReport`]: how it ended ([`Ending`]), what it wrote, when,
//! and whether everything it started is known to be gone
//! ([`Cleanup`](crate::contract::result::Cleanup)). It
//! knows nothing about what the program is.
//!
//! ```no_run
//! # async fn demo() {
//! use delega_core::process::{ProcessSpec, run};
//! use std::time::Duration;
//!
//! let spec = ProcessSpec::new("/bin/echo", Duration::from_secs(30)).args(["hello"]);
//! let report = run(spec).await;
//! assert_eq!(report.stdout.text(), "hello\n");
//! # }
//! ```
//!
//! # How a child is owned
//!
//! The child is started in a process group of its own, so its pid is the group
//! id. One owner task holds the child until a single reap at the end and is the
//! only sender of signals. While the child runs, a periodic `ps` scan records
//! the descendants with their start times, so a descendant that left the group
//! is still found and a recycled pid is never signalled. A stop is SIGTERM to
//! the group, SIGKILL after the grace period (repeated on a timer that does
//! not wait for inspection), and individual signals to tracked descendants
//! until the group is empty and every tracked descendant is gone. The
//! leader's exit is observed without reaping it. A descendant that keeps the
//! output pipes open after the kill does not hold the report: the owner closes
//! its ends after the drain bound and says so in the diagnostics.
//!
//! Dropping a [`ProcessHandle`] interrupts the run. If the owner is dropped
//! (the runtime shuts down, or a panic), its `Drop` sends SIGKILL to the group.
//!
//! # What cleanup does not cover
//!
//! `Cleanup::Confirmed` covers the child's process group and the descendants
//! the scan observed. Known gaps, which this layer does not close:
//!
//! - A child that starts a detached grandchild (its own process group) and
//!   exits at once leaves that grandchild running, because it was never
//!   observed, and the report still says `Confirmed`. Tracked as delega issue
//!   1; see the ignored tests in `tests/gaps.rs`.
//! - The same hole opens when a crash lands between two scans: a descendant
//!   that left the group after the last scan and before the crash is unknown.
//!   The pre-stop scan only runs for a stop the owner starts, not for a crash.
//!
//! [`Diagnostics::unobserved_descendants_possible`] is always true for a
//! started child, and [`Diagnostics::capture_gap_observed`] says when the scan
//! is known to have missed its budget or failed.
//!
//! # Platform
//!
//! Unix only; the module is empty elsewhere. The group is made with
//! `process_group(0)` (a new process group in the same session) rather than
//! `setsid`, because `setsid` needs `pre_exec`, which is `unsafe`, and the
//! workspace forbids `unsafe`. A child therefore keeps its controlling
//! terminal, which only matters when delega itself runs on one.

#![cfg(unix)]

mod capture;
mod handle;
pub mod limits;
mod owner;
mod report;
mod run;
mod spec;
mod sys;
mod timestamp;
mod tree;

#[cfg(test)]
mod tests;

pub use capture::Captured;
pub use handle::{Interrupter, ProcessHandle, run};
pub use report::{
    Diagnostics, Ending, LeaderExit, ProcessReport, ProcessTiming, SignalRecord, SignalTarget,
};
pub use run::PIPE_WARNING;
pub use spec::{CaptureSpec, EnvPolicy, Keep, ProcessSpec};
pub use timestamp::rfc3339_utc;
pub use tree::InspectionStats;
