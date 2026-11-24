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
//! A spec with a [`KeeperCommand`] is a kept run: the owner starts the keeper,
//! and the keeper starts the child as its own child, exactly as below, and
//! holds it unreaped until the owner says `reap` (see "The keeper"). Everything
//! in this section holds for both; where a kept run differs, the section says
//! so.
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
//! group. That first sweep is bounded by `DESCENDANT_SIGNAL_BUDGET` (250 ms)
//! so a slow `ps` cannot hold the owner or delay the group signal. When it does
//! not complete in time it signalled nothing, and the owner repeats the sweep
//! beside its reads, without that bound: one sweep at a time (so at most one
//! listing of its own in flight, each bounded by `PS_TIMEOUT`), repeated only
//! after a listing that failed or timed out, after a pause that doubles from
//! `TERM_SWEEP_RETRY_PAUSE` (100 ms) to `TERM_SWEEP_RETRY_MAX_PAUSE` (2 s). The
//! first sweep that returns a listing ends the repeats, so a descendant gets
//! one TERM from the stop however many attempts it took. SIGKILL follows after
//! the grace period (the group KILL repeats on a timer that does not wait for
//! inspection) with the same treatment: a bounded sweep, then an unbounded
//! repeat if it did not complete. Signal records keep their meaning (one
//! `Descendants` entry per completed sweep); `inspection.attempts` counts every
//! listing started, including these repeats.
//! A leader that is only stopped has
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
//! owner closes its ends after the drain bound and says so in the diagnostics,
//! and the verdict is `Unconfirmed` ([`PIPES_HELD_REASON`]) even when every
//! sweep afterwards found nothing, because a process that none of them saw
//! held the output. This holds with or without a keeper.
//! If process listing is unavailable (no `ps`, or repeated failures) cleanup
//! does not wait out its deadline: it kills the group, reaps the leader,
//! checks the group with `kill(-pgid, 0)`, and reports `Unconfirmed` saying
//! that.
//!
//! Dropping a [`ProcessHandle`] interrupts the run. If the owner is dropped (the
//! runtime shuts down) its `Drop` sends SIGKILL to the group and to the
//! descendants seen at the latest scan, and, in a kept run, closes its end of
//! the keeper's socket, so the keeper stops what is left; a panic in the owner
//! loop is followed by a hard stop and a report.
//!
//! # The keeper
//!
//! A kept run has a keeper process between the owner and the child (module
//! `keeper`). The caller names the program that runs [`keeper_main`]; the
//! `delega` binary does so with a hidden `process-keeper` subcommand, and the
//! CLI and the MCP server always supply it. A spec without one is owned
//! directly, as before the keeper existed, and its report has
//! `diagnostics.keeper` `None`; a spec whose keeper cannot be started ends
//! `FailedToStart` with nothing started.
//!
//! - The keeper leads a session of its own. Its standard input is one end of a
//!   Unix socket pair whose other end only the owner holds, close-on-exec; the
//!   child's standard input, output and error are pipes the owner made, passed
//!   to the keeper with `SCM_RIGHTS`, so the owner reads and writes them as it
//!   does for a child of its own. Both ends of every pipe are close-on-exec, made
//!   under the spawn lock.
//! - The keeper starts the child in a session of the child's own, so the
//!   child's pid is its group id and session id as above. It watches the child
//!   with `waitid(WEXITED | WNOWAIT)` and tells the owner how it ended, and it
//!   reaps the child only when the owner says `reap`, after the owner's last
//!   signal. The child's pid therefore keeps naming its group and session while
//!   the owner signals them.
//! - On Linux the keeper calls `prctl(PR_SET_CHILD_SUBREAPER)`, so a descendant
//!   whose parent exits becomes a child of the keeper instead of init. The
//!   keeper reaps such orphans by pid (never with a wait for any child, which
//!   would take the child's status first), and its children are owned by every
//!   listing. It also walks its own tree from `/proc` without `ps`
//!   (`/proc/<pid>/task/<tid>/children`, or the parent field of every
//!   `/proc/<pid>/stat` where that file is missing). Two rules govern it.
//!   Nothing is signalled by pid number: a process is signalled only through a
//!   pidfd opened after it was found and checked to be a live child of the
//!   parent it was found under, with its start time kept so the pid is checked
//!   again before its children are read; a process that cannot be held (no
//!   pidfd, or a parent that changed) is not signalled and nothing below it is
//!   walked. And no zero is trusted without proof: `census` counts the tree as
//!   empty only when two passes in a row agree, a full scan of every
//!   `/proc/*/stat` finds the same set (the children files can omit a live
//!   child while another exits during the read), and no exited, unreaped child
//!   of the keeper other than the program showed the tree in flux (checked
//!   before the walk reaps it). `freeze` stops each process and reads its
//!   children only once the stop has taken effect, and is complete only when a
//!   pass from the keeper's own children found nothing new, held and stopped
//!   everything, refused nothing, saw no flux, and the full scan finds no live
//!   process outside the held set below the keeper or a held process (with
//!   everything stopped nothing can fork or exit, so the scan is a real
//!   second source); then SIGKILL and SIGCONT go through the pidfds and a
//!   complete census must find nothing. Its budget is checked for every
//!   process. An answer that is not complete is never zero, so neither the
//!   owner's verdict nor the keeper's own cleanup ends on an unproven empty
//!   tree. Once any answer of a run was unproven in a way a later one cannot
//!   repair (a refused hold or a failed read, in any answer, or a freeze
//!   task that failed), the verdict stays `Unconfirmed`
//!   ([`KEEPER_UNPROVEN_REASON`]); no later proof supersedes it. A freeze
//!   that only did not complete (its details are exactly a walk's
//!   `complete=false refused=0 read_failures=0`: passes disagreed, the tree
//!   was in flux, or the budget ran out) is recorded apart: it leaves the
//!   verdict `Unconfirmed` for the same reason unless the final proof of
//!   the narrower Rule A below supersedes it. The keeper records either
//!   kind before it sends the answer (the census and freeze it runs at the
//!   reap included) and says so in its `reaped` line (`unproven`,
//!   `incomplete`); the owner's socket reader records it as it reads the
//!   answer, before handing it to the request, so an answer that arrives
//!   after its request timed out or was cancelled still counts. A census
//!   that is incomplete only because the tree keeps changing never claimed
//!   zero and does not count. A process listed in a children file whose stat
//!   reads as missing is gone only if its pidfd says so (`hidepid` can hide a
//!   live one). A queued child is taken under its parent only while the
//!   parent's pidfd shows it alive, and otherwise only as the keeper's own
//!   child; the parent's pidfd is asked again after the child's parent field
//!   was compared with the parent's pid, and a parent that exited by then
//!   drops the candidate unsignalled (the comparison may have matched a
//!   process that reused the parent's pid), so the child is held only as the
//!   keeper's own direct child or refused. The owner reads a start time while
//!   holding the pin's lock; the child's own identity is learned under that
//!   lock too, with the check of the pin, the read and storing it in one
//!   hold, and is not learned at all once the pin is released. What the lock
//!   guarantees is bounded by the release handshake: the keeper reaps the
//!   child only after `released`, which waits for the lock, so a keeper that
//!   lets go through the handshake cannot free the pid mid-read. A keeper
//!   that dies while the owner holds the lock is outside it: the reaper above
//!   it can reap the child before the read, a reused pid can then supply a
//!   replacement's start time before the socket reader clears the pin, and
//!   only the keeper-loss verdict covers that (the residual below; the run
//!   is `Unconfirmed`, [`KEEPER_LOST_REASON`]). A read of `/proc` that fails for any
//!   reason but the process being gone (ENOENT, ESRCH) makes the pass
//!   incomplete, never counts as absence (the stat of a held process
//!   included), and so does any error from a directory listing of `/proc`;
//!   an error while listing the task directory of a pid already listed (a
//!   process reaped mid-listing ends it with ENOENT) counts as that process
//!   gone only on strict proof of exit (opening its pidfd fails with ESRCH,
//!   or a poll shows the pidfd readable, which needs every thread to have
//!   exited; a zombie leader can have live threads), and otherwise as a
//!   failed read; a children read is kept only if the parent's pidfd shows it had not
//!   exited by the end of the read; a refused hold anywhere in a freeze leaves
//!   it unproven whatever later passes find; and the freeze releases its
//!   pidfds before the final census. Each answer carries its passes and
//!   whether it completed, in `diagnostics.keeper.tree_reports`. On Linux an owner's sweep that
//!   finds nothing counts only when the keeper's census agrees, and a keeper
//!   that still finds processes alive when it reaps the child makes the verdict
//!   `Unconfirmed` ([`TREE_LEFT_AT_REAP_REASON`]). A keeper that could not
//!   become a child subreaper starts the child anyway, and every such run is
//!   `Unconfirmed` ([`NO_SUBREAPER_REASON`]): the guarantee is missing, so the
//!   verdict says so rather than refusing runs on such a system.
//! - When the owner's end of the socket closes (the owner exited, crashed or
//!   was killed with SIGKILL), on SIGTERM, SIGINT, SIGHUP or SIGQUIT to the
//!   keeper, when its parent changes, and at a ceiling of the time limit plus
//!   the cleanup deadline plus `KEEPER_CEILING_MARGIN` (10 s), the keeper stops
//!   the tree by itself: on Linux TERM to the child's group and to its tree, the
//!   grace, then freezes until nothing is alive; elsewhere one listing first,
//!   then TERM to the group and to what a sweep finds (group, session,
//!   ancestry, run marker), the grace, then KILL until two sweeps in a row find
//!   nothing. Either way within the cleanup deadline; then it reaps the child
//!   and exits. When the owner may still be there (a signal, the ceiling, a new
//!   parent), the keeper first says `releasing` and waits up to
//!   `KEEPER_RELEASE_WAIT` (2 s) for `released`: the owner sends its group and
//!   member signals while holding a lock that its answer also takes, so none of
//!   them is in flight, and none follows, once the keeper frees the child's
//!   pid. Without `released` (or the owner's end closing with a real end of
//!   file; a failed read of the socket is not one) the keeper does not reap
//!   the child: it exits and leaves it to the reaper above it (init or
//!   another subreaper), which frees the pid when it reaps it. The keeper
//!   installs its signal handlers before it starts the child.
//!   `PR_SET_PDEATHSIG` is not used: it fires when the thread that forked the
//!   child exits, which in a multi-threaded owner is not the owner's
//!   end. End of file on a socket comes when the last descriptor of the owner's
//!   end closes, whichever thread held it.
//! - The run marker: a kept child's environment has `DELEGA_RUN_MARKER`
//!   ([`MARKER_ENV`]), a list of random values separated by `:` (the one in the
//!   owner's own environment, if any, then a new one), so a descendant that left
//!   the child's session and whose parent is gone can still be found. Cleanup
//!   sweeps read it from `/proc/<pid>/environ` on Linux and from one `ps -U
//!   <uid> -E` listing elsewhere, taken beside the ordinary listing. Both read
//!   the process's environment area as the kernel exposes it (see module
//!   `marker`). A marked process that is in the
//!   child's session when it is read (it started after the ordinary listing)
//!   is a member and is signalled as one. A process found only by the marker is
//!   never signalled by number alone: it is stopped with SIGSTOP when first
//!   seen, and receives SIGKILL and SIGCONT only from a KILL sweep whose reading
//!   shows it still stopped with the same start time; a stopped process cannot
//!   exit by itself, so its pid cannot have been reused.
//! - The hard stop of a kept run is a freeze: everything a listing finds outside
//!   the group is stopped with SIGSTOP and the listing is taken again, up to
//!   `FREEZE_MAX_ROUNDS` (6) listings, until one finds nothing that was not
//!   already stopped; then SIGKILL and SIGCONT. On Linux the keeper's own freeze
//!   runs at the same time. A freeze that ran out of listings while processes
//!   kept appearing, and whose keeper did not report its tree empty, makes the
//!   verdict `Unconfirmed` ([`FREEZE_INCOMPLETE_REASON`]).
//! - A keeper that does not say whether it started the child within
//!   `KEEPER_START_TIMEOUT`, or ends before saying so, is not killed at once:
//!   the owner closes its end, so a keeper that did start the child stops it,
//!   and waits the grace plus `KEEPER_ABANDON_WAIT` for the keeper to exit
//!   before it kills it. The run ends `FailedToStart` with cleanup
//!   `Unconfirmed` ([`KEEPER_UNANSWERED_REASON`]); only a keeper that could not
//!   be started, or that said the child could not be, gives `Confirmed`.
//! - If the keeper ends before it reaped the child (it was killed), the child's
//!   pid no longer pins its group or session. The owner sends no group signal
//!   after that, follows the child by its recorded start time like a tracked
//!   descendant, and the verdict is `Unconfirmed`
//!   ([`KEEPER_LOST_REASON`]).
//!
//! What a keeper costs, measured on macOS arm64 (macOS 27.2, about 960
//! processes, load average about 16, release build): starting
//! `delega process-keeper` and letting it exit took a median of 6.3 to 6.4 ms
//! over two rounds of 60 runs (`/usr/bin/true`, measured the same way, 4.6 to
//! 5.0 ms). A whole run of `/usr/bin/true` from start to report took a median of
//! 87 ms kept and 62 ms owned directly, 40 runs each
//! (`measure_the_keepers_cost` in `tests/keeper.rs`); most of the difference is
//! the marker listing of each cleanup sweep. That listing, `ps -U <uid> -Eww -o
//! pid=,stat=,lstart=,command=`, took a median of 30 ms against 20 ms for the
//! ordinary listing in one round of 40 runs at about 940 processes, and both
//! 56 ms in a round under heavier load; it printed about 300 KB. On Linux the
//! marker is read from `/proc` and costs no extra listing; it has not been
//! measured.
//!
//! # What `Confirmed` means
//!
//! `Cleanup::Confirmed` means a final sweep, taken after the leader exited and
//! before it was reaped, found no live process in the child's group, none in
//! its session, and no tracked descendant; in a kept run also no process
//! carrying the run marker and, on Linux, no live process in the keeper's tree
//! by the keeper's census. The sweep runs once for a plain exit where nothing
//! but the leader was ever seen, and twice in a row when any descendant was
//! observed or signalled. It does not cover what "What remains possible" lists.
//!
//! Rule A: cleanup is `Confirmed` only with proof. A signal the cleanup had to
//! send and did not, because the process could not be verified (see "Why the
//! owner never signals a foreign process"), leaves the verdict `Unconfirmed`
//! ([`UNVERIFIED_SIGNAL_REASON`]), and so does any hold the keeper refused or
//! read it failed in an answer, a freeze task that failed, or a freeze that
//! did not complete ([`KEEPER_UNPROVEN_REASON`]), whatever later sweeps find.
//! One exception, the narrower Rule A for Linux kept runs: the
//! keeper is a child subreaper, so every live descendant is in its tree. At
//! the reap the keeper takes a final census after the child has exited, and
//! says in its `reaped` line (`proven`) whether that census proves the tree
//! empty: complete (two passes agree and the scan of every `/proc/*/stat`
//! confirms), nothing alive, no hold refused within that census, and no read
//! of `/proc` failed in the keeper at any point of the run, decided after
//! its last walk. The proof is about the tree at the reap and nothing else:
//! it is not the verdict, and it says nothing about holds refused earlier in
//! the run. The verdict lets it supersede two things, both about processes
//! that while alive are in the keeper's tree: the owner's refusals of group
//! and session members and tracked processes, and the keeper's freezes that
//! only did not complete (an incomplete freeze says the keeper had not
//! stopped everything yet, not that a process escaped). Everything else is
//! sticky and keeps cleanup `Unconfirmed` independently of the proof: a
//! refusal meant for a process found only by the run marker, a hold the
//! keeper refused and a read it failed (also when reported inside a freeze
//! answer), a freeze task that failed or a keeper line outside the protocol
//! (the keeper can say `proven` and `unproven` in the same `reaped` line,
//! and such a run is `Unconfirmed`), a lost keeper ([`KEEPER_LOST_REASON`])
//! and an unacknowledged reap ([`NO_REAP_ACK_REASON`]). The proof is also
//! required: a Linux kept run
//! whose keeper is a subreaper, acknowledged the reap and left nothing alive,
//! but did not prove the tree empty, is `Unconfirmed`
//! ([`FINAL_CENSUS_UNPROVEN_REASON`]) even when nothing was refused. macOS
//! has no subreaper, and its keeper never reports a proof; its verdict does
//! not wait for one, and nothing there is superseded.
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
//! # What a macOS verdict covers
//!
//! This describes cleanup after a kept run started a child; a run that started
//! none has nothing to sweep. On macOS the keeper is not a subreaper, so it
//! cannot adopt orphans. Cleanup finds descendants through the child's process
//! group, its session, earlier listings and the run marker. An output pipe
//! still held when the drain bound runs out makes the verdict `Unconfirmed`.
//! A descendant that none of these reach (for example one that left the
//! session and does not show the marker, or a fast fork relay) can be missed,
//! and the run can then end `Confirmed` while it runs. The evidence and
//! measurements are under "What remains possible". A kept run on macOS carries
//! [`MACOS_KEEPER_NOTE`](crate::provider::MACOS_KEEPER_NOTE) in its
//! `provider_notes`.
//!
//! # What remains possible
//!
//! Without a keeper, two gaps remain, and both end with `Confirmed` while a
//! descendant runs:
//!
//! - A descendant that calls `setsid` and whose parent exited before any
//!   listing saw the two together. The parent is the child itself (it exits, or
//!   crashes, between two listings with the descendant calling `setsid` after
//!   the earlier one) or a member of the session that exits at once. A process
//!   whose parent exits is a child of init from that moment, in a session of
//!   its own, so no later listing can tie it to the child, and nothing in the
//!   process table says who its parent was. Listings are taken at spawn, then
//!   at 50, 100 and 200 ms after it (`CAPTURE_RAMP`), then every 500 ms
//!   (`CAPTURE_INTERVAL`), and once more, within a budget, before every stop and
//!   before the signals that follow a natural exit; the last cannot run earlier
//!   than the exit itself. The window is therefore the time between two
//!   neighbouring listings: 50 to 100 ms in the first 200 ms of a run and 500
//!   ms after it (the first periodic listing is 500 ms after the ramp ends, at
//!   700 ms). Under load, a listing that is late widens it. Each extra listing
//!   costs one run of `ps`. Measured on macOS arm64 with `ps -axo
//!   pid=,ppid=,pgid=,stat=,lstart=` run 60 times in a row from a script, 822
//!   processes, load about 10: median 12.3 ms, p90 13.0 ms (an earlier set of
//!   50 runs at 785 processes gave a median of 11.4 ms). The ramp applies to
//!   every process-layer run, the provider version probe and Codex's MCP probe
//!   included, and each run that lasts past 50, 100 or 200 ms gets the
//!   listings of the steps it outlives, so one `delega` run can take more than
//!   three extra `ps` runs.
//!   [`Diagnostics::capture_gap_observed`] says when a scan is known to have
//!   missed its budget or failed, which widens that window, and when no
//!   trusted identity of the child itself was learned (no listing showed it
//!   while it was pinned, as when the keeper let go before the first one). A descendant whose
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
//! And if the owner is killed with SIGKILL, nothing stops the child (see "What
//! no in-process code can do").
//!
//! A kept run closes these on Linux. On macOS it closes the SIGKILL of the
//! owner, and the two gaps remain in part, as "What a macOS verdict covers"
//! states; tracking forks on macOS is shelved (delega issues 167 and 189). In
//! detail:
//!
//! - On Linux the keeper is the reaper of every orphan, so the `setsid`
//!   descendant and every generation of the relay are the keeper's descendants
//!   by parent links; the keeper's census keeps the verdict from being
//!   `Confirmed` while one lives, and its freeze stops the relay, which cannot
//!   fork once stopped. The tests `a_setsid_grandchild_whose_parent_exits_at_once_is_stopped_in_a_kept_run`,
//!   `an_orphan_that_clears_its_environment_is_stopped_by_the_keeper_on_linux` and
//!   `a_fork_relay_is_stopped_in_a_kept_run` in `tests/gaps.rs` run in the Linux
//!   CI job; they have not run on a developer machine.
//! - On macOS there is no subreaper. A descendant that left the session, whose
//!   parent is gone, and that does not show the marker can be missed. Among
//!   these are a process started with an environment that leaves the marker
//!   out, and, measured on macOS 27.2 with System Integrity Protection enabled,
//!   the Apple programs measured: `ps -E` showed no
//!   environment for `/bin/sleep`, `/bin/sh` or `/usr/bin/perl`, and showed it
//!   for a Homebrew binary and for this crate's test binary. Whether this applies
//!   depends on the machine: a macOS system that does not hide the environment
//!   of its own programs shows the marker of `sleep` too, and a run on GitHub's
//!   macos-latest runner found such an orphan by its marker. The tests probe
//!   it (start `/bin/sleep` with a marker value and look for it in `ps -E`) and
//!   expect accordingly, and so does the acceptance harness, which records the
//!   answer in its results. Where it is hidden, such a descendant
//!   can end with `Confirmed` while it runs, as without a keeper.
//! - On macOS a fast fork relay can escape the freeze. In 50 interrupted kept runs of
//!   the relay in `tests/gaps.rs` (perl, a new generation about every 5 ms, 4000
//!   generations), measured on macOS 27.2 arm64 at a load average of about 15
//!   (`measure_the_relay_freeze`), the freeze stopped it in none: each listing
//!   takes 20 ms or more, so the generation it shows has exited before its
//!   SIGSTOP, and the relay runs perl, whose environment `ps -E` does not show.
//!   That relay holds the output pipes, and with the held-pipe verdict all 50
//!   runs of a second measurement (load average about 20) ended `Unconfirmed`
//!   and none `Confirmed`; before it, all 50 had ended `Confirmed` with a relay
//!   process alive. A relay that does not hold the output can end
//!   `Confirmed` while it runs.
//! - On Linux, two windows remain. The keeper is started with a synchronous
//!   spawn, so an `exec` of the keeper that stalls (on a hung file system, for
//!   example) holds the owner's start without a bound. And a keeper that stops
//!   the tree on its own (a signal, its ceiling, a new parent) and gets no
//!   `released` from the owner within 2 s, or whose read of the socket fails,
//!   exits without reaping the child (and a `reap` it cannot complete is not
//!   acknowledged, which makes the verdict `Unconfirmed`):
//!   from then until the reaper above it reaps the child, an owner that wakes
//!   (it was stopped) and has not seen the keeper's end close could still send
//!   a group signal by the child's pid; once the child is reaped that pid can
//!   name another group.
//! - In one line each, the residuals the keeper accepts:
//!   - a stalled `exec` of the keeper at spawn holds the owner's start without
//!     a bound;
//!   - a keeper that is lost, or that exits without the owner's `released`,
//!     leaves the child unreaped to the reaper above it, and the verdict is
//!     `Unconfirmed`; a start time the owner read under the pin's lock
//!     around that moment can be a replacement's, since the lock holds only
//!     against the release handshake;
//!   - on macOS, the two kinds of process "What a macOS verdict covers" names
//!     can be missed;
//!   - a process the kernel prevents inspecting or signalling (`hidepid`,
//!     another user's, no pidfd) leaves the verdict `Unconfirmed`, and the
//!     keeper's own cleanup keeps going until its deadline and then exits.
//! - SIGKILL aimed at the keeper itself, on every platform. The owner notices
//!   (the keeper's end of the socket closes before `reaped`) and reports
//!   `Unconfirmed`; if the owner is killed with the keeper, nothing stops the
//!   child.
//! - The owner and the keeper both stopped for good (SIGSTOP) until after the
//!   keeper's ceiling, a power loss, or the machine's own limits.
//!
//! A descendant that stays in the session, even in a group of its own, is not
//! part of any of these. Neither is a descendant that left the session while a
//! scan or the listing at the exit saw its parent as a member or as a tracked
//! process.
//!
//! # Pipes when several children start at once
//!
//! On macOS std creates a child's pipes with `pipe()` and marks them
//! close-on-exec only afterwards; Linux uses `pipe2` with `O_CLOEXEC`. A child
//! forked by another thread in between inherits those pipe ends and keeps them
//! through `exec`. A provider that holds another run's standard input keeps
//! that run from reading end of input until the holder exits, which a provider
//! that reads its prompt to end of file never does on its own (#104). Every
//! child of this module, the `ps` listing included, is therefore spawned while
//! holding one process-wide lock (`spawn.rs`). The delega CLI's plugin
//! commands and the workspace's tests and acceptance harness take it too,
//! through [`spawn_guard`], and clippy's `disallowed_methods` (the workspace
//! `clippy.toml`) rejects a direct `Command` spawn, `output` or `status` call
//! anywhere in the workspace. The lock is held only across the spawn call. A program that
//! embeds this crate and spawns on its own threads without [`spawn_guard`] can
//! still leak descriptors into these children and receive theirs.
//!
//! # Why the owner never signals a foreign process
//!
//! A process is signalled only if it is a live member of the child's group (the
//! group id is the child's pid, which the unreaped child pins: unreaped by the
//! owner, or in a kept run by the keeper until the owner says `reap`), a live
//! member of its session (read as above and read again just before the
//! signal), a tracked process whose pid and start time match what was recorded,
//! or a process carrying the run marker, which is stopped first and killed only
//! when a later reading shows it still stopped with the same start time (see
//! "The keeper"). A process becomes tracked only by being a member, or by having
//! a parent that is owned at that listing: the child itself, a member, a tracked
//! process with a matching start time, or, in a kept run, the keeper. The keeper
//! is the owner's own unreaped child, so its pid cannot name another process,
//! and it starts nothing but the child, so its children are the child and, on
//! Linux, the orphans of the child's tree. Init, the process that runs the owner
//! and the keeper are never owned, so a process reparented to init is not
//! adopted through its parent, and it is signalled only if it was tracked
//! earlier and still has the same start time. When the keeper of a kept run is
//! gone before the reap, the child's pid pins nothing: no group signal is sent
//! from then on, and nothing is a member by its group or session id. A process
//! that carries the run marker without being a descendant (a process a worker
//! started through a service, or one that copied another run's value from `ps
//! -E`) is treated as the run's; only a process that has the value can be
//! one. No process can join another session or group, so a member is a
//! descendant of the child by construction.
//!
//! On Linux each such signal is delivered through a pidfd, opened first and
//! checked against `/proc/<pid>/stat` before the signal: a group member must
//! still have the child's group id, a session member its session id, a tracked
//! process the start time in clock ticks recorded when it was first tracked,
//! and a marked process its recorded start time (and, for SIGKILL, be
//! stopped). A pid reused between the listing and the signal is therefore
//! never signalled; this also covers the SIGKILL that `Drop` sends to the
//! tracked processes of the latest scan. Nothing is ever signalled by pid
//! number on Linux: a process without a pidfd (a kernel before 5.3, or a
//! refusal), or a tracked or marked process with no recorded start time (one
//! is recorded only when `/proc` shows the process now holding the pid has
//! the keeper, the pinned child, or the verified listed parent as its parent;
//! a marked process's start time must read the same before and after its
//! environment), is not signalled, and the verdict is `Unconfirmed`
//! ([`UNVERIFIED_SIGNAL_REASON`]) unless the narrower Rule A of "What
//! `Confirmed` means" supersedes it. Such a process counts as refused only
//! while its pid may still name a live process: a pidfd opened for the pid
//! that fails with ESRCH, or that shows its process exited, means the process
//! found has exited (its pid could be reused only after it was reaped), so
//! there is nothing to signal; nothing is signalled to find this out. A
//! keeper that does not acknowledge the reap makes the verdict `Unconfirmed`
//! too ([`NO_REAP_ACK_REASON`]).
//!
//! What remains is a pid recycled between the two rows of one listing (the
//! child's `ppid` names the pid of a process that died and whose number a new
//! member took within the run of one `ps`), and elsewhere (no
//! pidfds, no `/proc`) a pid reused between the listing and the signal: a
//! session member is read once more just before its signal, which narrows it
//! to the time between those two calls, but a tracked or group member is
//! signalled by the identity its listing showed, and two processes can share a
//! pid and a start time within the resolution of `ps` (one second on macOS).
//! Each needs a pid to wrap within a few milliseconds to one run of `ps`.
//!
//! # What no in-process code can do
//!
//! If the process that owns the child is killed with SIGKILL, or is stopped
//! for good, no code of the owner runs: its `Drop`, the time limit and the
//! cleanup all go with it. The same holds for a crash that skips destructors
//! and `exit` called from another thread. Without a keeper the child, which
//! leads a session of its own and so does not receive the signals its owner's
//! terminal sends, keeps running with nothing left to enforce its time limit.
//! The keeper is the supervisor outside the owning process that covers this: it
//! stops the tree when the owner's end of its socket closes, or at its ceiling
//! when the owner is stopped for good. A graceful signal (SIGINT, SIGTERM,
//! SIGHUP, SIGQUIT) is the caller's to turn into an interrupt, as `delega run`
//! does.
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
//! is compiled but has not been checked. It has the only `unsafe` block in
//! production code (the tests of `delega-mcp` have one more, for `mkfifo`): a
//! `pre_exec` hook in `sys.rs` that calls only `setsid`, used for the child and
//! for the keeper.
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
//! - The keeper's subreaper, its `/proc` walk and freeze, the reaping of
//!   orphans, and the marker read from `/proc/<pid>/environ` run only on Linux;
//!   their tests run in the Linux CI job and compile and pass clippy locally
//!   for `x86_64-unknown-linux-gnu`.

