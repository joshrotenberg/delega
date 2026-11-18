//! `delega skill list|show NAME`: the embedded skills. Not built yet.

use std::process::ExitCode;

use clap::{Parser, Subcommand};

use super::OutputFormat;

#[derive(Debug, Parser)]
pub struct Args {
    #[command(subcommand)]
    action: Action,
}

#[derive(Debug, Subcommand)]
#[allow(dead_code)]
enum Action {
    /// List the skills built into this executable
    List {
        /// Print as text or as a versioned JSON catalog
        #[arg(long, value_enum, default_value = "text")]
        output: OutputFormat,
    },
    /// Print one skill
    Show {
        /// The skill name
        name: String,
        /// Print as text or as JSON
        #[arg(long, value_enum, default_value = "text")]
        output: OutputFormat,
    },
}

pub fn run(_args: Args) -> ExitCode {
    super::not_implemented("skill")
}
