//! `delega skill list|show NAME`: the embedded skills. Not built yet.

use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
pub struct Args {
    #[command(subcommand)]
    action: Action,
}

#[derive(Debug, Subcommand)]
enum Action {
    /// List the skills built into this executable
    List,
    /// Print one skill
    Show {
        /// The skill name
        name: String,
    },
}

pub fn run(args: Args) -> ExitCode {
    match args.action {
        Action::List | Action::Show { .. } => super::not_implemented("skill"),
    }
}
