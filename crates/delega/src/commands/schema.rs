//! `delega schema request|result`: print a contract schema.

use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use delega_core::contract::schema::{REQUEST_SCHEMA, RESULT_SCHEMA};

/// Which contract to print.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Which {
    /// The request schema
    Request,
    /// The result schema
    Result,
}

#[derive(Debug, Parser)]
pub struct Args {
    /// The contract to print
    which: Which,
}

pub fn run(args: Args) -> ExitCode {
    let text = match args.which {
        Which::Request => REQUEST_SCHEMA,
        Which::Result => RESULT_SCHEMA,
    };
    super::output::emit("schema", text.as_bytes())
}
