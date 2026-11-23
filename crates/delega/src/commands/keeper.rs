//! `delega process-keeper`: the keeper between delega and a provider.
//!
//! Not part of the public surface and hidden from help. `delega run` and
//! `delega mcp` start this subcommand for every provider process, with one end
//! of a socket pair as standard input; it starts the provider, holds it, and
//! stops its whole tree if delega goes away. See
//! `delega_core::process::KeeperCommand`.

use std::process::ExitCode;

use clap::Parser;

/// The name of the hidden subcommand.
pub const NAME: &str = "process-keeper";

#[derive(Debug, Parser)]
pub struct Args {}

pub fn run(_args: Args) -> ExitCode {
    let code = delega_core::process::keeper_main();
    ExitCode::from(u8::try_from(code).unwrap_or(2))
}
