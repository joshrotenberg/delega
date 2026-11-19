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
/// How long a finished result stays readable: one hour.
pub const RETENTION: Duration = Duration::from_secs(60 * 60);
/// The longest a single `wait` blocks, whatever timeout the caller asks for.
pub const WAIT_CEILING: Duration = Duration::from_secs(300);
/// The timeout `wait` uses when the caller gives none.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(60);

/// The longest `shutdown` waits for runs to settle after asking them to stop.
/// The process layer bounds each stop on its own, by its cleanup deadline.
pub const SHUTDOWN_BOUND: Duration = Duration::from_secs(30);
/// The longest `interrupt` or `close` waits for a running run to settle.
pub const INTERRUPT_BOUND: Duration = Duration::from_secs(30);
/// Slack added to [`SHUTDOWN_BOUND`] for the blocking wait in `Drop`.
pub const DROP_SLACK: Duration = Duration::from_secs(2);

/// Most activity entries kept per run. Older ones are counted and dropped.
pub const MAX_ACTIVITY: usize = 200;
/// Most characters of partial text kept per run. The end of the text is kept.
pub const MAX_PARTIAL_TEXT_CHARS: usize = 16 * 1024;
/// Activity entries a compact read of an unfinished run returns.
pub const COMPACT_ACTIVITY: usize = 10;
/// Characters of partial text a compact read returns, taken from the end.
pub const COMPACT_PARTIAL_TEXT_CHARS: usize = 2_000;

/// Most run or worker ids one `wait` may name.
pub const MAX_WAIT_TARGETS: usize = 256;
