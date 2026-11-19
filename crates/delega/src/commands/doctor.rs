//! `delega doctor`: report providers, limits and anything that would stop a run.
//!
//! Nothing is written and no worker is started. The one provider process doctor
//! starts is the `--version` probe a run makes, with the same environment and
//! time limit. The report holds names, counts and paths; it never holds an
//! environment value, a deny rule or the content of a file.
//!
//! Exit status: 0 when a run can go ahead (at least one provider is ready and
//! process inspection works), 1 when something would stop a run, 2 when doctor
//! itself cannot run.

use std::process::ExitCode;

use clap::Parser;
use delega_core::provider::ProviderOptions;
use tokio::runtime::Builder;

use super::OutputFormat;

#[derive(Debug, Parser)]
pub struct Args {
    /// Print as text or as JSON
    #[arg(long, value_enum, default_value = "text")]
    output: OutputFormat,
}

/// Exit status when doctor itself cannot run.
const EXIT_UNABLE: u8 = 2;

pub fn run(args: Args) -> ExitCode {
    let runtime = match Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(err) => return unable(&format!("cannot start the async runtime: {err}")),
    };
    let options = ProviderOptions {
        hook: super::guard_command(),
        ..ProviderOptions::default()
    };
    let report = runtime.block_on(delega_core::doctor::diagnose(&options));
    // The version probe has ended; a stuck helper thread must not hold the exit.
    runtime.shutdown_timeout(std::time::Duration::from_secs(5));
    let report = match report {
        Ok(report) => report,
        Err(refusal) => return unable(&refusal.to_string()),
    };
    let text = match args.output {
        OutputFormat::Text => report.to_text(),
        OutputFormat::Json => report.to_json(),
    };
    let written = super::output::emit("doctor", text.as_bytes());
    if written != ExitCode::SUCCESS {
        return written;
    }
    ExitCode::from(report.exit_status())
}

fn unable(message: &str) -> ExitCode {
    super::output::report(&format!("delega doctor: {message}"));
    ExitCode::from(EXIT_UNABLE)
}
