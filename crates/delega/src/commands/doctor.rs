//! `delega doctor`: Report providers and limits. Not built yet.

use std::process::ExitCode;

use clap::Parser;

#[derive(Debug, Parser)]
pub struct Args {}

pub fn run(_args: Args) -> ExitCode {
    super::not_implemented("doctor")
}
