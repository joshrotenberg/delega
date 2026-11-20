//! One module per subcommand. Adding a subcommand adds a file here and a line
//! in [`Command`].

mod config;
mod doctor;
mod hook;
mod mcp;
pub(crate) mod output;
mod plugin;
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
    /// Manage the delega plugin for Claude Code and Codex
    #[command(subcommand)]
    Plugin(plugin::Command),
    /// Print the request or result schema built into this executable
    Schema(schema::Args),
    /// List or print the skills built into this executable
    Skill(skill::Args),
    /// Report providers found, their versions and anything that would stop a run
    Doctor(doctor::Args),
    /// Print the version
    Version(version::Args),
    /// The command guard Claude Code calls before a Bash tool call
    #[command(name = hook::NAME, hide = true)]
    Guard(hook::Args),
}

impl Command {
    /// Run the subcommand and return the process exit status.
    pub fn run(self) -> ExitCode {
        match self {
            Command::Run(a) => run::run(*a),
            Command::Mcp(a) => mcp::run(a),
            Command::Plugin(command) => command.run(),
            Command::Schema(a) => schema::run(a),
            Command::Skill(a) => skill::run(a),
            Command::Doctor(a) => doctor::run(a),
            Command::Version(a) => version::run(a),
            Command::Guard(a) => hook::run(a),
        }
    }
}

/// The hook command that runs this executable's command guard, so
/// `--allow-command` can be enforced for Claude Code. Without a usable path to
/// this executable no guard is configured, and the result says that exact
/// command grants were not applied.
pub(crate) fn guard_command() -> Option<delega_core::provider::HookCommand> {
    let exe = std::env::current_exe().ok()?;
    exe.is_absolute()
        .then(|| delega_core::provider::HookCommand::new(exe, [hook::NAME]))
}

/// Add the `--env NAME` and `--env NAME=VALUE` arguments to the provider
/// options, for `delega run` and `delega mcp` alike. Nothing is checked here: a
/// name that is not valid, that describes the calling session or that delega
/// owns is refused by the provider layer when a worker starts, with error kind
/// `invalid_env_name` or `forbidden_env_name`, and the message never repeats a
/// value.
pub(crate) fn apply_env(options: &mut delega_core::provider::ProviderOptions, given: &[String]) {
    for given in given {
        match given.split_once('=') {
            Some((name, value)) => options.env_set.push((name.to_string(), value.to_string())),
            None => options.env_pass.push(given.clone()),
        }
    }
}
