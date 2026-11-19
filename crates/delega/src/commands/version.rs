//! `delega version`: print the version, or the identity of this build as JSON.

use std::process::ExitCode;

use clap::Parser;
use delega_mcp::BuildInfo;

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

/// The facts of this build that the library cannot know.
pub(crate) fn build_info() -> BuildInfo {
    BuildInfo {
        revision: REVISION.to_string(),
        features: features().into_iter().map(String::from).collect(),
    }
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
        // The identity the MCP server serves as a resource, so the two agree.
        OutputFormat::Json => format!("{}\n", delega_mcp::identity(&build_info())),
    };
    super::output::emit("version", text.as_bytes())
}
