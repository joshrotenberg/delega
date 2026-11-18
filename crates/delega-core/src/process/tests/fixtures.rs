//! Shared helpers: scratch directory, fake children, process checks.

use std::path::PathBuf;
use std::time::Duration;

use crate::process::sys::{self, Signal};
use crate::process::{EnvPolicy, ProcessReport, ProcessSpec};

/// A private directory per test for pid files and markers.
pub(super) struct Scratch {
    dir: tempfile::TempDir,
}

impl Scratch {
    pub(super) fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("create scratch directory"),
        }
    }

    pub(super) fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    pub(super) fn dir(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    /// Wait until a script has written a pid to the named file.
    pub(super) async fn pid(&self, name: &str) -> i32 {
        let path = self.path(name);
        for _ in 0..1000 {
            if let Some(pid) = std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| text.trim().parse::<i32>().ok())
            {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the script never wrote {name}");
    }

    /// The pid in a file that must already exist.
    pub(super) fn pid_now(&self, name: &str) -> i32 {
        std::fs::read_to_string(self.path(name))
            .expect("pid file exists")
            .trim()
            .parse()
            .expect("pid file holds a pid")
    }
}

/// Stops leftover processes when a test ends, even when it fails, so a failed
/// assertion does not leave strays on the machine.
pub(super) struct Reaper(pub(super) Vec<i32>);

impl Drop for Reaper {
    fn drop(&mut self) {
        for pid in &self.0 {
            let _ = sys::signal_process(*pid, Signal::KILL);
        }
    }
}

/// A shell script as a child, in the scratch directory, with a short grace
/// and drain so stop paths run quickly, and only `PATH` inherited.
pub(super) fn sh(scratch: &Scratch, script: &str, limit: Duration) -> ProcessSpec {
    let mut spec = ProcessSpec::new("/bin/sh", limit)
        .args(["-c", script])
        .cwd(scratch.dir())
        .env(EnvPolicy::inherit_only(["PATH"]));
    spec.stop_grace = Duration::from_millis(200);
    spec.drain_bound = Duration::from_millis(500);
    spec.cleanup_deadline = Duration::from_secs(8);
    spec
}

/// Whether a process is alive. A zombie counts as gone: it has run to its end
/// and only waits for its parent to collect it.
pub(super) fn is_running(pid: i32) -> bool {
    let out = std::process::Command::new("/bin/ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .expect("run ps");
    let stat = String::from_utf8_lossy(&out.stdout);
    let stat = stat.trim();
    !stat.is_empty() && !stat.starts_with('Z')
}

/// Wait up to five seconds for every pid to be gone, then assert it.
pub(super) async fn assert_gone(what: &str, pids: &[i32]) {
    for _ in 0..500 {
        if pids.iter().all(|pid| !is_running(*pid)) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let alive: Vec<_> = pids.iter().filter(|pid| is_running(**pid)).collect();
    panic!("{what}: still running: {alive:?}");
}

pub(super) fn assert_confirmed(report: &ProcessReport) {
    assert_eq!(
        report.cleanup,
        crate::contract::result::Cleanup::Confirmed,
        "{report:#?}"
    );
}
