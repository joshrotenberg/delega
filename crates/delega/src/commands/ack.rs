//! Test seams, in debug builds only: acknowledgements a test waits for instead
//! of sleeping and guessing that `delega` has reached a state.
//!
//! Each [`Ack`] is active only when its environment variable names a path. The
//! first time the state is reached, `delega` creates that file new and writes
//! one line to it in a single write; a later arrival, or a path that already
//! exists (a FIFO included), writes nothing and never blocks. A test waits for
//! the file to exist, with a bound.
//!
//! A release build compiles none of this: [`acknowledge`] and the other entry
//! points are empty there, and the variable names are not in the binary.

/// A state a test can wait for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ack {
    /// The result writer of `delega run` found standard output not writable
    /// (a full pipe) and is waiting for a reader.
    ResultBlocked,
    /// `delega run` asked its result write to stop, after a signal or at the
    /// write deadline, and is waiting for the print thread to say how the
    /// write ended.
    ResultStopRequested,
    /// The standard error writer found standard error not writable (a full
    /// pipe) and is about to block writing a line to it.
    StderrBlocked,
    /// `delega mcp` found its standard output not writable (a full pipe) while
    /// serving: the transport's next write blocks.
    McpStdoutBlocked,
}

#[cfg(debug_assertions)]
mod hooks {
    use std::io::Write;
    use std::os::fd::AsFd;
    use std::path::Path;

    use rustix::event::{PollFd, PollFlags, Timespec, poll};

    use super::Ack;

    /// The variable that turns on `ack`, naming the file to create.
    const fn variable(ack: Ack) -> &'static str {
        match ack {
            Ack::ResultBlocked => "DELEGA_TEST_ACK_RESULT_BLOCKED",
            Ack::ResultStopRequested => "DELEGA_TEST_ACK_RESULT_STOP_REQUESTED",
            Ack::StderrBlocked => "DELEGA_TEST_ACK_STDERR_BLOCKED",
            Ack::McpStdoutBlocked => "DELEGA_TEST_ACK_MCP_STDOUT_BLOCKED",
        }
    }

    /// The prefix of the files [`signal`] creates, one per signal.
    const SIGNAL: &str = "DELEGA_TEST_ACK_RUN_SIGNAL";

    /// Create `path` new and write one line to it, once. An existing path is
    /// left alone.
    fn write_new(path: &Path) {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            let _ = file.write_all(b"ack\n");
        }
    }

    pub fn acknowledge(ack: Ack) {
        if let Some(path) = std::env::var_os(variable(ack)) {
            write_new(Path::new(&path));
        }
    }

    pub fn signal(count: u64) {
        if let Some(prefix) = std::env::var_os(SIGNAL) {
            let mut path = prefix;
            path.push(format!("-{count}"));
            write_new(Path::new(&path));
        }
    }

    /// Whether `fd` is not writable right now. An error or hangup counts as
    /// writable: the write itself reports it.
    fn full(fd: impl AsFd) -> bool {
        let mut fds = [PollFd::new(&fd, PollFlags::OUT)];
        matches!(poll(&mut fds, Some(&Timespec::default())), Ok(0))
    }

    pub fn if_full(fd: impl AsFd, ack: Ack) {
        if std::env::var_os(variable(ack)).is_some() && full(fd) {
            acknowledge(ack);
        }
    }

    pub fn watch_stdout(ack: Ack) {
        if std::env::var_os(variable(ack)).is_none() {
            return;
        }
        std::thread::spawn(move || {
            let stdout = std::io::stdout();
            loop {
                if full(stdout.as_fd()) {
                    acknowledge(ack);
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        });
    }
}

/// Acknowledge `ack` when its variable is set.
#[cfg(debug_assertions)]
pub fn acknowledge(ack: Ack) {
    hooks::acknowledge(ack);
}

/// Acknowledge the `count`th signal `delega run` handled while waiting for the
/// run, with `DELEGA_TEST_ACK_RUN_SIGNAL` naming a path prefix: the file is
/// the prefix followed by `-count`.
#[cfg(debug_assertions)]
pub fn signal(count: u64) {
    hooks::signal(count);
}

/// Acknowledge `ack` when its variable is set and `fd` is not writable now.
#[cfg(debug_assertions)]
pub fn if_full(fd: impl std::os::fd::AsFd, ack: Ack) {
    hooks::if_full(fd, ack);
}

/// When the variable of `ack` is set, watch standard output on a thread of
/// its own and acknowledge `ack` the first time it is not writable.
#[cfg(debug_assertions)]
pub fn watch_stdout(ack: Ack) {
    hooks::watch_stdout(ack);
}

#[cfg(not(debug_assertions))]
pub const fn acknowledge(_: Ack) {}

#[cfg(not(debug_assertions))]
pub const fn signal(_: u64) {}

#[cfg(not(debug_assertions))]
pub fn if_full(_: impl std::os::fd::AsFd, _: Ack) {}

#[cfg(not(debug_assertions))]
pub const fn watch_stdout(_: Ack) {}
