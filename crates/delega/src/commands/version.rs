//! `delega version`: print the version.

use std::process::ExitCode;

use clap::Parser;

#[derive(Debug, Parser)]
pub struct Args {}

pub fn run(_args: Args) -> ExitCode {
    println!("{}", delega_core::VERSION);
    ExitCode::SUCCESS
}
