//! `delega mcp`: Serve the MCP tools over stdio. Not built yet.

use std::process::ExitCode;

use clap::Parser;

#[derive(Debug, Parser)]
pub struct Args {}

pub fn run(_args: Args) -> ExitCode {
    super::not_implemented("mcp")
}
