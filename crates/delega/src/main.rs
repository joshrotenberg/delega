//! The `delega` command line.
//!
//! Nothing is implemented yet, so every invocation says so and exits with the
//! status `docs/api.md` assigns to a refused run. Printing a fixed message here
//! keeps a half-built binary from looking like a working one.

use std::process::ExitCode;

/// Exit status for a refused run, as `docs/api.md` defines it.
const REFUSED: u8 = 2;

fn main() -> ExitCode {
    eprintln!(
        "delega {}: no commands are implemented yet; see docs/api.md for the design",
        delega_mcp::VERSION
    );
    ExitCode::from(REFUSED)
}
