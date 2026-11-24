//! The spawn lock keeps a sibling child from inheriting a pipe that another
//! spawn has open without close-on-exec (#104, #107, #185).
//!
//! Ported from due's `darwin_sibling_pipe_collection_survives_the_cloexec_window`
//! (`crates/due-harness/src/supervisor.rs`, due at 8d56df2, due#144). due
//! interposes `pipe()` with an injected library so that a real spawn stops
//! between creating its pipe and marking it close-on-exec, starts a sibling in
//! that window and checks whether the sibling holds the pipe's write end. Here
//! the test opens the window itself: it takes [`spawn_guard`] and creates a
//! pipe with plain `pipe(2)`, which is the state std's spawn is in on macOS
//! after `pipe()` and before it marks the pipe close-on-exec. The order of
//! events is arranged, not timed:
//!
//! - Guarded, the siblings start through the process layer's `spawn`, which
//!   takes the same lock. The window stays open until the lock's test seam
//!   (`spawn::WAITING`) shows every sibling waiting to take the lock. A spawn
//!   that returns while the window is open, or siblings that are not all seen
//!   waiting within [`DEADLINE`], fail the test; a spawn that skips the lock
//!   is never seen waiting, so it cannot pass. Once the window has closed, no
//!   sibling may hold the pipe.
//! - Unguarded (macOS only), the sibling spawns directly. The window stays
//!   open until its spawn has returned, so it is certain to inherit the write
//!   end, and the check sees it. That is what makes a pass in the guarded case
//!   mean something.
//!
//! What this proves is the lock's contract: a spawn through the process
//! layer's `spawn` cannot run while another holder of `spawn_guard()` has a
//! pipe open without close-on-exec, and a spawn that skips the lock inside such
//! a window does inherit the pipe, which the check sees. It does not prove that
//! every place in production that creates a pipe or spawns a child takes the
//! guard. clippy's `disallowed_methods` rule in the workspace `clippy.toml`
//! enforces that, not this test.
//!
//! The siblings are built as the provider is when it is started directly:
//! `run::command`, `kill_on_drop` and `sys::new_session`. The keeper is started
//! the same way. The `pre_exec` hook of `new_session` makes std fork and exec
//! instead of using `posix_spawn`, and each sibling is checked to lead its own
//! session, which shows the hook ran in it. The `ps` listing in `tree.rs` is
//! started without a `pre_exec` hook, so std may use `posix_spawn` there; that
//! path is not exercised here.
//!
//! Whether a child holds the write end is read from the pipe: once the test
//! closes its own write end, a nonblocking read gives end of input if no other
//! process holds one and `EAGAIN` if one does.
//!
//! Each case runs in a process of its own: the outer test re-runs this test
//! binary on one ignored inner test, as due does, and kills it if it has not
//! finished within [`OUTER_DEADLINE`]. A regression that leaves a sibling
//! stuck holding the spawn lock (a spawn that takes it twice, say) then fails
//! that case without blocking every other spawn in the test binary, and no
//! other test's spawn can add to the waiting count or fork inside the window.
//! The siblings are `cat` reading a pipe from the inner process, so they also
//! end by themselves if that process is killed; otherwise every sibling is
//! killed and reaped, on failure paths too.

use std::os::fd::OwnedFd;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use rustix::io::{Errno, FdFlags};
use tokio::process::{Child, Command};

use crate::process::run::command;
use crate::process::spawn::{WAITING, spawn};
use crate::process::{EnvPolicy, ProcessSpec, spawn_guard, sys};
use crate::test_spawn::Guarded;

/// A bound on every wait inside a case for an event that does happen
/// (siblings waiting on the lock, a spawn returning, a killed child being
/// reaped), far above what it takes.
const DEADLINE: Duration = Duration::from_secs(10);
/// How long the outer test lets a case's process run before it kills it.
const OUTER_DEADLINE: Duration = Duration::from_secs(60);
/// Set by the outer test for the inner one, which does nothing without it.
const FIXTURE: &str = "DELEGA_SPAWN_LOCK_FIXTURE";

/// One started sibling. Dropping it kills the child and waits, for at most
/// [`DEADLINE`], until it is reaped, so every sibling is reaped however the
/// case ends, each on its own.
struct Sibling(Option<Child>);

impl Sibling {
    fn pid(&self) -> i32 {
        self.0
            .as_ref()
            .and_then(Child::id)
            .and_then(|id| i32::try_from(id).ok())
            .expect("a sibling's pid")
    }

    /// Kill and reap the child; whether it was reaped in time.
    fn reap(&mut self) -> bool {
        let Some(mut child) = self.0.take() else {
            return true;
        };
        let _ = child.start_kill();
        let began = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return true,
                Ok(None) if began.elapsed() < DEADLINE => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Ok(None) | Err(_) => return false,
            }
        }
    }
}

impl Drop for Sibling {
    fn drop(&mut self) {
        if !self.reap() {
            eprintln!("a killed sibling was not reaped within {DEADLINE:?}");
        }
    }
}

