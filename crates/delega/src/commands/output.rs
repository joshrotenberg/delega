//! The one place a command writes its result to standard output.

use std::io::{ErrorKind, Write};
use std::process::ExitCode;

/// What happened to a write to standard output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Written {
    /// Everything was written and flushed.
    Done,
    /// The reader closed the pipe. The reader has what it wanted.
    Closed,
    /// Any other error, already reported on stderr.
    Failed,
}

/// Write `bytes` to locked stdout and flush.
///
/// A closed pipe (`delega skill list | head -1`) is [`Written::Closed`], not an
/// error. Any other write error is reported on stderr, naming `command`.
pub fn write(command: &str, bytes: &[u8]) -> Written {
    let mut out = std::io::stdout().lock();
    match out.write_all(bytes).and_then(|()| out.flush()) {
        Ok(()) => Written::Done,
        Err(err) if err.kind() == ErrorKind::BrokenPipe => Written::Closed,
        Err(err) => {
            report(&format!("delega {command}: cannot write to stdout: {err}"));
            Written::Failed
        }
    }
}

/// Write `bytes` to stdout. A closed pipe is a quiet success; any other write
/// error exits with status 1.
pub fn emit(command: &str, bytes: &[u8]) -> ExitCode {
    match write(command, bytes) {
        Written::Done | Written::Closed => ExitCode::SUCCESS,
        Written::Failed => ExitCode::from(1),
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
