//! Shared helpers: scratch directory, fake children, process checks.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::watch;

use crate::process::sys::{self, Signal};
use crate::process::tree::{Lister, list_processes};
use crate::process::{EnvPolicy, ProcessHandle, ProcessReport, ProcessSpec};
use crate::test_spawn::Guarded;

/// How long a fixture waits for an event it does not control (a pid file, a
/// marker, a process state). The wait costs nothing when the event arrives, so
/// it is far longer than any load makes it take; it only ends a test whose
/// event never comes, with a message that says what was awaited.
pub(super) const AWAIT: Duration = Duration::from_secs(90);

/// Poll `ready` every 10 ms until it holds, for at most [`AWAIT`]. `what` names
/// the event for the failure message.
pub(super) async fn await_event(what: &str, mut ready: impl FnMut() -> bool) {
    let began = Instant::now();
    while !ready() {
        assert!(
            began.elapsed() < AWAIT,
            "waited {AWAIT:?} and never saw: {what}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

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
        let read = || {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| text.trim().parse::<i32>().ok())
        };
        await_event(&format!("the script writing a pid to {name}"), || {
            read().is_some()
        })
        .await;
        read().expect("the pid file was just read")
    }

    /// Wait until a file exists and is not empty.
    pub(super) async fn exists(&self, name: &str) {
        let path = self.path(name);
        await_event(&format!("the script writing {name}"), || {
            std::fs::metadata(&path).is_ok_and(|m| m.len() > 0)
        })
        .await;
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

/// A shell script as a child, in the scratch directory, and only `PATH`
/// inherited. The grace, the drain bound and the cleanup deadline are long
/// compared with anything a loaded machine adds, so a run does not end by one
/// of them unless the test makes it (a test about a child that ignores TERM
/// pays the grace once, one second).
pub(super) fn sh(scratch: &Scratch, script: &str, limit: Duration) -> ProcessSpec {
    let mut spec = ProcessSpec::new("/bin/sh", limit)
        .args(["-c", script])
        .cwd(scratch.dir())
        .env(EnvPolicy::inherit_only(["PATH"]));
    spec.stop_grace = Duration::from_secs(1);
    spec.drain_bound = Duration::from_secs(10);
    spec.cleanup_deadline = Duration::from_secs(120);
    spec
}

/// Whether a process is alive. A zombie counts as gone: it has run to its end
/// and only waits for its parent to collect it.
pub(super) fn is_running(pid: i32) -> bool {
    let out = std::process::Command::new("/bin/ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .guarded_output()
        .expect("run ps");
    let stat = String::from_utf8_lossy(&out.stdout);
    let stat = stat.trim();
    !stat.is_empty() && !stat.starts_with('Z')
}

/// [`is_running`] on a thread of its own. A `ps` run blocks, and under load it
/// takes seconds; run on the runtime's thread it would also keep the owner task
/// under test from running for just as long.
pub(super) async fn is_running_async(pid: i32) -> bool {
    tokio::task::spawn_blocking(move || is_running(pid))
        .await
        .expect("the ps task ended")
}

/// Wait for every pid to be gone (see [`AWAIT`]), then assert it.
pub(super) async fn assert_gone(what: &str, pids: &[i32]) {
    let began = Instant::now();
    loop {
        let mut alive = Vec::new();
        for pid in pids {
            if is_running_async(*pid).await {
                alive.push(*pid);
            }
        }
        if alive.is_empty() {
            return;
        }
        assert!(
            began.elapsed() < AWAIT,
            "{what}: still running after {AWAIT:?}: {alive:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

pub(super) fn assert_confirmed(report: &ProcessReport) {
    assert_eq!(
        report.cleanup,
        crate::contract::result::Cleanup::Confirmed,
        "{report:#?}"
    );
}

/// A background-able command: `sleep 60` in a process group of its own, in the
/// session of whoever starts it.
pub(super) const OWN_GROUP: &str = "perl -e 'use POSIX; setpgid(0,0); exec q(sleep), q(60)'";

/// `sleep 60` in a session of its own, beyond what the session sweep reaches.
pub(super) const OWN_SESSION: &str = "perl -e 'use POSIX; POSIX::setsid(); exec q(sleep), q(60)'";

pub(super) fn perl_available() -> bool {
    std::process::Command::new("perl")
        .arg("-e")
        .arg("1")
        .guarded_output()
        .is_ok_and(|out| out.status.success())
}

/// Start a run, wait until the scripts have written the named pid files, guard
/// those pids, then wait for the report. The guard must be kept by the test:
/// it stops leftovers only if an assertion fails afterwards.
pub(super) async fn run_guarded(
    scratch: &Scratch,
    spec: ProcessSpec,
    files: &[&str],
) -> (ProcessReport, Vec<i32>, Reaper) {
    let handle = ProcessHandle::start(spec);
    let mut pids = Vec::new();
    for file in files {
        pids.push(scratch.pid(file).await);
    }
    let reaper = Reaper(pids.clone());
    (handle.wait().await, pids, reaper)
}

/// Shell: after starting a background command with `&`, wait until it has
/// become the leader of its own group (so it has run `setsid` or `setpgid`).
pub(super) const WAIT_ESCAPED: &str =
    "until [ \"$(ps -o pgid= -p $! | tr -d ' ')\" = \"$!\" ]; do sleep 0.02; done;";

/// Wait until a process leads its own group, for tests that must not race the
/// descendant's own `setsid`.
pub(super) async fn wait_own_group(pid: i32) {
    let began = Instant::now();
    loop {
        let own = tokio::task::spawn_blocking(move || {
            let out = std::process::Command::new("/bin/ps")
                .args(["-o", "pgid=", "-p", &pid.to_string()])
                .guarded_output()
                .expect("run ps");
            String::from_utf8_lossy(&out.stdout).trim() == pid.to_string()
        })
        .await
        .expect("the ps task ended");
        if own {
            return;
        }
        assert!(
            began.elapsed() < AWAIT,
            "waited {AWAIT:?} and never saw: process {pid} becoming a group leader"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Wait until the periodic scan has recorded `pid` as a tracked descendant.
pub(super) async fn wait_tracked(handle: &ProcessHandle, pid: i32) {
    await_event(&format!("the scan tracking {pid}"), || {
        handle.tracked_pids().contains(&(pid as u32))
    })
    .await;
}

/// A lister that, while armed, does not answer until the gate is opened, and
/// then reads the real table. It counts the calls that had to wait.
#[derive(Clone)]
pub(super) struct Held {
    pub(super) armed: Arc<AtomicBool>,
    pub(super) waiting: Arc<AtomicU32>,
    open: watch::Sender<bool>,
}

impl Held {
    pub(super) fn new(armed: bool) -> Self {
        Self {
            armed: Arc::new(AtomicBool::new(armed)),
            waiting: Arc::new(AtomicU32::new(0)),
            open: watch::channel(false).0,
        }
    }

    pub(super) fn lister(&self) -> Lister {
        let held = self.clone();
        Arc::new(move || {
            let armed = held.armed.load(Ordering::SeqCst);
            let waiting = held.waiting.clone();
            let mut gate = held.open.subscribe();
            Box::pin(async move {
                if armed {
                    waiting.fetch_add(1, Ordering::SeqCst);
                    let _ = gate.wait_for(|open| *open).await;
                }
                list_processes().await
            })
        })
    }

    pub(super) fn release(&self) {
        self.open.send_replace(true);
    }
}
