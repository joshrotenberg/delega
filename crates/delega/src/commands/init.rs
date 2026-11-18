//! `delega init`: install the plugin and skill into Claude Code, Codex or
//! both. Not built yet.

use std::process::ExitCode;

use clap::Parser;

#[derive(Debug, Parser)]
#[allow(dead_code)]
pub struct Args {
    /// Show what would be done without doing it
    #[arg(long)]
    dry_run: bool,
}

pub fn run(_args: Args) -> ExitCode {
    super::not_implemented("init")
}
