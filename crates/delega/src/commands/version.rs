//! `delega version`: print the version.

use std::process::ExitCode;

use clap::Parser;
use delega_core::contract::SCHEMA_VERSION;

use super::OutputFormat;

/// The source revision recorded by `build.rs`, or `unknown`.
const REVISION: &str = env!("DELEGA_GIT_REVISION");

/// The Cargo features this binary was built with.
fn features() -> Vec<&'static str> {
    env!("DELEGA_FEATURES")
        .split(',')
        .filter(|f| !f.is_empty())
        .collect()
}

#[derive(Debug, Parser)]
pub struct Args {
    /// Print as text or as JSON
    #[arg(long, value_enum, default_value = "text")]
    output: OutputFormat,
}

pub fn run(args: Args) -> ExitCode {
    let text = match args.output {
        OutputFormat::Text => format!("{}\n", delega_core::VERSION),
        OutputFormat::Json => format!(
            "{}\n",
            serde_json::json!({
                "name": "delega",
                "version": delega_core::VERSION,
                "schema_version": SCHEMA_VERSION,
                "revision": REVISION,
                "features": features(),
            })
        ),
    };
    super::output::emit("version", text.as_bytes())
}
