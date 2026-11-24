//! Run host CLIs in their own sessions, from a neutral directory with no input
//! and bounded output. The process layer stops remaining processes on
//! interrupt, timeout or exit.
//!
//! Without a keeper, a descendant that calls `setsid` and whose parent exits
//! between two listings can escape (see [What remains possible](delega_core::process#what-remains-possible)).

use std::path::Path;
use std::time::Duration;

use delega_core::process::{
    CaptureSpec, Ending, EnvPolicy, Keep, ProcessHandle, ProcessReport, ProcessSpec,
};

use crate::commands::run::signals::Signals;

/// How long a host command that changes registrations may run.
pub const CHANGE_LIMIT: Duration = Duration::from_secs(300);
/// How long a host listing may run.
pub const LISTING_LIMIT: Duration = Duration::from_secs(30);
/// The most output kept from each stream of a change command.
pub const CHANGE_BYTES: usize = 1024 * 1024;
/// The most output kept from each stream of a listing.
pub const LISTING_BYTES: usize = 512 * 1024;

/// Host commands share the signal handlers for the entire plugin operation.
pub struct Runner {
    signals: Signals,
    interrupted: bool,
}

impl Runner {
    pub fn install() -> std::io::Result<Self> {
        Ok(Self {
            signals: Signals::install()?,
            interrupted: false,
        })
    }

    /// Run the host CLI in its own session and stop remaining processes on
    /// interrupt, timeout or exit. The report keeps exit and cleanup separate.
    ///
    /// Without a keeper, a descendant that calls `setsid` and whose parent exits
    /// between two listings can escape (see [What remains possible](delega_core::process#what-remains-possible)).
    pub async fn run(
        &mut self,
        program: &Path,
        args: &[String],
        limit: Duration,
        max: usize,
    ) -> Result<ProcessReport, String> {
        // A signal between host commands also stops the sequence.
        if !self.interrupted {
            self.interrupted = tokio::select! {
                biased;
                _ = self.signals.next() => true,
                () = std::future::ready(()) => false,
            };
        }
        if self.interrupted {
            return Err(format!("{} was interrupted", program.display()));
        }

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

        let handle = ProcessHandle::start(spec);
        let interrupter = handle.interrupter();
        let wait = handle.wait();
        tokio::pin!(wait);
        let report = tokio::select! {
            biased;
            _ = self.signals.next() => {
                self.interrupted = true;
                interrupter.interrupt();
                wait.await
            }
            report = &mut wait => report,
        };
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use super::*;

    fn fake_host(dir: &Path, script: &str) -> PathBuf {
        let path = dir.join("fake-host");
        fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    fn assert_descendant_stopped(pid_file: &Path) {
        let pid = fs::read_to_string(pid_file).expect("fake host recorded its descendant");
        #[allow(clippy::disallowed_methods)]
        let output = {
            let _guard = delega_core::process::spawn_guard();
            std::process::Command::new("/bin/ps")
                .args(["-o", "stat=", "-p", pid.trim()])
                .output()
                .expect("inspect the descendant")
        };
        assert!(output.stderr.is_empty(), "{output:?}");
        let stat = std::str::from_utf8(&output.stdout).unwrap().trim();
        assert!(
            stat.is_empty() || stat.starts_with('Z'),
            "descendant {pid:?} is still running: {stat}"
        );
    }

    #[tokio::test]
    async fn a_host_timeout_stops_its_descendant() {
        let tmp = tempfile::tempdir().unwrap();
        let host = fake_host(
            tmp.path(),
            r#"
            /bin/sleep 30 &
            printf '%s\n' "$!" > "$1"
            wait
        "#,
        );
        let pid_file = tmp.path().join("descendant.pid");
        let limit = Duration::from_secs(3);
        let error = Runner::install()
            .unwrap()
            .run(
                &host,
                &[pid_file.display().to_string()],
                limit,
                CHANGE_BYTES,
            )
            .await
            .expect_err("host command should time out");

        assert!(
            error.ends_with("did not finish within 3 s and was stopped"),
            "{error}"
        );
        assert_descendant_stopped(&pid_file);
    }

    #[tokio::test]
    async fn a_descendant_holding_output_does_not_turn_host_success_into_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let host = fake_host(
            tmp.path(),
            r#"
            /bin/sleep 30 &
            printf '%s\n' "$!" > "$1"
            printf '[]\n'
        "#,
        );
        let pid_file = tmp.path().join("descendant.pid");
        let report = Runner::install()
            .unwrap()
            .run(
                &host,
                &[pid_file.display().to_string()],
                LISTING_LIMIT,
                LISTING_BYTES,
            )
            .await
            .expect("host command should succeed");

        assert_eq!(report.ending, Ending::Exited { code: 0 });
        assert_eq!(report.stdout.text(), "[]\n");
        assert_descendant_stopped(&pid_file);
    }

    #[tokio::test]
    async fn host_output_keeps_only_the_first_bytes_of_each_stream() {
        let tmp = tempfile::tempdir().unwrap();
        let host = fake_host(
            tmp.path(),
            r#"
            printf out
            printf err >&2
            /bin/dd if=/dev/zero bs=1048576 count=2 2>/dev/null
            /bin/dd if=/dev/zero bs=1048576 count=2 >&2 2>/dev/null
            printf end
            printf end >&2
        "#,
        );
        for (limit, max) in [(CHANGE_LIMIT, CHANGE_BYTES), (LISTING_LIMIT, LISTING_BYTES)] {
            let report = Runner::install()
                .unwrap()
                .run(&host, &[], limit, max)
                .await
                .unwrap();
            assert_eq!(report.ending, Ending::Exited { code: 0 });
            for (stream, prefix) in [(&report.stdout, b"out"), (&report.stderr, b"err")] {
                assert_eq!(stream.bytes.len(), max);
                assert_eq!(&stream.bytes[..3], prefix);
                assert!(stream.bytes[3..].iter().all(|byte| *byte == 0));
                assert!(stream.truncated);
                assert_eq!(stream.total_bytes, 2 * 1024 * 1024 + 6);
            }
        }
    }
}
