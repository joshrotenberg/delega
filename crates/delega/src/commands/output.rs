//! The one place a command writes its result to standard output.

use std::fs::File;
use std::io::{ErrorKind, Write};
use std::os::fd::AsFd;
use std::process::ExitCode;

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
