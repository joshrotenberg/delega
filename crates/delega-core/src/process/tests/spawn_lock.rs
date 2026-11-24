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
//! after `pipe()` and before it marks the pipe close-on-exec. It keeps that
//! window open until every sibling has said it is about to call spawn, and in
//! the guarded case for [`WINDOW`] more:
//!
//! - Guarded, the siblings start through the process layer's `spawn`, which
//!   takes the same lock. A spawn that returns while the window is open is a
//!   failure, and no sibling may hold the pipe afterwards.
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
//! process holds one and `EAGAIN` if one does. The read is made under the spawn
//! lock, so no spawn of another test can be between fork and exec at that
//! moment holding a copy, and the window is held under the lock too, so the
//! only child that can be forked inside it is the unguarded sibling.
//!
//! Every wait is bounded by [`DEADLINE`], so a regression that leaves a sibling
//! stuck (for example a spawn that takes the lock twice) fails the test instead
//! of hanging it; the stuck thread is left behind. Every child that was started
//! is killed and reaped, on failure paths too.

use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use rustix::io::{Errno, FdFlags};
use tokio::process::{Child, Command};

use crate::process::run::command;
use crate::process::spawn::spawn;
use crate::process::{EnvPolicy, ProcessSpec, spawn_guard, sys};

/// How long the window stays open in the guarded case after every sibling has
/// said it is about to call spawn. Any guarded sibling whose spawn returned in
/// this time would be a failure; the length only decides how long a
/// regression has to show itself.
const WINDOW: Duration = Duration::from_millis(200);
/// A bound on waits for events that do happen (siblings reaching their spawn
/// call, an unguarded spawn returning, a killed child being reaped), far above
/// what they take.
const DEADLINE: Duration = Duration::from_secs(10);

/// What a sibling's thread sends back.
type Started = std::io::Result<Child>;

/// The children started so far. Dropping this kills and reaps them, so a
/// failing test leaves no process behind either.
#[derive(Default)]
struct Children(Vec<Child>);

impl Children {
    /// Kill and reap every child, and fail if one is not reaped in time.
    fn reap(mut self) {
        let children = std::mem::take(&mut self.0);
        for mut child in children {
            assert!(
                kill_and_reap(&mut child),
                "a killed sibling was not reaped within {DEADLINE:?}"
            );
        }
    }
}

impl Drop for Children {
    fn drop(&mut self) {
        for child in &mut self.0 {
            if !kill_and_reap(child) {
                eprintln!("a killed sibling was not reaped within {DEADLINE:?}");
            }
        }
    }
}

