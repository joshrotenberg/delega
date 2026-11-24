//! Cleanup reads the environments of other processes of this user to find the
//! run marker: one `ps -U <uid> -Eww` listing per sweep on macOS,
//! `/proc/<pid>/environ` per candidate on Linux. Those environments can hold
//! secrets of any process, so none of their text may reach the report, its
//! diagnostics or an error.

use std::io::Read;
use std::time::Duration;

use super::fixtures::{Scratch, is_running, sh};
use super::keeper::{SLEEP_ENV, kept};
use crate::process::ProcessHandle;
use crate::test_spawn::Guarded;

/// The variable that carries the probe value in the sibling's environment.
/// Nothing in delega reads it.
const PROBE_ENV: &str = "DELEGA_TEST_ENV_LEAK_PROBE";

/// A value no other process holds: a fixed prefix and 16 random bytes in hex.
fn probe_value() -> String {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .expect("read /dev/urandom");
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("leakprobe{hex}")
}

/// The sibling process, killed and reaped however the test ends.
struct Sibling(std::process::Child);

impl Drop for Sibling {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Start this test binary as a 60 s sleeper with `value` under [`PROBE_ENV`].
/// It is the test's child, outside any run. It is this binary rather than
/// `/bin/sleep` because macOS hides the environment of Apple's own programs
/// from `ps -E`, and the test needs a sibling whose environment cleanup does
/// see.
fn start_sibling(value: &str) -> Sibling {
    let exe = std::env::current_exe().expect("the test binary's path");
    let child = std::process::Command::new(exe)
        .args([
            "--exact",
            "process::tests::keeper::sleeper_entry",
            "--test-threads=1",
            "-q",
        ])
        .env_clear()
        .env(SLEEP_ENV, "60")
        .env(PROBE_ENV, value)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .guarded_spawn()
        .expect("start the sibling");
    Sibling(child)
}

/// Whether the environment of `pid`, read the way cleanup reads it, holds
/// `value`. On macOS that is the same `ps -U <uid> -Eww` listing cleanup takes;
/// on Linux it is `/proc/<pid>/environ`.
#[cfg(not(target_os = "linux"))]
fn environment_shows(_pid: u32, value: &str) -> bool {
    let uid = rustix::process::getuid().as_raw().to_string();
    std::process::Command::new("/bin/ps")
        .args(["-U", &uid, "-Eww", "-o", "pid=,stat=,lstart=,command="])
        .env("LC_ALL", "C")
        .guarded_output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(value))
}

#[cfg(target_os = "linux")]
fn environment_shows(pid: u32, value: &str) -> bool {
    std::fs::read(format!("/proc/{pid}/environ")).is_ok_and(|block| {
        block
            .split(|b| *b == 0)
            .any(|entry| entry == format!("{PROBE_ENV}={value}").as_bytes())
    })
}

/// Wait until the sibling's environment is readable with the value, so the
/// process has finished starting before the run begins.
async fn await_environment(pid: u32, value: &str) {
    let value = value.to_owned();
    super::fixtures::await_value("the sibling's environment showing the probe value", || {
        if environment_shows(pid, &value) {
            Ok(())
        } else {
            Err("not yet".to_owned())
        }
    })
    .await;
}

/// A kept run's cleanup reads the marker from the environments of every
/// process of this user while a sibling process holds a random value in its
/// environment. Neither the report (with every diagnostic, the keeper's tree
/// reports and the inspection's last error), nor the warnings, may contain
/// that value. The test checks that the sibling was alive across the run, that
/// cleanup took at least one marker reading, and that the value is readable
/// where cleanup reads (`ps -Eww` on macOS, `/proc` on Linux), so a pass means
/// delega dropped the value rather than never saw it.
#[tokio::test]
async fn environment_text_read_by_cleanup_never_reaches_the_report_or_its_diagnostics() {
    let value = probe_value();
    let sibling = start_sibling(&value);
    let pid = sibling.0.id();
    await_environment(pid, &value).await;
    assert!(is_running(pid as i32), "the sibling is not running");

    let s = Scratch::new();
    let report = ProcessHandle::start(kept(sh(&s, "exit 0", Duration::from_secs(60))))
        .wait()
        .await;

    assert!(
        is_running(pid as i32),
        "the sibling ended during the run, so cleanup may not have read it"
    );
    assert!(
        environment_shows(pid, &value),
        "the sibling's environment no longer shows the probe value"
    );
    let stats = &report.diagnostics.inspection;
    assert!(
        stats.marker_readings > 0,
        "cleanup took no marker reading, so it never read the sibling's environment"
    );
    let keeper = report
        .diagnostics
        .keeper
        .as_ref()
        .expect("a kept run reports its keeper");

    let texts = [
        ("the report", format!("{report:?}")),
        ("the pretty report", format!("{report:#?}")),
        ("the diagnostics", format!("{:?}", report.diagnostics)),
        ("the warnings", report.diagnostics.warnings.join("\n")),
        (
            "the inspection error",
            stats.last_error.clone().unwrap_or_default(),
        ),
        ("the keeper diagnostics", format!("{keeper:?}")),
        ("the keeper tree reports", keeper.tree_reports.join("\n")),
    ];
    for (what, text) in texts {
        assert!(
            !text.contains(&value),
            "{what} contains the sibling's environment value ({} bytes)",
            value.len()
        );
    }
    drop(sibling);
}
