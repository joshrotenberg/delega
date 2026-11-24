//! Test seams, in debug builds only: acknowledgements a test waits for instead
//! of sleeping and guessing that `delega` has reached a state.
//!
//! Each seam observes the state itself (a signal handled, a stop requested),
//! never a sign that usually comes with it. Each is active only when its
//! environment variable names a path. The first time the state is reached,
//! `delega` creates that file new and writes one line to it in a single write,
//! on a thread of its own so the caller never waits on the filesystem; a later
//! arrival, or a path that already exists (a FIFO included), writes nothing. A
//! test waits for the file to exist, with a bound. Nothing runs, and no thread
//! is started, unless the variable is set.
//!
//! A release build compiles none of this: the entry points are empty there,
//! and the variable names are not in the binary.

/// A state a test can wait for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ack {
    /// `delega run` asked its result write to stop, after a signal or at the
    /// write deadline, and is waiting for the print thread to say how the
    /// write ended.
    ResultStopRequested,
}

#[cfg(debug_assertions)]
mod hooks {
    use std::ffi::OsString;
    use std::io::Write;
    use std::path::Path;

    use super::Ack;

    /// The variable that turns on `ack`, naming the file to create.
    const fn variable(ack: Ack) -> &'static str {
        match ack {
            Ack::ResultStopRequested => "DELEGA_TEST_ACK_RESULT_STOP_REQUESTED",
        }
    }

    /// The prefix of the files [`signal`] creates, one per signal.
    const SIGNAL: &str = "DELEGA_TEST_ACK_RUN_SIGNAL";

    /// Create `path` new and write one line to it, once, on a thread of its
    /// own: the callers are on the signal path, and creating a file can
    /// block (a stalled mount, say). An existing path is left alone, and a
    /// thread that cannot be started writes nothing.
    fn write_new(path: OsString) {
        let _ = std::thread::Builder::new()
            .name("delega-test-ack".into())
            .spawn(move || {
                if let Ok(mut file) = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(Path::new(&path))
                {
                    let _ = file.write_all(b"ack\n");
                }
            });
    }

    pub fn acknowledge(ack: Ack) {
        if let Some(path) = std::env::var_os(variable(ack)) {
            write_new(path);
        }
    }

    pub fn signal(count: u64) {
        if let Some(mut path) = std::env::var_os(SIGNAL) {
            path.push(format!("-{count}"));
            write_new(path);
        }
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

#[cfg(not(debug_assertions))]
pub const fn acknowledge(_: Ack) {}

#[cfg(not(debug_assertions))]
pub const fn signal(_: u64) {}
