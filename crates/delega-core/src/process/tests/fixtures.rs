//! Shared helpers: scratch directory, fake children, process checks.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant, SystemTime};

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
        self.pid_file(name).await.pid
    }

    pub(super) async fn pid_file(&self, name: &str) -> PidFile {
        let path = self.path(name);
        let mut record = None;
        await_event(&format!("the script writing a pid to {name}"), || {
            record = PidFile::read(&path);
            record.is_some()
        })
        .await;
        record.expect("the pid file was just read")
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

/// Read the PID and timestamp from the same open file. A rewrite during
/// the read invalidates the sample, so a newer mtime cannot vouch for an old PID.
pub(super) struct PidFile {
    pub(super) pid: i32,
    modified: Option<SystemTime>,
}

impl PidFile {
    fn read(path: &Path) -> Option<Self> {
        let mut file = std::fs::File::open(path).ok()?;
        let modified = file.metadata().and_then(|meta| meta.modified()).ok();
        let mut text = String::new();
        file.read_to_string(&mut text).ok()?;
        if modified != file.metadata().and_then(|meta| meta.modified()).ok() {
            return None;
        }
        let pid = text.trim().parse().ok()?;
        Some(Self { pid, modified })
    }
}

/// The process observed at registration. A pidfd pins the identity on Linux.
pub(super) struct ProcessIdentity {
    #[cfg(target_os = "linux")]
    fd: std::os::fd::OwnedFd,
    #[cfg(not(target_os = "linux"))]
    pid: i32,
    #[cfg(not(target_os = "linux"))]
    started: String,
}

impl ProcessIdentity {
    pub(super) fn capture(pid: i32) -> Option<Self> {
        if pid <= 1 {
            return None;
        }
        #[cfg(target_os = "linux")]
        {
            use rustix::process::{Pid, PidfdFlags, pidfd_open};
            let fd = pidfd_open(Pid::from_raw(pid)?, PidfdFlags::empty()).ok()?;
            Some(Self { fd })
        }
        #[cfg(not(target_os = "linux"))]
        {
            Some(Self {
                pid,
                started: process_start(pid)?,
            })
        }
    }

    fn from_pid_file(record: &PidFile) -> Option<Self> {
        let modified = record.modified?;
        // On Linux, open the pidfd before looking up the start ticks.
        let identity = Self::capture(record.pid)?;
        #[cfg(target_os = "linux")]
        let matches = {
            let ticks = sys::start_ticks(record.pid)?;
            started_before_file(ticks, modified)? && !sys::pidfd_exited(&identity.fd)
        };
        #[cfg(not(target_os = "linux"))]
        let matches = start_seconds(&identity.started)?
            <= modified
                .duration_since(SystemTime::UNIX_EPOCH)
                .ok()?
                .as_secs();
        matches.then_some(identity)
    }

    pub(super) fn signal(&self, signal: Signal) -> bool {
        #[cfg(target_os = "linux")]
        {
            rustix::process::pidfd_send_signal(&self.fd, signal).is_ok()
        }
        #[cfg(not(target_os = "linux"))]
        {
            // ps has one-second precision, and the process can exit between
            // the check and kill. macOS has no pidfd to close that window.
            process_start(self.pid).as_ref() == Some(&self.started)
                && sys::signal_process(self.pid, signal).is_ok()
        }
    }
}

#[cfg(target_os = "linux")]
fn started_before_file(ticks: u64, modified: SystemTime) -> Option<bool> {
    use rustix::param::clock_ticks_per_second;
    use rustix::time::{ClockId, clock_gettime};

    // Translate the wall-clock mtime to time since boot. Reading boottime
    // first makes the elapsed time at the write a lower bound. Require the
    // entire start tick to precede that bound, since /proc rounds it down.
    let boot_now = clock_gettime(ClockId::Boottime);
    let age = SystemTime::now().duration_since(modified).ok()?;
    let boot_now = Duration::new(
        boot_now.tv_sec.try_into().ok()?,
        boot_now.tv_nsec.try_into().ok()?,
    );
    let file_ticks = boot_now.checked_sub(age)?.as_nanos() * u128::from(clock_ticks_per_second())
        / 1_000_000_000;
    Some(u128::from(ticks) < file_ticks)
}

#[cfg(not(target_os = "linux"))]
fn start_seconds(started: &str) -> Option<u64> {
    let output = std::process::Command::new("/bin/date")
        .args(["-j", "-u", "-f", "%a %b %e %T %Y", started, "+%s"])
        .env_clear()
        .env("LC_ALL", "C")
        .guarded_output()
        .ok()?;
    output.status.success().then_some(())?;
    std::str::from_utf8(&output.stdout)
        .ok()?
        .trim()
        .parse()
        .ok()
}

#[cfg(not(target_os = "linux"))]
fn process_start(pid: i32) -> Option<String> {
    let output = std::process::Command::new("/bin/ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .env_clear()
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .guarded_output()
        .ok()?;
    let started = std::str::from_utf8(&output.stdout).ok()?.trim();
    (output.status.success() && !started.is_empty()).then(|| started.to_owned())
}

/// Stops leftover processes when a test ends, even when it fails.
#[derive(Default)]
pub(super) struct Reaper(Vec<ProcessIdentity>);

impl Reaper {
    pub(super) fn new(pids: impl IntoIterator<Item = i32>) -> Self {
        Self(
            pids.into_iter()
                .filter_map(ProcessIdentity::capture)
                .collect(),
        )
    }

    pub(super) async fn watch(&mut self, scratch: &Scratch, name: &str) -> i32 {
        let record = scratch.pid_file(name).await;
        self.register(&record);
        record.pid
    }

    pub(super) fn register(&mut self, record: &PidFile) -> Option<&ProcessIdentity> {
        self.0.push(ProcessIdentity::from_pid_file(record)?);
        self.0.last()
    }

    pub(super) fn add(&mut self, identity: Option<ProcessIdentity>) {
        self.0.extend(identity);
    }
}

impl Drop for Reaper {
    fn drop(&mut self) {
        for process in &self.0 {
            process.signal(Signal::KILL);
        }
    }
}

tokio::task_local! {
    /// Whether the test runs its children through the test keeper; set by
    /// `both!`, read by [`sh`].
    pub(super) static KEEP: bool;
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
    if KEEP.try_with(|keep| *keep).unwrap_or(false) {
        spec.keeper = Some(super::keeper::test_keeper());
    }
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

/// Whether the run marker shows for Apple's own programs here: `ps -E` shows
/// the environment of a `/bin/sleep` started with a marker value. macOS hides
/// it under System Integrity Protection; a machine without it shows it, and
/// Linux reads `/proc` instead. Probed once.
pub(super) fn marker_visible_for_system_programs() -> bool {
    static VISIBLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *VISIBLE.get_or_init(|| {
        if cfg!(target_os = "linux") {
            return true;
        }
        let value = format!("{:x}feed", std::process::id());
        let Ok(mut sleeper) = std::process::Command::new("/bin/sleep")
            .arg("5")
            .env_clear()
            .env(crate::process::MARKER_ENV, &value)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .guarded_spawn()
        else {
            return false;
        };
        std::thread::sleep(Duration::from_millis(200));
        let shown = std::process::Command::new("/bin/ps")
            .args(["-Eww", "-o", "command=", "-p", &sleeper.id().to_string()])
            .env("LC_ALL", "C")
            .guarded_output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&value));
        let _ = sleeper.kill();
        let _ = sleeper.wait();
        shown
    })
}

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
    let mut reaper = Reaper::default();
    for file in files {
        pids.push(reaper.watch(scratch, file).await);
    }
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

/// The live processes started during this test whose command line names
/// `dir` (Linux, from `/proc/<pid>/cmdline`): what a case started there and
/// left running, found independently of any heartbeat or listing delega used.
/// The scripts of a case pass its directory on their command lines (see
/// `gaps::long_relay`), and a fork keeps the command line. The command line is
/// read, as bytes, because it needs no ptrace rights, unlike the working
/// directory, which a process that is not dumpable denies. A failed
/// enumeration, or a process started during the test whose stat or command
/// line cannot be read for a reason other than the process being gone, is an
/// error, never absence.
#[cfg(target_os = "linux")]
pub(super) fn case_processes(dir: &std::path::Path) -> Result<Vec<i32>, String> {
    let gone = |e: &std::io::Error| {
        e.kind() == std::io::ErrorKind::NotFound || e.raw_os_error() == Some(3)
    };
    let names = path_names(dir)?;
    let own_start = std::fs::read("/proc/self/stat")
        .ok()
        .and_then(|stat| stat_state_and_start(&stat))
        .map(|(_, started)| started)
        .ok_or("unreadable /proc/self/stat")?;
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc").map_err(|e| format!("read /proc: {e}"))? {
        let entry = entry.map_err(|e| format!("read /proc entry: {e}"))?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<i32>().ok())
        else {
            continue;
        };
        // A process older than this test process cannot be one of its own,
        // so it is skipped before its command line is read.
        let stat = match std::fs::read(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat,
            Err(e) if gone(&e) => continue,
            Err(e) => return Err(format!("read /proc/{pid}/stat: {e}")),
        };
        let (state, started) =
            stat_state_and_start(&stat).ok_or_else(|| format!("unreadable /proc/{pid}/stat"))?;
        if started < own_start || state == "Z" || state == "X" {
            continue;
        }
        let cmdline = match std::fs::read(format!("/proc/{pid}/cmdline")) {
            Ok(cmdline) => cmdline,
            Err(e) if gone(&e) => continue,
            Err(e) => return Err(format!("read /proc/{pid}/cmdline: {e}")),
        };
        if names.iter().any(|name| holds_path(&cmdline, name)) {
            found.push(pid);
        }
    }
    Ok(found)
}