/// Reap every sibling, each on its own, then fail if any was not reaped.
fn reap_all(siblings: Vec<Sibling>) {
    let unreaped = siblings
        .into_iter()
        .map(|mut sibling| sibling.reap())
        .filter(|reaped| !reaped)
        .count();
    assert_eq!(
        unreaped, 0,
        "{unreaped} killed sibling(s) not reaped within {DEADLINE:?}"
    );
}

/// What one run of the window saw.
struct Seen {
    /// Whether some child held the window's write end once the test closed its
    /// own.
    held: bool,
    /// The window's read end, still open, for the check after the children
    /// are gone. Only the unguarded case reads it, and that case is macOS only.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    read: OwnedFd,
    siblings: Vec<Sibling>,
}

/// A child set up as the provider is when started directly (see
/// `OwnedProcess::spawn`). `cat` with its standard input a pipe from this
/// process: it lives until it is killed or this process exits.
fn provider_command(dir: &Path) -> Command {
    let spec = ProcessSpec::new("/bin/cat", DEADLINE)
        .cwd(dir)
        .env(EnvPolicy::inherit_only(["PATH"]))
        .stdin("");
    let mut command = command(&spec);
    command.kill_on_drop(true);
    sys::new_session(command.as_std_mut());
    command
}

/// Whether a process other than this one holds the write end of the pipe
/// whose read end is `read`, once this process has closed its own. Taken under
/// the spawn lock, so no spawn can be between fork and exec holding a copy.
fn write_end_held(read: &OwnedFd) -> bool {
    let _held = spawn_guard();
    rustix::io::ioctl_fionbio(read, true).expect("make the read end nonblocking");
    let mut byte = [0u8; 1];
    match rustix::io::read(read, &mut byte) {
        Ok(0) => false,
        Ok(n) => panic!("nothing writes to the window's pipe, yet {n} byte(s) came"),
        Err(Errno::AGAIN) => true,
        Err(e) => panic!("read the window's pipe: {e}"),
    }
}

/// Receive one sibling before `until`, or fail saying `what`.
fn receive(rx: &mpsc::Receiver<std::io::Result<Sibling>>, until: Instant, what: &str) -> Sibling {
    match rx.recv_timeout(until.saturating_duration_since(Instant::now())) {
        Ok(started) => started.expect("start a sibling"),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("{what} within {DEADLINE:?}: a spawn may be stuck on the lock")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("a sibling's thread ended without a result; its message is above")
        }
    }
}

