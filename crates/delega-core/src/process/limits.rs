//! Every numeric tunable of the process layer.
//!
//! The defaults for a [`ProcessSpec`](super::ProcessSpec) and the fixed
//! intervals of the owner loop are named here so a change is one line and the
//! tests and documentation can refer to them.

use std::time::Duration;

/// Grace between the polite stop (SIGTERM) and the kill (SIGKILL) when a spec
/// does not set one.
pub const DEFAULT_STOP_GRACE: Duration = Duration::from_millis(1500);
/// How long output pipes may stay open after the kill before the owner closes
/// its ends and says so.
pub const DEFAULT_DRAIN_BOUND: Duration = Duration::from_millis(2000);
/// How long, from the first stop signal, the owner keeps signalling before it
/// gives up and reports cleanup as unconfirmed, so a result always arrives.
pub const DEFAULT_CLEANUP_DEADLINE: Duration = Duration::from_millis(10_000);

/// Bytes kept from standard output when a spec does not set a bound.
pub const DEFAULT_STDOUT_CAPTURE_BYTES: usize = 16 * 1024 * 1024;
/// Bytes kept from standard error when a spec does not set a bound.
pub const DEFAULT_STDERR_CAPTURE_BYTES: usize = 256 * 1024;

/// Offsets from the spawn of the child at which the owner takes extra
/// descendant listings, after the one at spawn and before it settles at
/// `CAPTURE_INTERVAL`. A child that starts a descendant in a session of its
/// own and ends within the first scan interval is otherwise seen by no
/// listing together with that descendant. A listing that is late is taken as
/// soon as the previous one is done, not skipped.
pub const CAPTURE_RAMP: [Duration; 3] = [
    Duration::from_millis(50),
    Duration::from_millis(100),
    Duration::from_millis(200),
];
/// Interval between descendant scans while the child runs, once the ramp
/// (`CAPTURE_RAMP`) is over.
pub const CAPTURE_INTERVAL: Duration = Duration::from_millis(500);
/// How long the first signal after a natural exit of the child waits for one
/// descendant listing taken after the exit. The leader is gone, so nothing is
/// delayed for anyone. The listing is the only one that can see a member of the
/// session alive together with a descendant that left it, so a signal sent
/// without it can orphan that descendant, and the verdict then cannot be
/// `Confirmed`. The scan in progress at the exit began before it and cannot
/// stand in, so up to two runs of `ps`, each bounded by `PS_TIMEOUT`, may follow
/// one another. One run took 18 to 54 ms when measured on macOS and several
/// hundred ms with fifty runs at once.
pub const EXIT_LISTING_BUDGET: Duration =
    Duration::from_millis(2 * PS_TIMEOUT.as_millis() as u64 + 100);
/// How long a requested stop (an interrupt or the time limit) waits for that
/// listing before it signals anyway. A caller waits for the stop, so the wait
/// is short; a listing that is not done by then is a missed one and the verdict
/// is `Unconfirmed`. A second interrupt ends the wait at once, and dropping the
/// handle does not wait at all.
pub const STOP_LISTING_BUDGET: Duration = Duration::from_millis(500);
/// Poll interval while a stop waits for that scan.
pub const PRE_STOP_POLL_INTERVAL: Duration = Duration::from_millis(1);

/// Interval between repeated SIGKILLs to the group while the owner waits for
/// cleanup. It does not depend on process inspection.
pub const KILL_REPEAT_INTERVAL: Duration = Duration::from_millis(100);
/// Pause between cleanup passes (inspect, then signal tracked descendants).
pub const CLEANUP_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Deadline for one `ps` run.
pub const PS_TIMEOUT: Duration = Duration::from_millis(1500);
/// Most fresh listings taken after one whose only unparsable lines are rows
/// for an exiting process (Linux only), within `PS_TIMEOUT` of the
/// inspection's start.
pub const EXITING_ROW_RETRIES: u32 = 1;
/// Most bytes of `ps` output accepted.
pub const PS_MAX_BYTES: usize = 4_000_000;
/// Most bytes of the marker listing accepted (`ps -E`, macOS), which carries
/// every environment of this user's processes. About 300 KB was measured with
/// some 1,000 processes on a development machine.
pub const MARKER_PS_MAX_BYTES: usize = 32_000_000;

