//! The one place a command writes its result to standard output.

use std::io::{ErrorKind, Write};
use std::process::ExitCode;

/// Write `bytes` to locked stdout and flush.
///
/// A closed pipe (`delega skill list | head -1`) is a quiet success: the
/// reader has what it wanted. Any other write error is reported on stderr,
/// naming `command`, and exits with status 1.
pub fn emit(command: &str, bytes: &[u8]) -> ExitCode {
    let mut out = std::io::stdout().lock();
    match out.write_all(bytes).and_then(|()| out.flush()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) if err.kind() == ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("delega {command}: cannot write to stdout: {err}");
            ExitCode::from(1)
        }
    }
}
