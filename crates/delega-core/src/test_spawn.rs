//! std's `Command::spawn`, `output` and `status` for tests, under the process
//! layer's spawn lock (#104).
//!
//! The unit tests run in one process, beside process-layer runs of other
//! tests. A fixture that spawns `ps` or `mkfifo` without the lock can, on
//! macOS, hand a gated fake provider the write end of its output pipe (so the
//! fixture waits until that provider exits) or give its own child a provider's
//! standard input (so that run waits for the fixture's child). clippy's
//! `disallowed_methods` (`clippy.toml` in this crate) sends every test spawn
//! through here.

use std::io;
use std::process::{Child, Command, ExitStatus, Output, Stdio};

use crate::process::spawn_guard;

pub(crate) trait Guarded {
    /// `spawn` while holding the spawn lock.
    fn guarded_spawn(&mut self) -> io::Result<Child>;
    /// `output` while holding the spawn lock for the spawn only. Like `output`,
    /// standard input is `/dev/null` and both outputs are captured; this
    /// replaces any standard streams set before.
    fn guarded_output(&mut self) -> io::Result<Output>;
    /// `status` while holding the spawn lock for the spawn only.
    fn guarded_status(&mut self) -> io::Result<ExitStatus>;
}

impl Guarded for Command {
    #[allow(clippy::disallowed_methods)]
    fn guarded_spawn(&mut self) -> io::Result<Child> {
        let _held = spawn_guard();
        self.spawn()
    }

    fn guarded_output(&mut self) -> io::Result<Output> {
        self.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .guarded_spawn()?
            .wait_with_output()
    }

    fn guarded_status(&mut self) -> io::Result<ExitStatus> {
        self.guarded_spawn()?.wait()
    }
}
