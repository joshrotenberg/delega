//! `delega version`: print the version.

use std::process::ExitCode;

use clap::Parser;
use delega_core::contract::SCHEMA_VERSION;

use super::OutputFormat;

#[derive(Debug, Parser)]
pub struct Args {
    /// Print as text or as JSON
    #[arg(long, value_enum, default_value = "text")]
    output: OutputFormat,
}

pub fn run(args: Args) -> ExitCode {
    match args.output {
        OutputFormat::Text => println!("{}", delega_core::VERSION),
        OutputFormat::Json => println!(
            "{}",
            serde_json::json!({
                "name": "delega",
                "version": delega_core::VERSION,
                "schema_version": SCHEMA_VERSION,
            })
        ),
    }
    ExitCode::SUCCESS
}
