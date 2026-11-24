//! Running a host CLI: from a neutral working directory, with no input, a
//! time limit and a bound on the output kept.

use std::path::Path;
use std::time::Duration;

use delega_core::process::{
    self, CaptureSpec, Ending, EnvPolicy, Keep, ProcessReport, ProcessSpec,
};

/// How long a host command that changes registrations may run.
pub const CHANGE_LIMIT: Duration = Duration::from_secs(300);
/// How long a host listing may run.
pub const LISTING_LIMIT: Duration = Duration::from_secs(30);
/// The most output kept from each stream of a change command.
pub const CHANGE_BYTES: usize = 1024 * 1024;
/// The most output kept from each stream of a listing.
pub const LISTING_BYTES: usize = 512 * 1024;

/// Run the host CLI in its own session. The report keeps the exit and cleanup
/// outcomes separate, including when descendants held the output open.
pub async fn run(
    program: &Path,
    args: &[String],
    limit: Duration,
    max: usize,
) -> Result<ProcessReport, String> {
    let mut spec = ProcessSpec::new(program, limit)
        .args(args)
        .cwd("/")
        .env(EnvPolicy::inherit_all().set("PWD", "/"));
    let capture = CaptureSpec {
        limit: max,
        keep: Keep::Head,
    };
    spec.stdout = capture;
    spec.stderr = capture;

    let report = process::run(spec).await;
    match &report.ending {
        Ending::Exited { .. } | Ending::Signaled { .. } => Ok(report),
        Ending::FailedToStart { message } => {
            let prefix = format!("could not start {}: ", program.display());
            let detail = message.strip_prefix(&prefix).unwrap_or(message);
            Err(format!("cannot start {}: {detail}", program.display()))
        }
        Ending::WaitFailed { message } => {
            let detail = message.strip_prefix("wait failed: ").unwrap_or(message);
            Err(format!("cannot wait for {}: {detail}", program.display()))
        }
        Ending::TimedOut { limit } => Err(format!(
            "{} did not finish within {} s and was stopped",
            program.display(),
            limit.as_secs()
        )),
        Ending::Interrupted => Err(format!("{} was interrupted", program.display())),
    }
}

/// Both streams, trimmed, for showing to the user.
pub fn shown(report: &ProcessReport) -> String {
    let stdout = report.stdout.text();
    let stderr = report.stderr.text();
    [stdout.trim(), stderr.trim()]
        .into_iter()
        .filter(|stream| !stream.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}