#![cfg(unix)]

mod capture;
pub(crate) mod dialogue;
mod handle;
mod keeper;
pub mod limits;
mod marker;
mod owner;
mod report;
mod run;
mod spawn;
mod spec;
mod sys;
mod tap;
mod timestamp;
mod tree;

#[cfg(test)]
mod tests;

pub use capture::Captured;
pub use handle::{Interrupter, ProcessHandle, run};
pub use keeper::{KeeperCommand, keeper_main};
pub use marker::MARKER_ENV;
pub use owner::{
    FINAL_CENSUS_UNPROVEN_REASON, FREEZE_INCOMPLETE_REASON, KEEPER_LOST_REASON,
    KEEPER_UNPROVEN_REASON, LATE_LISTING_REASON, NO_REAP_ACK_REASON, NO_SUBREAPER_REASON,
    PIPES_HELD_REASON, TREE_LEFT_AT_REAP_REASON, UNVERIFIED_SIGNAL_REASON, is_late_listing_only,
};
pub use report::{
    Diagnostics, Ending, KeeperDiagnostics, LeaderExit, ProcessReport, ProcessTiming, SignalRecord,
    SignalTarget,
};
pub use run::{KEEPER_UNANSWERED_REASON, PIPE_WARNING};
pub use spawn::{SpawnGuard, spawn_guard};
pub use spec::{CaptureSpec, EnvPolicy, Keep, OutputTap, ProcessSpec};
pub use timestamp::rfc3339_utc;
#[cfg(test)]
pub(crate) use tree::not_started;
pub use tree::{InspectionStats, check_inspection};
