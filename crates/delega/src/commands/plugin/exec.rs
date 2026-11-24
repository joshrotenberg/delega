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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use delega_core::contract::result::Cleanup;

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
            let _guard = process::spawn_guard();
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
        let error = run(
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
        let report = run(
            &host,
            &[pid_file.display().to_string()],
            LISTING_LIMIT,
            LISTING_BYTES,
        )
        .await
        .expect("host command should succeed");

        assert_eq!(report.ending, Ending::Exited { code: 0 });
        assert_eq!(report.stdout.text(), "[]\n");
        assert_eq!(report.cleanup, Cleanup::Confirmed);
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
            let report = run(&host, &[], limit, max).await.unwrap();
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