/// Most listings one freeze takes. A freeze stops (SIGSTOP) every process a
/// listing finds outside the group and lists again until a listing finds
/// nothing that is not already stopped; a run of it that ends without that is
/// incomplete and the verdict cannot be `Confirmed`.
pub const FREEZE_MAX_ROUNDS: u32 = 6;
/// How long the keeper may take to start the program and say so before the
/// run is refused.
pub const KEEPER_START_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the owner waits for the keeper to answer one request (a census of
/// its tree, a freeze, or the reap).
pub const KEEPER_REPLY_TIMEOUT: Duration = Duration::from_secs(5);
/// Added to the time limit and the cleanup deadline for the keeper's own
/// ceiling: if the owner has not had the program reaped by then, the keeper
/// stops the tree by itself, as when the owner is gone.
pub const KEEPER_CEILING_MARGIN: Duration = Duration::from_secs(10);
/// How long a keeper that stops the tree on its own (a signal, its ceiling)
/// waits for the owner to answer `released` before it reaps the program.
pub const KEEPER_RELEASE_WAIT: Duration = Duration::from_secs(2);
/// How long the owner waits for a keeper that did not answer the handshake to
/// stop whatever it started and exit, after closing its end of the socket.
pub const KEEPER_ABANDON_WAIT: Duration = Duration::from_secs(5);
/// How often the keeper reaps adopted orphans and checks that its parent is
/// still the owner.
pub const KEEPER_TICK: Duration = Duration::from_millis(100);
/// Most passes of the keeper's tree walk in one freeze (Linux).
pub const KEEPER_FREEZE_PASSES: u32 = 64;

/// Size of the buffer for one read from standard output.
pub const STDOUT_READ_CHUNK: usize = 64 * 1024;
/// Size of the buffer for one read from standard error.
pub const STDERR_READ_CHUNK: usize = 16 * 1024;
/// A tail capture is trimmed when it reaches this multiple of its bound, so
/// trimming is amortised and memory stays below the multiple plus one chunk.
pub const TAIL_TRIM_FACTOR: usize = 2;

/// Most signal records kept in the diagnostics. Later ones are counted.
pub const MAX_SIGNAL_RECORDS: usize = 64;
/// Longest inspection error text kept in the diagnostics, in characters.
pub const MAX_ERROR_CHARS: usize = 1000;
/// Least time the last cleanup pass gets after the owner loop gave up, so a
/// deadline that has already passed still allows one inspection and verdict.
pub const FINAL_CLEANUP_PASS: Duration = Duration::from_millis(1000);

/// How long one sweep that signals descendants may take before the owner moves
/// on. The group is signalled first and without waiting; this only bounds the
/// part that needs a process listing, so a slow or hung listing cannot hold the
/// owner. A stop sweep that runs out of it is repeated without this bound, beside
/// the owner's reads (see `TERM_SWEEP_RETRY_PAUSE`).
pub const DESCENDANT_SIGNAL_BUDGET: Duration = Duration::from_millis(250);
/// First pause before the repeat of a stop sweep (TERM, or KILL after the
/// grace) whose bounded first attempt did not complete within
/// `DESCENDANT_SIGNAL_BUDGET`. The repeat runs one unbounded sweep at a time
/// and pauses again, doubling up to `TERM_SWEEP_RETRY_MAX_PAUSE`, only after a
/// failed or timed-out listing.
pub const TERM_SWEEP_RETRY_PAUSE: Duration = Duration::from_millis(100);
/// Longest pause between repeats of a stop sweep.
pub const TERM_SWEEP_RETRY_MAX_PAUSE: Duration = Duration::from_millis(2000);
/// Consecutive failed listings during cleanup after which inspection is
/// treated as unavailable and cleanup finishes with what it can check.
pub const INSPECTION_FAILURE_LIMIT: u32 = 3;
/// Longest wait for the leader to exit after the final kill when inspection is
/// unavailable.
pub const LEADER_EXIT_WAIT: Duration = Duration::from_millis(1000);
/// Poll interval for that wait and for a stopped leader.
pub const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(10);
/// Least margin `cleanup_deadline` must have beyond `stop_grace` plus
/// `drain_bound`; a smaller deadline is raised to that sum.
pub const CLEANUP_DEADLINE_MARGIN: Duration = Duration::from_millis(500);
/// A timer far enough away to stand for "never" in a select arm.
pub const FAR_FUTURE: Duration = Duration::from_secs(86_400);
/// Size of the buffer for one read of `ps` output.
pub const PS_READ_CHUNK: usize = 64 * 1024;
/// Most session members recorded in the diagnostics.
pub const MAX_SWEPT_RECORDS: usize = 64;
/// Consecutive clean sweeps required to confirm cleanup when nothing was ever
/// observed or signalled beyond the leader: a plain exit.
pub const CLEAN_SWEEPS_PLAIN: u32 = 1;
/// Consecutive clean sweeps required once any descendant was observed or
/// signalled. A sweep that follows a kill confirms it only if the next one
/// agrees.
pub const CLEAN_SWEEPS_AFTER_ACTIVITY: u32 = 2;

/// Most output chunks queued for a stdout tap. A full queue drops chunks, so a
/// slow tap cannot hold the owner loop.
pub const TAP_QUEUE_CHUNKS: usize = 256;
/// How long, after cleanup, the owner waits for a stdout tap to finish what is
/// queued before it reports and leaves the tap behind.
pub const TAP_DRAIN_WAIT: Duration = Duration::from_millis(250);