/// The ways a command line can name `dir`: as given, and canonical.
#[cfg(target_os = "linux")]
fn path_names(dir: &std::path::Path) -> Result<Vec<Vec<u8>>, String> {
    use std::os::unix::ffi::OsStrExt;
    let canonical = std::fs::canonicalize(dir).map_err(|e| format!("canonicalize: {e}"))?;
    let mut names = vec![dir.as_os_str().as_bytes().to_vec()];
    let canonical = canonical.as_os_str().as_bytes().to_vec();
    if !names.contains(&canonical) {
        names.push(canonical);
    }
    Ok(names)
}

/// Whether `bytes` holds `path` as a whole path: followed by the end, a `/`,
/// or a byte that cannot continue a file name the scripts use (a NUL between
/// arguments, a quote, a space), so one directory does not match another
/// whose name it begins.
#[cfg(any(target_os = "linux", test))]
fn holds_path(bytes: &[u8], path: &[u8]) -> bool {
    if path.is_empty() || bytes.len() < path.len() {
        return false;
    }
    (0..=bytes.len() - path.len()).any(|at| {
        bytes[at..].starts_with(path)
            && bytes
                .get(at + path.len())
                .is_none_or(|next| !(next.is_ascii_alphanumeric() || b"._-".contains(next)))
    })
}