/// Open a pipe without close-on-exec under the spawn lock, start `count`
/// siblings from other threads, close the window once the case's condition
/// holds and report whether any child holds the write end. `guarded` siblings
/// start through the process layer's `spawn`; the others spawn directly.
fn open_window(runtime: &tokio::runtime::Runtime, dir: &Path, count: usize, guarded: bool) -> Seen {
    let window = spawn_guard();
    assert_eq!(
        WAITING.load(Ordering::SeqCst),
        0,
        "nothing else in this process may wait on the spawn lock"
    );
    // Plain pipe(2): no close-on-exec, as std's pipes are on macOS between
    // `pipe()` and the `fcntl` that marks them.
    let (read, write) = rustix::pipe::pipe().expect("open the window's pipe");
    for end in [&read, &write] {
        let flags = rustix::io::fcntl_getfd(end).expect("read the pipe's flags");
        assert!(
            !flags.contains(FdFlags::CLOEXEC),
            "the window's pipe must start without close-on-exec"
        );
    }

    let (tx, rx) = mpsc::channel::<std::io::Result<Sibling>>();
    for _ in 0..count {
        let tx = tx.clone();
        let handle = runtime.handle().clone();
        let mut command = provider_command(dir);
        // Not joined: a sibling stuck on the lock must not hang the case. If
        // the case has given up, the send fails and dropping the sibling
        // reaps it.
        std::thread::spawn(move || {
            // tokio's Command registers the child with the runtime.
            let _runtime = handle.enter();
            let started = if guarded {
                spawn(&mut command)
            } else {
                // The unguarded sibling has to fork inside the window to show
                // that the check sees an inherited pipe end, so it must bypass
                // the spawn lock. The case runs in a process of its own and
                // the window's holder keeps the lock, so no other spawn runs
                // beside it.
                #[allow(clippy::disallowed_methods)]
                command.spawn()
            };
            let _ = tx.send(started.map(|child| Sibling(Some(child))));
        });
    }
    drop(tx);

    let mut siblings = Vec::new();
    let until = Instant::now() + DEADLINE;
    if guarded {
        // Proof of contention: every sibling waits on the lock while the
        // window is open, and none gets through.
        loop {
            match rx.try_recv() {
                Ok(_) => panic!(
                    "a sibling's spawn returned while another holder of the spawn lock had a \
                     pipe open without close-on-exec"
                ),
                Err(mpsc::TryRecvError::Disconnected) => {
                    panic!("a sibling's thread ended without a result; its message is above")
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            let waiting = WAITING.load(Ordering::SeqCst);
            if waiting >= count {
                break;
            }
            assert!(
                Instant::now() < until,
                "only {waiting} of {count} siblings were seen waiting on the spawn lock within \
                 {DEADLINE:?}: a spawn through the process layer did not take the lock"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    } else {
        // The window stays open until each unguarded sibling has started.
        for _ in 0..count {
            siblings.push(receive(&rx, until, "an unguarded sibling did not start"));
        }
    }

    // Close the window as std does: mark the pipe close-on-exec, then let go
    // of the lock.
    for end in [&read, &write] {
        rustix::io::fcntl_setfd(end, FdFlags::CLOEXEC).expect("mark the pipe close-on-exec");
    }
    drop(window);

    let until = Instant::now() + DEADLINE;
    while siblings.len() < count {
        siblings.push(receive(&rx, until, "a guarded sibling did not start"));
    }
    for sibling in &siblings {
        let pid = sibling.pid();
        assert_eq!(
            sys::session_of(pid),
            Some(pid),
            "a sibling does not lead its own session, so the new_session pre_exec hook \
             (and with it std's fork and exec path) did not run"
        );
    }

    drop(write);
    let held = write_end_held(&read);
    Seen {
        held,
        read,
        siblings,
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build a runtime")
}

/// Whether this is the case's own process. Run any other way, an inner test
/// does nothing: in the shared test process other tests' spawns would wait
/// on the lock and fork beside it.
fn in_fixture_process() -> bool {
    if std::env::var_os(FIXTURE).is_some() {
        return true;
    }
    eprintln!("SKIPPED: run by its outer test in a process of its own");
    false
}

/// Re-run this test binary on the inner test `name` alone, kill it if it
/// takes longer than [`OUTER_DEADLINE`] and fail with its output unless it
/// ran exactly that one test and passed.
fn run_case(name: &str) {
    let module = module_path!()
        .split_once("::")
        .map(|(_, rest)| rest)
        .expect("a module path below the crate");
    let test = format!("{module}::{name}");
    let dir = tempfile::tempdir().expect("create scratch directory");
    let out = dir.path().join("out");
    // One open file for both streams, so they share an offset and neither
    // overwrites the other.
    let file = std::fs::File::create(&out).expect("create the output file");
    let mut case = std::process::Command::new(std::env::current_exe().expect("this test binary"))
        .args([test.as_str(), "--exact", "--ignored", "--nocapture"])
        .args(["--test-threads=1"])
        .env(FIXTURE, "1")
        .stdin(std::process::Stdio::null())
        .stdout(file.try_clone().expect("share the output file"))
        .stderr(file)
        .guarded_spawn()
        .expect("start the case's process");
    let began = Instant::now();
    let status = loop {
        if let Some(status) = case.try_wait().expect("wait for the case's process") {
            break Some(status);
        }
        if began.elapsed() >= OUTER_DEADLINE {
            let _ = case.kill();
            let _ = case.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let output = std::fs::read_to_string(&out).unwrap_or_default();
    match status {
        None => {
            panic!("{test} did not finish within {OUTER_DEADLINE:?}; killed. Output:\n{output}")
        }
        Some(status) if !status.success() => panic!("{test} failed ({status}). Output:\n{output}"),
        Some(_) => assert!(
            output.contains("test result: ok. 1 passed"),
            "{test} did not run as the one test of its process. Output:\n{output}"
        ),
    }
}

#[test]
fn a_guarded_sibling_never_holds_a_pipe_from_another_spawns_cloexec_window() {
    run_case("guarded_case");
}

#[test]
#[ignore = "run by its outer test in a process of its own"]
fn guarded_case() {
    if !in_fixture_process() {
        return;
    }
    let runtime = runtime();
    let dir = tempfile::tempdir().expect("create scratch directory");
    let seen = open_window(&runtime, dir.path(), 4, true);
    let held = seen.held;
    reap_all(seen.siblings);
    assert!(
        !held,
        "a sibling started through the spawn lock holds the write end of a pipe from \
         another spawn's close-on-exec window"
    );
}

/// The check can see the bug: a sibling that skips the lock and starts inside
/// the window holds the pipe's write end. On Linux std creates its pipes with
/// `pipe2` and `O_CLOEXEC`, so there is no window to open there.
#[cfg(target_os = "macos")]
#[test]
fn an_unguarded_sibling_started_in_the_cloexec_window_is_seen_holding_the_pipe() {
    run_case("unguarded_case");
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "run by its outer test in a process of its own"]
fn unguarded_case() {
    if !in_fixture_process() {
        return;
    }
    let runtime = runtime();
    let dir = tempfile::tempdir().expect("create scratch directory");
    let seen = open_window(&runtime, dir.path(), 1, false);
    let (held, read) = (seen.held, seen.read);
    reap_all(seen.siblings);
    assert!(
        held,
        "an unguarded sibling started in the window was not seen holding the pipe's write end"
    );
    assert!(
        !write_end_held(&read),
        "the write end is still held after the sibling was reaped, so it was not the sibling \
         that held it"
    );
}
