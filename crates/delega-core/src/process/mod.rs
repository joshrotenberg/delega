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
//! signalled. A scan follows ancestry from the child and from every live
//! member of the child's group or session, not from the child alone: a member
//! stays alive after the child exits, and the children of the child are
//! reparented at that moment, so the members are the only parents left to
//! follow. When the child exits by itself (normally, with an error or by a
//! crash) the owner takes one such listing before it sends the first signal,
//! because the TERM that follows can end a member and orphan what it started. A
//! stop is SIGTERM to the group, then SIGCONT (so a stopped
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
//! for each row of the table, because `ps` has no session keyword that works on
//! both macOS and Linux: from `/proc/<pid>/stat` on Linux, with `getsid` on
//! macOS), and every tracked descendant with a
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
//!
//! It also requires that the listing taken before the first signal completed
//! (see "How a child is owned"). That listing is the only one that can see a
//! member alive together with a descendant that left the session, because the
//! first signal can end the member and orphan the descendant. When it did not
//! complete (the scan timed out or failed, or was not done within its budget),
//! the verdict is `Unconfirmed` even if every later sweep found nothing, with a
//! reason that says the listing did not complete. A later listing does not make
//! up for it, since it is taken after the signals. A periodic scan that failed
//! earlier does not matter once a later one succeeded.
//!
//! Three budgets bound the wait for that listing, and trade a prompt stop
//! against a verdict that can be trusted:
//!
//! - After a natural exit, `EXIT_LISTING_BUDGET` (two listing timeouts, 3.1 s).
//!   The leader is gone, so nothing is delayed for anyone; the normal cost is
//!   one listing.
//! - Before a requested stop (an interrupt or the time limit),
//!   `STOP_LISTING_BUDGET` (500 ms). A caller is waiting for the stop, so when
//!   the listing is not done by then the stop is sent anyway and the verdict is
//!   `Unconfirmed`. A second interrupt while the owner waits ends the wait at
//!   once.
//! - Dropping a [`ProcessHandle`] or the future of [`run`] does not wait at all:
//!   the child is stopped immediately, as before the listing existed.
//!
//! A stop on a machine where `ps` takes longer than 500 ms is therefore prompt
//! and honestly unconfirmed, not slow and confirmed.
//!
//! Anything else is `Unconfirmed`, naming the pids and groups and saying why.
//! Cleanup runs to `cleanup_deadline` only for processes that are seen and do
//! not die.
//!
//! A process listing that is empty, that does not contain this process, or
//! that has a line that cannot be parsed is a failed inspection, never "nothing
//! found", so it cannot confirm cleanup.
//!
//! The environment variable `DELEGA_TEST_NO_PROCESS_LISTING` is a test-only
//! seam for `acceptance/`: when it is set, whatever its value, no program is
//! used to list processes, so inspection is unavailable and cleanup is
//! reported as not confirmed. It cannot substitute a program, so it can only
//! make the verdict weaker.
//!
//! # What remains possible
//!
//! Two gaps remain. Both end with `Confirmed` while a descendant runs, and both
//! have an ignored test in `tests/gaps.rs` that states the wanted behaviour
//! (the relay, and a descendant whose parent was the child itself):
//!
//! - A descendant that calls `setsid` and whose parent exited before any
//!   listing saw the two together. The parent is the child itself (it exits, or
//!   crashes, within one scan interval of the descendant calling `setsid`) or a
//!   member of the session that exits at once. A process whose parent exits is
//!   a child of init from that moment, in a session of its own, so no later
//!   listing can tie it to the child, and nothing in the process table says who
//!   its parent was. The scan runs every 500 ms and once more, within a budget,
//!   before every stop and before the signals that follow a natural exit; it
//!   cannot run earlier than the exit itself.
//!   [`Diagnostics::capture_gap_observed`] says when a scan is known to have
//!   missed its budget or failed, which widens that window. A descendant whose
//!   parent is still alive in the session when the child exits is not part of
//!   this gap: the listing before the first signal adopts it.
//! - A fast fork relay: a descendant that ignores TERM, changes process group
//!   each generation, and forks a successor and exits within a few
//!   milliseconds. Every listing holds a row that is gone when its session is
//!   read, and the successor is not in the listing. Such rows, and rows whose
//!   session cannot be read (for example another user's process under a `/proc`
//!   mounted with `hidepid=1`), are counted in
//!   `diagnostics.inspection.vanished_rows` and are not treated as members: a
//!   process that short-lived is far more often unrelated, and treating each as
//!   possibly ours kept honest runs unconfirmed on a machine with churn.
//!
//! A descendant that stays in the session, even in a group of its own, is not
//! part of either gap. Neither is a descendant that left the session while a
//! scan or the listing at the exit saw its parent as a member or as a tracked
//! process.
//!
//! What would close the first gap is something a process table cannot supply: a kernel
//! facility that reparents orphans to the owner (a child subreaper on Linux,
//! which would make the host process the reaper of every orphan and is not done
//! here) or reports each fork as it happens (`kqueue` process events on macOS).
//! Marking descendants through their environment would follow them past
//! `setsid` but not past a program that clears its environment. Reporting
//! `Unconfirmed` whenever a process of the right age is a child of init was
//! rejected: on a machine that starts daemons it would make ordinary runs
//! unconfirmed.
//!
//! # Why the owner never signals a foreign process
//!
//! A process is signalled only if it is a live member of the child's group (the
//! group id is the child's pid, which the unreaped child pins), a live member of
//! its session (read as above and read again just before the signal), or a
//! tracked process whose pid and start time match what was recorded. A process
//! becomes tracked only by being a member, or by having a parent that is owned
//! at that listing: the child itself, a member, or a tracked process with a
//! matching start time. Init and the process that runs the owner are never
//! owned, so a process reparented to init is not adopted through its parent, and
//! it is signalled only if it was tracked earlier and still has the same start
//! time. No process can join another session or group, so a member is a
//! descendant of the child by construction. What remains is a pid recycled
//! between the two rows of one listing (the child's `ppid` names the pid of a
//! process that died and whose number a new member took within the run of one
//! `ps`), a pid reused between the listing and the per-row session read (a foreign
//! row would then read as a member and be signalled, after the session member
//! is read once more just before the signal, which narrows it to the time
//! between those two calls), and two processes with one pid and one start time
//! within the resolution of `ps` (one second on macOS). Each needs a pid to wrap
//! within a few milliseconds to one run of `ps`.
//!
//! # What no in-process code can do
//!
//! If the process that owns the child is killed with SIGKILL, or is stopped
//! for good, no code of this crate runs: the owner's `Drop`, the time limit and
//! the cleanup all go with it. The child, which leads a session of its own and
//! so does not receive the signals its owner's terminal sends, keeps running
//! with nothing left to enforce its time limit. The same holds for a crash that
//! skips destructors, a power loss, and `exit` called from another thread. A
//! caller that must survive this needs a supervisor outside the owning process;
//! this crate has none. A graceful signal (SIGINT, SIGTERM, SIGHUP, SIGQUIT) is
//! the caller's to turn into an interrupt, as `delega run` does.
//!
//! # Platform
//!
//! Unix only; the module is empty elsewhere. The session of a process is read
//! from the sixth field of `/proc/<pid>/stat` on Linux, because `getsid(2)`
//! answers 0 there for kernel threads and for processes whose session leader is
//! outside the pid namespace, and rustix cannot represent that answer (it
//! panics in a debug build and is undefined behaviour in a release build). A
//! missing proc entry is a process that is gone; a session of 0 is read as an
//! ordinary answer that is never the child's. On macOS `getsid` is called
//! through rustix for pids above 0; `ps -ax` lists no pid 0 row there and no
//! listed process answered 0 when checked. Nothing else guards that call: if
//! `getsid` answered 0 there, rustix would hit the same undefined behaviour in a
//! release build. macOS and Linux are the supported systems; on other Unix
//! systems (FreeBSD, where kernel processes can share session 0) the same path
//! is compiled but has not been checked. It has the workspace's one
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
//! - The `/proc/<pid>/stat` session read for processes in other sessions, for
//!   other users' processes and for kernel threads, now read for every row of
//!   every listing (the 0 answer is covered by unit tests).
//! - A zombie leader keeps its session: the session of the members it left behind
//!   answers its pid, and its children are reparented when it exits, not when it
//!   is reaped. Both hold on macOS; the adoption of a `setsid` descendant whose
//!   parent is a member depends on them.
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
mod tap;
mod timestamp;
mod tree;

#[cfg(test)]
mod tests;

pub use capture::Captured;
pub use handle::{Interrupter, ProcessHandle, run};
pub use owner::{LATE_LISTING_REASON, is_late_listing_only};
pub use report::{
    Diagnostics, Ending, LeaderExit, ProcessReport, ProcessTiming, SignalRecord, SignalTarget,
};
pub use run::PIPE_WARNING;
pub use spec::{CaptureSpec, EnvPolicy, Keep, OutputTap, ProcessSpec};
pub use timestamp::rfc3339_utc;
pub use tree::InspectionStats;
