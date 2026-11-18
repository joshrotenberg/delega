//! `delega run`: run one assignment in the foreground. Not built yet.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{ArgGroup, Parser, ValueEnum};

use super::OutputFormat;
use crate::duration::parse_duration_secs;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Provider {
    Claude,
    Codex,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Effort {
    Low,
    Medium,
    High,
}

/// Options from `docs/api.md`. They are parsed and checked now so that the
/// help text shows the planned surface; nothing uses them yet.
#[derive(Debug, Parser)]
#[command(group(ArgGroup::new("mode").args(["read", "write", "full"])))]
#[allow(dead_code)]
pub struct Args {
    /// A request or profile file; explicit options override it
    #[arg(long, value_name = "FILE")]
    request: Option<PathBuf>,
    /// The provider to run
    #[arg(long, value_enum)]
    provider: Option<Provider>,
    /// The assignment
    #[arg(short, long)]
    prompt: Option<String>,
    /// Read the assignment from stdin
    #[arg(long)]
    stdin: bool,
    /// Working directory of the worker
    #[arg(short = 'C', long, value_name = "DIR")]
    cwd: Option<PathBuf>,
    /// A label for the worker
    #[arg(long)]
    name: Option<String>,
    /// Provider model name or alias
    #[arg(long)]
    model: Option<String>,
    /// Reasoning effort
    #[arg(long, value_enum)]
    effort: Option<Effort>,
    /// Time limit for the run: 90s, 20m, 2h, or a plain number of seconds
    #[arg(long, value_name = "DURATION", value_parser = parse_duration_secs)]
    timeout: Option<u64>,
    /// Read-only permissions (the default)
    #[arg(long)]
    read: bool,
    /// Allow editing files in the working directory
    #[arg(long)]
    write: bool,
    /// No restriction from delega
    #[arg(long)]
    full: bool,
    /// A shell command the worker may run, matched exactly (repeatable)
    #[arg(long, value_name = "CMD")]
    allow_command: Vec<String>,
    /// An extra directory the worker may read (repeatable)
    #[arg(long, value_name = "DIR")]
    add_dir: Vec<PathBuf>,
    /// A JSON Schema file the answer must satisfy
    #[arg(long, value_name = "FILE")]
    schema: Option<PathBuf>,
    /// Continue a saved provider session
    #[arg(long, value_name = "ID", conflicts_with = "ephemeral")]
    resume: Option<String>,
    /// Do not keep provider history
    #[arg(long)]
    ephemeral: bool,
    /// Print the answer as text or the whole result as JSON
    #[arg(long, value_enum)]
    output: Option<OutputFormat>,
    /// Validate and show what would run, without starting anything
    #[arg(long)]
    dry_run: bool,
    /// Write one line per provider event to stderr
    #[arg(long)]
    progress: bool,
    /// Provider options, passed after `--`
    #[arg(last = true)]
    native_args: Vec<String>,
}

pub fn run(_args: Args) -> ExitCode {
    super::not_implemented("run")
}
