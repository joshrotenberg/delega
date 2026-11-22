//! Every numeric tunable of the supervisor.
//!
//! The defaults for [`Limits`](super::Limits) and the fixed bounds on what a
//! read returns are named here so a change is one line and the tests and
//! `docs/supervisor.md` can refer to them.

use std::time::Duration;

/// Most workers that may be live at once. A closed worker does not count.
pub const MAX_LIVE_WORKERS: usize = 16;
/// Most runs that may be running at once. A run beyond it waits in the queue.
pub const MAX_RUNNING_RUNS: usize = 4;
/// Most runs that may wait on one worker, behind its running run.
pub const MAX_QUEUED_PER_WORKER: usize = 16;
/// Most finished results kept readable, across the whole supervisor.
pub const RETAINED_RESULTS: usize = 256;
/// Most bytes of finished results kept readable, estimated from their text,
/// structured answer, notes and partial text. The oldest are dropped first; the
/// newest result is always kept.
pub const RETAINED_BYTES: usize = 64 * 1024 * 1024;
/// How long a finished result stays readable: one hour.
pub const RETENTION: Duration = Duration::from_secs(60 * 60);
/// The longest a single `wait` blocks, whatever timeout the caller asks for.
pub const WAIT_CEILING: Duration = Duration::from_secs(300);
/// The timeout `wait` uses when the caller gives none.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(30);

/// The longest `shutdown` waits for runs to settle after asking them to stop.
/// The process layer bounds each stop on its own, by its cleanup deadline.
pub const SHUTDOWN_BOUND: Duration = Duration::from_secs(30);
/// The longest `interrupt` or `close` waits for a running run to settle.
pub const INTERRUPT_BOUND: Duration = Duration::from_secs(30);
/// How often the bounds of `wait`, `interrupt` and `close` compare wall time
/// with their limit. Their timers count only the time the machine was awake;
/// this check ends them once the limit has passed in wall time, within this
/// long of the machine waking.
pub(super) const WALL_CHECK_INTERVAL: Duration = Duration::from_secs(1);
/// The longest `start` spends checking the request's paths, executable and
/// schema before it is admitted.
pub const PREFLIGHT_BOUND: Duration = Duration::from_secs(10);
/// Most request checks that may be outstanding at once. Each `start` checks its
/// paths on an OS thread of its own, and a check on a filesystem that does not
/// answer keeps its thread until it does, even after `start` gave up waiting. A
/// `start` that finds every slot taken waits for one, without a thread, within
/// [`PREFLIGHT_BOUND`] counted from when it began; a burst of concurrent starts
/// is served in turn. One that gets no slot in time is refused
/// `preflight_timeout`. The value equals [`MAX_LIVE_WORKERS`]; it bounds the
/// threads a hung filesystem can cost.
pub const MAX_PENDING_PREFLIGHTS: usize = MAX_LIVE_WORKERS;

/// Most activity entries kept per run. Older ones are counted and dropped.
pub const MAX_ACTIVITY: usize = 200;
/// Most characters of partial text kept per run. The end of the text is kept.
pub const MAX_PARTIAL_TEXT_CHARS: usize = 16 * 1024;
/// Activity entries a compact read of an unfinished run returns.
pub const COMPACT_ACTIVITY: usize = 10;
/// Characters of partial text a compact read returns, taken from the end.
pub const COMPACT_PARTIAL_TEXT_CHARS: usize = 2_000;

/// Characters of the answer a compact read returns. A longer answer is cut and
/// the response says so and gives the full length; `detail: full` returns all.
pub const COMPACT_RESULT_TEXT_CHARS: usize = 20_000;

/// Serialized bytes of the structured answer a compact read returns. A larger one
/// is left out and the response says so and gives its size; `detail: full`
/// returns it.
pub const COMPACT_STRUCTURED_BYTES: usize = 16 * 1024;

/// Most run or worker ids one `wait` may name.
pub const MAX_WAIT_TARGETS: usize = 256;
