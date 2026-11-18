//! The `delega` command line.
//!
//! This file only parses arguments and hands off. Each subcommand lives in its
//! own module under `commands/`.

mod commands;
mod duration;

use std::process::ExitCode;

use clap::Parser;

use commands::Command;

/// Run Claude Code and Codex agents from a program.
#[derive(Debug, Parser)]
#[command(name = "delega", version = delega_core::VERSION, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

fn main() -> ExitCode {
    // clap exits with status 2 on a usage error, as docs/api.md requires.
    let cli = Cli::parse();
    cli.command.run()
}
