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
//! The child is started as the leader of a new session (`setsid`), so its pid
//! is also its process group id and its session id, and it has no controlling
//! terminal. One owner task holds the child until a single reap at the end and
//! is the only sender of signals. While the child runs, a periodic `ps` scan
//! records its descendants with their start times, so a descendant that left
//! the group or the session is still found and a recycled pid is never
//! signalled. A stop is SIGTERM to the group, then SIGCONT (so a stopped
//! process receives it), then the same to everything a sweep finds outside the
//! group; SIGKILL follows after the grace period, and the group KILL repeats on
//! a timer that does not wait for inspection. A leader that is only stopped has
//! not exited and is stopped like any other. The leader's exit is observed
//! without reaping it.
//!
//! Cleanup is a sweep, repeated until one finds nothing (two in a row when any
//! descendant was observed or signalled during the run, so a sweep that follows
//! a kill must be confirmed by the next): every live process in
//! the child's group, every live process whose session id is the child's (read
//! with `getsid` for each row of the table, because `ps` has no session keyword
//! that works on both macOS and Linux), and every tracked descendant with a
//! matching start time. The sweep signals what it finds. A descendant that
//! keeps the output pipes open after the kill does not hold the report: the
//! owner closes its ends after the drain bound and says so in the diagnostics.
//! If process listing is unavailable (no `ps`, or repeated failures) cleanup
//! does not wait out its deadline: it kills the group, reaps the leader,
//! checks the group with `kill(-pgid, 0)`, and reports `Unconfirmed` saying
//! that.
//!
//! Dropping a [`ProcessHandle`] interrupts the run. If the owner is dropped (the
//! runtime shuts down) its `Drop` sends SIGKILL to the group and to the
//! descendants seen at the latest scan; a panic in the owner loop is followed by
//! a hard stop and a report.
//!
//! # What `Confirmed` means
//!
//! `Cleanup::Confirmed` means a final sweep, taken after the leader exited and
//! before it was reaped, found no live process in the child's group, none in
//! its session, and no tracked descendant. The sweep runs once for a plain exit
//! where nothing but the leader was ever seen, and twice in a row when any
//! descendant was observed or signalled. It does not cover the gaps below.
//! Anything else is `Unconfirmed`, naming the pids and groups and saying why.
//! Cleanup runs to `cleanup_deadline` only for processes that are seen and do
//! not die.
//!
//! # What remains possible
//!
//! Two gaps remain, tracked as delega issue 1. Both end with `Confirmed` while
//! a descendant runs, and both have an ignored test in `tests/gaps.rs` that
//! states the wanted behaviour:
//!
//! - A descendant that calls `setsid` and is orphaned before any scan saw it
//!   while its parent lived (a detached grandchild whose parent exits at once,
//!   or a crash before the next scan). It is in no group or session of the
//!   child and has no known ancestry. The scan runs every 500 ms and once more,
//!   within a budget, before a stop the owner starts; it does not run for a
//!   crash. [`Diagnostics::capture_gap_observed`] says when the scan is known
//!   to have missed its budget or failed, which widens that window.
//! - A fast fork relay: a descendant that ignores TERM, changes process group
//!   each generation, and forks a successor and exits within a few
//!   milliseconds. Every listing holds a row that is gone when its session is
//!   read, and the successor is not in the listing. Such rows are counted in
//!   `diagnostics.inspection.vanished_rows` and are not treated as members: a
//!   process that short-lived is far more often unrelated, and treating each as
//!   possibly ours kept honest runs unconfirmed on a machine with churn.
//!
//! A descendant that stays in the session, even in a group of its own, is not
//! part of either gap.
//!
//! # Platform
//!
//! Unix only; the module is empty elsewhere. It has the workspace's one
//! `unsafe`: a `pre_exec` hook in `sys.rs` that calls only `setsid`.
//!
//! Verified on macOS only. Not yet verified on Linux, for the Linux run to
//! check:
//!
//! - `ps -o lstart` of one process can differ by a second between calls on
//!   older procps, which would break pid identity matching.
//! - `kill(-pgid, 0)` and group signals when only a zombie leader remains
//!   (macOS answers EPERM, Linux succeeds), and the EPERM path in cleanup.
//! - `waitid` with `WNOWAIT` for a stopped leader: macOS reports a stop as a
//!   result; the code accepts only exit, kill and core dump, which is the
//!   Linux behaviour too, but the stopped-leader tests have not run there.
//! - `getsid` of processes in other sessions and of other users' processes.
//! - SIGCONT after TERM: on Linux a stopped process holds a TERM pending until
//!   it is continued, so `a_stopped_descendant_receives_terminate_when_the_group_does`
//!   can fail there if the SIGCONT is removed; on macOS it cannot fail.
//! - Fixtures use `perl` to start descendants in their own group or session and
//!   skip with a printed message when it is missing.

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
