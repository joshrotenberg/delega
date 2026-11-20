//! The one place a command writes its result to standard output.

use std::fs::File;
use std::io::{ErrorKind, Write};
use std::os::fd::AsFd;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::Errno;
use rustix::pipe::PIPE_BUF;

/// How long [`write_unless_stopped`] waits for standard output to become
/// writable before it looks again whether it was asked to stop.
const STOP_POLL: Timespec = Timespec {
    tv_sec: 0,
    tv_nsec: 20_000_000,
};

/// What happened to a write to standard output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Written {
    /// Everything was written.
    Done,
    /// The reader closed the pipe. The reader has what it wanted.
    Closed,
    /// Any other error, with the message to report.
    Failed(String),
}

/// Write `bytes` to standard output.
///
/// The write goes to a duplicate of descriptor 1, not through `std`'s stdout,
/// because `std` treats EBADF (a descriptor that is not open for writing) as a
/// successful write and the result would be lost silently. (A descriptor 1 that
/// was closed before start is reopened on /dev/null by the Rust runtime, so
/// that case cannot be told from a redirect to it.) A closed pipe (`delega skill list | head -1`)
/// is [`Written::Closed`], not an error. Anything else is [`Written::Failed`].
pub fn write(bytes: &[u8]) -> Written {
    let owned = match std::io::stdout().as_fd().try_clone_to_owned() {
        Ok(owned) => owned,
        Err(err) => return Written::Failed(format!("standard output is not open: {err}")),
    };
    let mut out = File::from(owned);
    match out.write_all(bytes).and_then(|()| out.flush()) {
        Ok(()) => Written::Done,
        Err(err) if err.kind() == ErrorKind::BrokenPipe => Written::Closed,
        Err(err) => Written::Failed(format!("cannot write to stdout: {err}")),
    }
}

/// A request to stop a [`write_unless_stopped`] from another thread, and
/// whether the write did stop before the end.
#[derive(Debug, Default)]
pub struct Stop {
    requested: AtomicBool,
    stopped: AtomicBool,
}

impl Stop {
    /// Ask the write to stop before its next piece.
    pub fn request(&self) {
        self.requested.store(true, Ordering::SeqCst);
    }

    /// Whether a write stopped with bytes left unwritten. Read once the writing
    /// thread has finished, this says exactly whether the whole text went out.
    pub fn stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }
}

/// Write `bytes` to standard output as [`write`] does, unless `stop` is
/// requested first: then the write ends with bytes left unwritten, records
/// that in `stop`, and returns `None`.
///
/// The bytes go out in pieces of at most `PIPE_BUF` bytes, each written only
/// after `poll` reports standard output writable, and `stop` is checked before
/// each piece. The mode of standard output is left as inherited: its file
/// description may be shared with other processes, so `O_NONBLOCK` is never
/// set, and a descriptor the caller made non-blocking stays so (its `EAGAIN` is
/// a write failure).
///
/// [`STOP_POLL`] bounds only the wait for readiness. When a pipe has `delega`
/// as its only writer, a piece written after `POLLOUT` fits (Linux reports
/// `POLLOUT` while the pipe has a free page), so the thread comes back to
/// `stop` within one `STOP_POLL` and one piece. A write after `POLLOUT` can
/// still block: when another writer shares the pipe; on macOS when pipe memory
/// is over its growth cap, because XNU's `POLLOUT` check counts
/// `MAX_PIPESIZE`, which can exceed the buffer the pipe has; on a terminal with
/// flow control, whose readiness does not promise room for a whole piece; and
/// on a regular file on a stalled filesystem. A stop then waits for the
/// caller's fallback (`PRINT_STOP_WAIT` in `delega run`).
///
/// An interrupted write (`EINTR`) is retried after checking `stop`. A closed
/// pipe (`EPIPE`) is [`Written::Closed`]. Any other error, `EAGAIN` from a
/// descriptor another process set to non-blocking included, is
/// [`Written::Failed`].
///
/// The value returned is exact: `Some(Written::Done)` only once every byte was
/// accepted by the kernel, and `None` only when bytes remain unwritten.
///
/// Cost on the normal path, measured in a review on an Apple M4 Pro with
/// release builds, 20 alternated runs per revision, about 3 MB drained through
/// a pipe: the median time from the first to the last byte went from 0.73 ms
/// to 5.59 ms for a Claude result and from 0.75 ms to 5.72 ms for a Codex one.
pub fn write_unless_stopped(bytes: &[u8], stop: &Stop) -> Option<Written> {
    let owned = match std::io::stdout().as_fd().try_clone_to_owned() {
        Ok(owned) => owned,
        Err(err) => {
            return Some(Written::Failed(format!(
                "standard output is not open: {err}"
            )));
        }
    };
    let mut out = File::from(owned);
    let mut rest = bytes;
    while !rest.is_empty() {
        if stop.requested.load(Ordering::SeqCst) {
            stop.stopped.store(true, Ordering::SeqCst);
            return None;
        }
        let mut fds = [PollFd::new(&out, PollFlags::OUT)];
        match poll(&mut fds, Some(&STOP_POLL)) {
            Ok(0) | Err(Errno::INTR) => continue,
            // Writable, or an error or hangup the write itself reports.
            Ok(_) => {}
            Err(err) => {
                return Some(Written::Failed(format!(
                    "cannot wait for stdout to be writable: {err}"
                )));
            }
        }
        match out.write(&rest[..rest.len().min(PIPE_BUF)]) {
            Ok(0) => {
                return Some(Written::Failed(
                    "cannot write to stdout: wrote nothing".into(),
                ));
            }
            Ok(n) => rest = &rest[n..],
            Err(err) if err.kind() == ErrorKind::Interrupted => {}
            Err(err) if err.kind() == ErrorKind::BrokenPipe => return Some(Written::Closed),
            Err(err) => return Some(Written::Failed(format!("cannot write to stdout: {err}"))),
        }
    }
    Some(Written::Done)
}

/// Write `bytes` to stdout. A closed pipe is a quiet success; any other write
/// error is reported on stderr, naming `command`, and exits with status 1.
pub fn emit(command: &str, bytes: &[u8]) -> ExitCode {
    match write(bytes) {
        Written::Done | Written::Closed => ExitCode::SUCCESS,
        Written::Failed(message) => {
            report(&format!("delega {command}: {message}"));
            ExitCode::from(1)
        }
    }
}

/// Write one line to standard error. A failure to write is ignored: there is
/// nowhere left to report it, and `eprintln!` would panic.
pub fn report(line: &str) {
    let mut err = std::io::stderr().lock();
    let _ = err
        .write_all(line.as_bytes())
        .and_then(|()| err.write_all(b"\n"));
}
