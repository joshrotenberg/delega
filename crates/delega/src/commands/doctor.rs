//! `delega doctor`: report providers, limits and anything that would stop a run.
//!
//! Nothing is written and no worker is started. The one provider process doctor
//! starts is the `--version` probe a run makes, with the same environment and
//! time limit. The report holds names, counts and paths; it never holds an
//! environment value, a deny rule or the content of a file. Managed settings
//! found for a provider are listed as a notice and change no verdict.
//!
//! Exit status: 0 when a run can go ahead (at least one provider is ready and
//! process inspection works), 1 when something would stop a run, 2 when doctor
//! itself cannot run.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use delega_core::provider::ProviderOptions;
use delega_core::provider::managed::SystemPaths;
use tokio::runtime::Builder;

use super::OutputFormat;

#[derive(Debug, Parser)]
pub struct Args {
    /// Print as text or as JSON
    #[arg(long, value_enum, default_value = "text")]
    output: OutputFormat,
}

/// Test-only, honoured in builds with debug assertions only: the directory the
/// system locations of managed settings are looked for under, in place of `/`.
const SYSTEM_ROOT_ENV: &str = "DELEGA_TEST_SYSTEM_ROOT";

/// Exit status when doctor itself cannot run.
const EXIT_UNABLE: u8 = 2;

pub fn run(args: Args) -> ExitCode {
    let runtime = match Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(err) => return unable(&format!("cannot start the async runtime: {err}")),
    };
    let mut options = ProviderOptions {
        hook: super::guard_command(),
        ..ProviderOptions::default()
    };
    // A configuration file that is in effect shapes what a run would meet, so
    // its environment and executables are applied here too. One that is refused
    // is reported as a problem and nothing from it is applied: the rest of the
    // report is what a run would meet without it.
    let status = super::config::read();
    if let Some(config) = status.config() {
        config.apply_to_options(&mut options);
    }
    let report = runtime.block_on(delega_core::doctor::diagnose_at(
        &options,
        &status,
        &system_paths(),
    ));
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

/// Where managed settings are looked for: the real system locations. In a
/// build with debug assertions, which is what the tests run, the test-only
/// variable `DELEGA_TEST_SYSTEM_ROOT` naming an absolute path moves them under
/// that directory. A release build ignores the variable, so one inherited from
/// the environment cannot hide what is on the system.
fn system_paths() -> SystemPaths {
    if cfg!(debug_assertions) {
        match std::env::var_os(SYSTEM_ROOT_ENV).map(PathBuf::from) {
            Some(root) if root.is_absolute() => return SystemPaths::under(root),
            _ => {}
        }
    }
    SystemPaths::default()
}

fn unable(message: &str) -> ExitCode {
    super::output::report(&delega_core::doctor::sanitize(&format!(
        "delega doctor: {message}"
    )));
    ExitCode::from(EXIT_UNABLE)
}
