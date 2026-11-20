//! One lock over starting child processes (#104).
//!
//! On macOS std creates a child's pipes with `pipe()` and marks them
//! close-on-exec only afterwards (Linux uses `pipe2` with `O_CLOEXEC`). A child
//! forked by another thread in between inherits those pipe ends and keeps them
//! through `exec`. A provider that inherits another run's standard input keeps
//! that run from ever reading end of input; one that inherits another run's
//! output keeps that pipe open after its provider exits.
//!
//! Every child the process layer starts (the provider, its version probe and
//! the `ps` listing) is spawned while holding this lock, so none of them is
//! forked while another of them has a pipe without close-on-exec. A spawn made
//! elsewhere in the same program is covered only if it holds [`spawn_guard`]
//! too.

use std::sync::{Mutex, MutexGuard, PoisonError};

static SPAWN: Mutex<()> = Mutex::new(());

/// Holds the process layer's spawn lock until dropped. See [`spawn_guard`].
#[must_use = "the lock is released as soon as the guard is dropped"]
pub struct SpawnGuard {
    _held: MutexGuard<'static, ()>,
}

/// Take the lock the process layer holds while it starts a child.
///
/// A program that embeds delega and starts processes of its own on other
/// threads can hold this guard across each of its spawn calls, so that neither
/// its children nor the processes delega starts inherit the other's pipes on
/// macOS.
///
/// Hold it only across the spawn call itself, then drop it before waiting for
/// the child or reading its output. Every process delega starts waits for it,
/// so a guard held while calling into delega, or held across an `.await` on a
/// runtime thread that a run needs (a `LocalSet`, or a current-thread
/// runtime), deadlocks that run. A spawn that blocks in the kernel holds up
/// every other spawn for as long, and a run's time limit cannot interrupt it.
pub fn spawn_guard() -> SpawnGuard {
    SpawnGuard {
        // A panic while the lock was held leaves nothing to repair: the lock
        // guards no data, only the moment of the spawn.
        _held: SPAWN.lock().unwrap_or_else(PoisonError::into_inner),
    }
}

/// Start `command` while holding the spawn lock. Every spawn of the process
/// layer goes through here; clippy's `disallowed_methods` (see `clippy.toml`)
/// keeps a direct `spawn` from bypassing it.
#[allow(clippy::disallowed_methods)]
pub(super) fn spawn(
    command: &mut tokio::process::Command,
) -> std::io::Result<tokio::process::Child> {
    let _held = spawn_guard();
    command.spawn()
}