#[test]
fn a_command_line_names_a_directory_only_as_a_whole_path() {
    let dir = b"/tmp/.tmpAbC123";
    assert!(holds_path(b"perl\0-e\0code\0/tmp/.tmpAbC123\0", dir));
    assert!(holds_path(
        b"sh\0-c\0exec perl -e 'x' '/tmp/.tmpAbC123'",
        dir
    ));
    assert!(holds_path(b"/tmp/.tmpAbC123/beat", dir));
    assert!(holds_path(b"/tmp/.tmpAbC123", dir));
    assert!(!holds_path(b"/tmp/.tmpAbC1234\0", dir));
    assert!(!holds_path(b"/tmp/.tmpAbC12\0", dir));
    assert!(!holds_path(b"", dir));
}

/// The state letter and the start time in clock ticks (field 22) of one
/// `/proc/<pid>/stat` line, counted from the last `)`. The line is bytes: the
/// kernel writes the task name as it is, which need not be UTF-8 and can hold
/// `)`; only the fields after the last `)` are read as text.
#[cfg(any(target_os = "linux", test))]
fn stat_state_and_start(stat: &[u8]) -> Option<(String, u64)> {
    let end = stat.iter().rposition(|&byte| byte == b')')?;
    let rest = std::str::from_utf8(&stat[end + 1..]).ok()?;
    let fields: Vec<&str> = rest.split_ascii_whitespace().collect();
    Some((fields.first()?.to_string(), fields.get(19)?.parse().ok()?))
}

#[test]
fn a_stat_line_with_a_name_that_is_not_utf8_and_holds_a_parenthesis_is_read() {
    // pid (comm) state ppid pgrp session tty tpgid flags minflt cminflt
    // majflt cmajflt utime stime cutime cstime priority nice threads
    // itrealvalue starttime ...
    let line = b"4242 (a\xff) b\xfe) S 1 4242 4242 0 -1 0 1 2 3 4 5 6 7 8 20 0 1 0 987654 9";
    assert_eq!(stat_state_and_start(line), Some(("S".to_string(), 987_654)));
    assert_eq!(stat_state_and_start(b"4242 (a\xff"), None);
}

