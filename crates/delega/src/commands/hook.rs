//! `delega command-guard`: the exact command guard Claude Code calls.
//!
//! Not part of the public surface and hidden from help. `delega run` writes this
//! subcommand into the settings it gives Claude Code, which starts it before a
//! `Bash` tool call, with the call on standard input and the allowed commands
//! in the `DELEGA_EXACT_BASH_GRANTS` variable. It exits 0 to allow the call and
//! 2 to deny it.

use std::process::ExitCode;

use clap::Parser;
use delega_core::provider::hook;

/// The name of the hidden subcommand, as `delega run` passes it to the hook
/// setting.
pub const NAME: &str = "command-guard";

#[derive(Debug, Parser)]
pub struct Args {}

pub fn run(_args: Args) -> ExitCode {
    let grants = std::env::var(hook::GRANTS_ENV).ok();
    let code = hook::run(
        &mut std::io::stdin().lock(),
        grants.as_deref(),
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    );
    ExitCode::from(u8::try_from(code).unwrap_or(2))
}
