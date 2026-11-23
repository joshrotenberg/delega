//! `delega run`: run one assignment in the foreground.
//!
//! This module holds the command line definition and the order of work. Each
//! step is a module of its own: [`request`] builds the request from the profile
//! and the flags, [`execute`] runs it (with [`signals`]), [`output`] prints what
//! came out and [`log`] is the one writer of standard error. Every failure before a run exists is still
//! printed as a result, so a caller reads one shape whatever happened.

mod execute;
pub(super) mod log;
mod output;
mod request;
pub(super) mod signals;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{ArgGroup, Parser, ValueEnum};
use delega_core::provider::{ProviderOptions, dry_run};

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

/// Options from `docs/api.md`.
#[derive(Debug, Parser)]
#[command(group(ArgGroup::new("mode").args(["read", "write", "build", "full"])))]
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
    /// Allow editing files in the working directory and running any command
    /// inside the provider's sandbox, which lets commands write to the working
    /// directory and temporary directories and gives them no network
    #[arg(long)]
    build: bool,
    /// No sandbox and no scope from delega
    #[arg(long)]
    full: bool,
    /// Refuse the run when the provider cannot enforce the permissions as
    /// stated, instead of running with a note
    #[arg(long)]
    strict: bool,
    /// A shell command the worker may run, matched exactly (repeatable)
    #[arg(long, value_name = "CMD")]
    allow_command: Vec<String>,
    /// An extra directory the worker may read (repeatable)
    #[arg(long, value_name = "DIR")]
    add_dir: Vec<PathBuf>,
    /// Give the worker an environment variable (repeatable). NAME passes this
    /// environment's value; NAME=VALUE sets it. Workers receive a fixed set
    /// only (user, shell, locale, proxy and certificate variables and the
    /// provider's own) plus what is given here. Prefer NAME for secrets: a
    /// value on the command line shows in the process table and shell history
    #[arg(short = 'e', long = "env", value_name = "NAME[=VALUE]")]
    env: Vec<String>,
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
    // The configuration file comes first: the request is built from it, so a
    // file that cannot be used refuses before anything else is looked at.
    let status = super::config::read();
    let file = status.applied.clone();
    let config = match status.into_config() {
        Ok(config) => config,
        Err(refusal) => {
            if args.progress {
                log.line("progress: finished: refused");
            }
            return output::refuse_dry_run(format, &request::flag_facts(args), &refusal, log);
        }
    };
    let request::Built { request, origin } = match request::build(args, &config, file) {
        Ok(built) => built,
        Err(failure) => {
            if args.progress {
                log.line("progress: finished: refused");
            }
            return output::refuse(format, &failure, log);
        }
    };
    let facts = request::Facts::of(&request, origin.clone());
    let mut options = ProviderOptions {
        system_paths: super::system_paths(),
        hook: super::guard_command(),
        origin,
        ..ProviderOptions::default()
    };
    config.apply_to_options(&mut options);
    super::apply_env(&mut options, &args.env);
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
    execute::run(request, options, &facts, log, move |result, stop| {
        output::print_result(format, &result, &writer_log, stop)
    })
}
