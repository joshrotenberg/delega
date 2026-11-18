//! Every numeric tunable of the process layer.
//!
//! The defaults for a [`ProcessSpec`](super::ProcessSpec) and the fixed
//! intervals of the owner loop are named here so a change is one line and the
//! tests and documentation can refer to them.

use std::time::Duration;

/// Grace between the polite stop (SIGTERM) and the kill (SIGKILL) when a spec
/// does not set one. The source default for model processes was 1.5 s.
pub const DEFAULT_STOP_GRACE: Duration = Duration::from_millis(1500);
/// How long output pipes may stay open after the kill before the owner closes
/// its ends and says so. The source default was 2 s.
pub const DEFAULT_DRAIN_BOUND: Duration = Duration::from_millis(2000);
/// How long, from the first stop signal, the owner keeps signalling before it
/// gives up and reports cleanup as unconfirmed. The source waited without
/// bound and only emitted a notification at its 10 s stop deadline; the
/// contract requires a result, so this bound ends the wait.
pub const DEFAULT_CLEANUP_DEADLINE: Duration = Duration::from_millis(10_000);

/// Bytes kept from standard output when a spec does not set a bound.
pub const DEFAULT_STDOUT_CAPTURE_BYTES: usize = 16 * 1024 * 1024;
/// Bytes kept from standard error when a spec does not set a bound.
pub const DEFAULT_STDERR_CAPTURE_BYTES: usize = 256 * 1024;

/// Interval between descendant scans while the child runs.
pub const CAPTURE_INTERVAL: Duration = Duration::from_millis(500);
/// How long a stop waits for one fresh descendant scan before signalling.
pub const PRE_STOP_CAPTURE_BUDGET: Duration = Duration::from_millis(25);
/// Poll interval while a stop waits for that scan.
pub const PRE_STOP_POLL_INTERVAL: Duration = Duration::from_millis(1);

/// Interval between repeated SIGKILLs to the group while the owner waits for
/// cleanup. It does not depend on process inspection.
pub const KILL_REPEAT_INTERVAL: Duration = Duration::from_millis(100);
/// Pause between cleanup passes (inspect, then signal tracked descendants).
pub const CLEANUP_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Deadline for one `ps` run.
pub const PS_TIMEOUT: Duration = Duration::from_millis(1500);
/// Most bytes of `ps` output accepted.
pub const PS_MAX_BYTES: usize = 4_000_000;

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
