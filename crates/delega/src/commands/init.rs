//! `delega init`: Install into Claude Code, Codex or both. Not built yet.

use std::process::ExitCode;

use clap::Parser;

#[derive(Debug, Parser)]
pub struct Args {
    /// Install into Claude Code only
    #[arg(long)]
    claude: bool,
    /// Install into Codex only
    #[arg(long)]
    codex: bool,
    /// Show what would be done without doing it
    #[arg(long)]
    dry_run: bool,
}

pub fn run(_args: Args) -> ExitCode {
    super::not_implemented("init")
}
