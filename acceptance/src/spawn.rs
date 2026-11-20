//! std's `Command::spawn`, `output` and `status` under delega's spawn lock
//! (#107).
//!
//! The harness runs cases on parallel jobs, each starting `delega`, `ps` and
//! `perl`. On macOS std creates a child's pipes and sets close-on-exec on them
//! afterwards, so a child one job forks inside that window inherits another
//! job's pipe ends: a reader then waits for an unrelated child to exit. Each
//! spawning method holds `delega_core::process::spawn_guard()` for the spawn
//! call only, which covers the pipes std creates during that call, and drops
//! it before waiting or reading output. Held longer, it would serialize every
//! case's processes for their whole life. clippy's `disallowed_methods` (the
//! workspace `clippy.toml`) rejects a direct spawn, `output` or `status`.
//!
//! `guarded_output` never changes the standard streams. std's `output`
//! defaults standard input to null and captures both outputs only where the
//! caller set nothing, and std cannot tell which streams were set, so each
//! caller sets every stream it relies on.

use std::io;
use std::process::{Child, Command, ExitStatus, Output};

pub trait Guarded {
    /// `spawn` while holding the spawn lock.
    fn guarded_spawn(&mut self) -> io::Result<Child>;
    /// Spawn under the lock with the streams as set, then wait and collect
    /// what was captured, as `output` does.
    fn guarded_output(&mut self) -> io::Result<Output>;
    /// `status` with the lock held for the spawn only.
    fn guarded_status(&mut self) -> io::Result<ExitStatus>;
}

impl Guarded for Command {
    #[allow(clippy::disallowed_methods)]
    fn guarded_spawn(&mut self) -> io::Result<Child> {
        let _held = delega_core::process::spawn_guard();
        self.spawn()
    }

    fn guarded_output(&mut self) -> io::Result<Output> {
        self.guarded_spawn()?.wait_with_output()
    }

    fn guarded_status(&mut self) -> io::Result<ExitStatus> {
        self.guarded_spawn()?.wait()
    }
}