/// Kill `child` and wait, for at most [`DEADLINE`], until it is reaped.
fn kill_and_reap(child: &mut Child) -> bool {
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

/// What one run of the window saw.
struct Seen {
    /// Siblings whose spawn returned while the window was open.
    started_in_window: usize,
    /// Whether some child held the window's write end once the test closed its
    /// own.
    held: bool,
    /// The window's read end, still open, for the check after the children
    /// are gone. Only the unguarded case reads it, and that case is macOS only.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    read: OwnedFd,
    children: Children,
}

/// A child set up as the provider is when started directly (see
/// `OwnedProcess::spawn`), that stays alive until it is killed.
fn provider_command(dir: &std::path::Path) -> Command {
    let spec = ProcessSpec::new("/bin/sleep", Duration::from_secs(600))
        .args(["600"])
        .cwd(dir)
        .env(EnvPolicy::inherit_only(["PATH"]));
    let mut command = command(&spec);
    command.kill_on_drop(true);
    sys::new_session(command.as_std_mut());
    command
}

/// Whether a process other than this one holds the write end of the pipe
/// whose read end is `read`, once this process has closed its own. Taken under
/// the spawn lock: see the module comment.
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

/// Receive one sibling's result before `until`, or fail saying `what`.
fn receive(rx: &mpsc::Receiver<Started>, until: Instant, what: &str) -> Child {
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

/// Open a pipe without close-on-exec under the spawn lock, start `siblings`
/// children from other threads while it is open, close the window and report
/// whether any child holds the write end. `guarded` siblings start through the
/// process layer's `spawn`; the others spawn directly.
fn open_window(
    runtime: &tokio::runtime::Runtime,
    dir: &std::path::Path,
    siblings: usize,
    guarded: bool,
) -> Seen {
    let mut children = Children::default();
    let window = spawn_guard();
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

    let reached = Arc::new(AtomicUsize::new(0));
    let (tx, rx) = mpsc::channel::<Started>();
    for _ in 0..siblings {
        let reached = reached.clone();
        let tx = tx.clone();
        let handle = runtime.handle().clone();
        let mut command = provider_command(dir);
        // Not joined: a sibling stuck on the lock must not hang the test. Each
        // thread ends right after it sends its result.
        std::thread::spawn(move || {
            // tokio's Command registers the child with the runtime.
            let _runtime = handle.enter();
            reached.fetch_add(1, Ordering::SeqCst);
            let started = if guarded {
                spawn(&mut command)
            } else {
                // The unguarded sibling has to fork inside the window to show
                // that the check sees an inherited pipe end, so it must bypass
                // the spawn lock. The window's holder keeps the lock, so no
                // other spawn in this binary can run beside it.
                #[allow(clippy::disallowed_methods)]
                command.spawn()
            };
            if let Err(mpsc::SendError(Ok(mut child))) = tx.send(started) {
                // The test gave up waiting; do not leave the child behind.
                kill_and_reap(&mut child);
            }
        });
    }
    drop(tx);

    // Positive evidence that every sibling is at its spawn call before the
    // window can close.
    let began = Instant::now();
    while reached.load(Ordering::SeqCst) < siblings {
        assert!(
            began.elapsed() < DEADLINE,
            "only {} of {siblings} siblings reached their spawn call within {DEADLINE:?}",
            reached.load(Ordering::SeqCst)
        );
        std::thread::sleep(Duration::from_millis(1));
    }

    if guarded {
        // None may get through while the window is open.
        let closes = Instant::now() + WINDOW;
        while let Ok(started) = rx.recv_timeout(closes.saturating_duration_since(Instant::now())) {
            children.0.push(started.expect("start a sibling"));
        }
    } else {
        // The window stays open until each unguarded sibling has started.
        let until = Instant::now() + DEADLINE;
        for _ in 0..siblings {
            children
                .0
                .push(receive(&rx, until, "an unguarded sibling did not start"));
        }
    }
    let started_in_window = children.0.len();

    // Close the window as std does: mark the pipe close-on-exec, then let go
    // of the lock.
    for end in [&read, &write] {
        rustix::io::fcntl_setfd(end, FdFlags::CLOEXEC).expect("mark the pipe close-on-exec");
    }
    drop(window);

    let until = Instant::now() + DEADLINE;
    while children.0.len() < siblings {
        children
            .0
            .push(receive(&rx, until, "a guarded sibling did not start"));
    }
    for child in &children.0 {
        let pid = child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .expect("a sibling's pid");
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
        started_in_window,
        held,
        read,
        children,
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build a runtime")
}

#[test]
fn a_guarded_sibling_never_holds_a_pipe_from_another_spawns_cloexec_window() {
    let runtime = runtime();
    let dir = tempfile::tempdir().expect("create scratch directory");
    let seen = open_window(&runtime, dir.path(), 4, true);
    let (started_in_window, held) = (seen.started_in_window, seen.held);
    seen.children.reap();
    assert_eq!(
        started_in_window, 0,
        "a sibling started through the spawn lock while another spawn held it"
    );
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
    let runtime = runtime();
    let dir = tempfile::tempdir().expect("create scratch directory");
    let seen = open_window(&runtime, dir.path(), 1, false);
    let (started_in_window, held, read) = (seen.started_in_window, seen.held, seen.read);
    seen.children.reap();
    assert_eq!(
        started_in_window, 1,
        "the unguarded sibling started in the window"
    );
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
