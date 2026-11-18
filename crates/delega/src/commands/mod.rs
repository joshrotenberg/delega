//! One module per subcommand. Adding a subcommand adds a file here and a line
//! in [`Command`].

mod doctor;
mod init;
mod mcp;
mod run;
mod schema;
mod skill;
mod version;

use std::process::ExitCode;

use clap::Subcommand;

/// Exit status for a refused run and for usage errors, from `docs/api.md`.
pub const EXIT_REFUSED: u8 = delega_core::contract::outcome::Outcome::Refused.exit_status();

/// How a command prints its result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    /// Plain text
    Text,
    /// JSON
    Json,
}

/// The planned command surface from `docs/api.md`.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run one assignment and print the answer, or the result as JSON
    Run(Box<run::Args>),
    /// Serve the MCP tools over stdio
    Mcp(mcp::Args),
    /// Install the plugin and skill into Claude Code, Codex or both
    Init(init::Args),
    /// Print the request or result schema built into this executable
    Schema(schema::Args),
    /// List or print the skills built into this executable
    Skill(skill::Args),
    /// Report providers found, their versions and anything that would stop a run
    Doctor(doctor::Args),
    /// Print the version
    Version(version::Args),
}

impl Command {
    /// Run the subcommand and return the process exit status.
    pub fn run(self) -> ExitCode {
        match self {
            Command::Run(a) => run::run(*a),
            Command::Mcp(a) => mcp::run(a),
            Command::Init(a) => init::run(a),
            Command::Schema(a) => schema::run(a),
            Command::Skill(a) => skill::run(a),
            Command::Doctor(a) => doctor::run(a),
            Command::Version(a) => version::run(a),
        }
    }
}

/// Report that a planned subcommand is not built yet.
pub(super) fn not_implemented(name: &str) -> ExitCode {
    eprintln!("delega {name}: not implemented yet");
    ExitCode::from(EXIT_REFUSED)
}
