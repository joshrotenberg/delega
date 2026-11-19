//! `delega run`: run one assignment in the foreground.
//!
//! This module holds the command line definition and the order of work. Each
//! step is a module of its own: [`request`] builds the request from the profile
//! and the flags, [`execute`] runs it (with [`signals`]), [`output`] prints what
//! came out and [`log`] is the one writer of standard error. Every failure before a run exists is still
//! printed as a result, so a caller reads one shape whatever happened.

mod execute;
mod log;
mod output;
mod request;
mod signals;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{ArgGroup, Parser, ValueEnum};
use delega_core::provider::{HookCommand, ProviderOptions, dry_run};

use super::{OutputFormat, hook};
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

/// Options from `docs/api.md`.
#[derive(Debug, Parser)]
#[command(group(ArgGroup::new("mode").args(["read", "write", "full"])))]
pub struct Args {
    /// A request or profile file; explicit options override it
    #[arg(long, value_name = "FILE")]
    request: Option<PathBuf>,
    /// The provider to run
    #[arg(long, value_enum)]
    provider: Option<Provider>,
    /// The assignment. Written `--prompt=-text` when it starts with a hyphen
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

pub fn run(args: Args) -> ExitCode {
    let log = log::Log::start();
    let code = go(&args, &log);
    // Bounded: a blocked standard error cannot hold the exit.
    log.finish();
    code
}

fn go(args: &Args, log: &log::Log) -> ExitCode {
    let format = args.output.unwrap_or(OutputFormat::Text);
    let request = match request::build(args) {
        Ok(request) => request,
        Err(failure) => {
            if args.progress {
                log.line("progress: finished: refused");
            }
            return output::refuse(format, &failure, log);
        }
    };
    let facts = request::Facts::of(&request);
    let mut options = ProviderOptions {
        hook: guard_command(),
        ..ProviderOptions::default()
    };
    if args.dry_run {
        return match dry_run(&request, &options) {
            Ok(plan) => output::dry_run(format, &plan, log),
            Err(refusal) => output::refuse_dry_run(format, &facts, &refusal, log),
        };
    }
    if args.progress {
        options.progress = Some(log.progress_sink());
    }
    let writer_log = log.clone();
    execute::run(request, options, &facts, log, move |result| {
        output::print_result(format, &result, &writer_log)
    })
}

/// The hook command that runs this executable's command guard, so
/// `--allow-command` can be enforced for Claude Code. Without a usable path to
/// this executable no guard is configured, and the result says that exact
/// command grants were not applied.
fn guard_command() -> Option<HookCommand> {
    let exe = std::env::current_exe().ok()?;
    exe.is_absolute()
        .then(|| HookCommand::new(exe, [hook::NAME]))
}