pub(super) fn process_names_path(pid: i32, path: &[u8]) -> bool {
    #[cfg(target_os = "linux")]
    let command = std::fs::read(format!("/proc/{pid}/cmdline")).ok();
    #[cfg(not(target_os = "linux"))]
    let command = std::process::Command::new("/bin/ps")
        .args(["-ww", "-o", "command=", "-p", &pid.to_string()])
        .env_clear()
        .guarded_output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| out.stdout);
    command.is_some_and(|command| holds_path(&command, path))
}

/// End the relay processes whose command lines name the case directory.
/// Linux signals through a pidfd. Elsewhere the heartbeat supplies a candidate
/// PID and the start time must still match. Retry for up to five seconds.
pub(super) async fn end_relay_leftovers(dir: &std::path::Path) {
    let began = Instant::now();
    while began.elapsed() < Duration::from_secs(5) {
        #[cfg(target_os = "linux")]
        {
            // An unknown answer is retried; the test has failed already
            // when this runs, so the teardown only does what it can.
            let left = case_processes(dir).unwrap_or_default();
            if left.is_empty() && case_processes(dir).is_ok() {
                return;
            }
            let names = path_names(dir).unwrap_or_default();
            for pid in left {
                if let Some(process) = ProcessIdentity::capture(pid)
                    && names.iter().any(|name| process_names_path(pid, name))
                {
                    process.signal(Signal::KILL);
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            if let Some(record) = PidFile::read(&dir.join("beat")) {
                let fresh = record
                    .modified
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age < Duration::from_millis(300));
                if !fresh {
                    return;
                }
                if let Some(process) = ProcessIdentity::from_pid_file(&record)
                    && process_names_path(record.pid, dir.as_os_str().as_encoded_bytes())
                {
                    process.signal(Signal::KILL);
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// Sets a flag when dropped, so a watcher thread stops even when an
/// assertion fails first.
pub(super) struct SetOnDrop(pub(super) Arc<AtomicBool>);

impl Drop for SetOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn waiting_child() -> std::process::Child {
    use std::process::Stdio;

    std::process::Command::new("/bin/cat")
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .guarded_spawn()
        .expect("start a child waiting for EOF")
}

#[test]
fn a_pid_file_does_not_vouch_for_a_process_started_after_it_was_written() {
    let scratch = Scratch::new();
    let path = scratch.path("pid");
    let written = SystemTime::now();
    // Cross a full second on macOS and a full start tick on Linux.
    std::thread::sleep(Duration::from_secs(1));
    let mut child = waiting_child();
    std::fs::write(&path, format!("{}\n", child.id())).expect("write the test PID");
    std::fs::File::options()
        .write(true)
        .open(&path)
        .expect("open the test PID file")
        .set_modified(written)
        .expect("set the test PID file's write time");
    let record = PidFile::read(&path).expect("read the test PID file");
    assert!(
        ProcessIdentity::capture(record.pid).is_some(),
        "the replacement must be visible"
    );
    let registered = ProcessIdentity::from_pid_file(&record);
    assert!(child.wait().expect("reap the replacement").success());
    assert!(registered.is_none(), "the PID file predates the process");
}

#[test]
fn reaper_does_not_signal_a_replacement_after_the_registered_process_exits() {
    let mut original = waiting_child();
    let reaper = Reaper::new([i32::try_from(original.id()).expect("a pid")]);
    #[cfg(not(target_os = "linux"))]
    let mut reaper = reaper;
    assert_eq!(reaper.0.len(), 1, "capture the original identity");
    assert!(original.wait().expect("reap the original").success());

    // The two starts must differ at ps's one-second precision. This test
    // leaves the same-second reuse window on macOS untested.
    #[cfg(not(target_os = "linux"))]
    std::thread::sleep(Duration::from_secs(1));
    let mut replacement = waiting_child();

    // On Linux the old PID is free, so this cannot distinguish a pidfd
    // signal from signalling that PID by number.
    #[cfg(target_os = "linux")]
    assert!(
        !reaper.0[0].signal(Signal::KILL),
        "the original pidfd must no longer address a process"
    );
    #[cfg(not(target_os = "linux"))]
    {
        // Simulate reuse by making the stored PID resolve to the replacement
        // while keeping the identity captured for the original.
        reaper.0[0].pid = i32::try_from(replacement.id()).expect("a pid");
    }

    drop(reaper);
    assert!(
        replacement.wait().expect("reap the replacement").success(),
        "the replacement was signalled"
    );
}
