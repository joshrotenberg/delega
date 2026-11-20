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
use std::time::Instant;

pub trait Guarded {
    /// `spawn` while holding the spawn lock.
    fn guarded_spawn(&mut self) -> io::Result<Child>;
    /// `spawn` while holding the spawn lock, with the instant taken once the
    /// lock is held, just before the spawn. A case is timed from that instant,
    /// so time spent queued behind another job's spawn does not count toward
    /// its elapsed time, its scheduled actions or its hang limit.
    fn guarded_spawn_timed(&mut self) -> io::Result<(Child, Instant)>;
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

    #[allow(clippy::disallowed_methods)]
    fn guarded_spawn_timed(&mut self) -> io::Result<(Child, Instant)> {
        let _held = delega_core::process::spawn_guard();
        let started = Instant::now();
        Ok((self.spawn()?, started))
    }

    fn guarded_output(&mut self) -> io::Result<Output> {
        self.guarded_spawn()?.wait_with_output()
    }

    fn guarded_status(&mut self) -> io::Result<ExitStatus> {
        self.guarded_spawn()?.wait()
    }
}

#[cfg(test)]
mod tests {
    use super::Guarded;
    use std::process::{Command, Stdio};
    use std::sync::mpsc::channel;
    use std::time::{Duration, Instant};

    #[test]
    fn a_timed_spawn_takes_its_start_instant_after_waiting_for_the_spawn_lock() {
        // The holder takes the lock, says so, and keeps it long enough for
        // the test thread to be waiting on it. Its last act while holding the
        // lock is to record the instant, so any instant taken under the lock
        // afterwards is not earlier, however the threads are scheduled.
        let (held, is_held) = channel();
        let holder = std::thread::spawn(move || {
            let guard = delega_core::process::spawn_guard();
            held.send(()).expect("signal that the lock is held");
            std::thread::sleep(Duration::from_millis(200));
            let released = Instant::now();
            drop(guard);
            released
        });
        is_held.recv().expect("the holder took the lock");
        let (mut child, started) = Command::new("/usr/bin/true")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .guarded_spawn_timed()
            .expect("spawn true");
        child.wait().expect("wait for true");
        let released = holder.join().expect("holder thread");
        assert!(
            started >= released,
            "the start instant was taken {:?} before the holder released the lock",
            released.duration_since(started)
        );
    }
}
