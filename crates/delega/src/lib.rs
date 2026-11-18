//! The `delega` command line.
//!
//! `main.rs` calls [`main`]; tests read the command definition through
//! [`command`]. This file only parses arguments and hands off. Each subcommand lives in its
//! own module under `commands/`.

mod commands;
mod duration;

use std::process::ExitCode;

use clap::{CommandFactory, Parser};

use commands::Command;

/// Run Claude Code and Codex agents from a program.
#[derive(Debug, Parser)]
#[command(name = "delega", version = delega_core::VERSION, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// The clap definition of the whole command line, for tools and tests that
/// need to know which subcommands and options exist.
pub fn command() -> clap::Command {
    Cli::command()
}

/// Parse the process arguments, run the command and return its exit status.
pub fn main() -> ExitCode {
    // clap exits with status 2 on a usage error, as docs/api.md requires.
    let cli = Cli::parse();
    cli.command.run()
}
