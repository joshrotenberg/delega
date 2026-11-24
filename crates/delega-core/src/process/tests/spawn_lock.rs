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
//! after `pipe()` and before it marks the pipe close-on-exec, and it keeps that
//! window open until the siblings have done what the case expects. No race has
//! to be won:
//!
//! - Guarded, the siblings start through the process layer's `spawn`, which
//!   takes the same lock. None of them can start while the window is open, and
//!   none holds the pipe afterwards.
//! - Unguarded (macOS only), the sibling spawns directly. It starts while the
//!   window is open, so it is certain to inherit the write end, and the check
//!   sees it. That is what makes a pass in the guarded case mean something.
//!
//! Whether a child holds the write end is read from the pipe: once the test
//! closes its own write end, a nonblocking read gives end of input if no other
//! process holds one and `EAGAIN` if one does. The read is made under the spawn
//! lock, so no spawn of another test can be between fork and exec at that
//! moment holding a copy, and the window is held under the lock too, so the
//! only child that can be forked inside it is the unguarded sibling. Every
//! child is killed and waited for before the test ends.

use std::os::fd::OwnedFd;
use std::sync::{Arc, Barrier, mpsc};
use std::time::Duration;

use rustix::io::{Errno, FdFlags};
use tokio::process::{Child, Command};

use crate::process::spawn::spawn;
use crate::process::spawn_guard;

/// How long the window stays open in the guarded case while the siblings wait
/// on the lock. Any guarded sibling that started in this time would be a
/// failure; the length only decides how long a regression has to show itself.
const WINDOW: Duration = Duration::from_millis(200);
/// A bound on waits for events that do happen (an unguarded spawn returning,
/// a killed child being reaped), far above what they take.
const BOUND: Duration = Duration::from_secs(60);

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
    children: Vec<Child>,
}

/// A child that stays alive until it is killed. No pipes of its own, so
/// unguarded siblings cannot inherit each other's.
fn sibling(dir: &std::path::Path) -> Command {
    let mut command = Command::new("/bin/sleep");
    command
        .arg("600")
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
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

    let barrier = Arc::new(Barrier::new(siblings + 1));
    let (tx, rx) = mpsc::channel();
    let threads: Vec<_> = (0..siblings)
        .map(|_| {
            let barrier = barrier.clone();
            let tx = tx.clone();
            let handle = runtime.handle().clone();
            let mut command = sibling(dir);
            std::thread::spawn(move || {
                // tokio's Command registers the child with the runtime.
                let _runtime = handle.enter();
                barrier.wait();
                let child = if guarded {
                    spawn(&mut command)
                } else {
                    // The unguarded sibling has to fork inside the window to
                    // show that the check sees an inherited pipe end, so it
                    // must bypass the spawn lock. The window's holder keeps
                    // the lock, so no other spawn in this binary can run
                    // beside it.
                    #[allow(clippy::disallowed_methods)]
                    command.spawn()
                };
                let _ = tx.send(child.expect("start a sibling"));
            })
        })
        .collect();
    drop(tx);
    barrier.wait();

    let mut children = Vec::new();
    if guarded {
        // Every sibling is at or past the barrier and trying to take the
        // lock; none may get through while the window is open.
        if let Ok(child) = rx.recv_timeout(WINDOW) {
            children.push(child);
        }
    } else {
        // Each unguarded sibling is started before the window closes.
        for _ in 0..siblings {
            children.push(
                rx.recv_timeout(BOUND)
                    .expect("an unguarded sibling did not start"),
            );
        }
    }
    let started_in_window = children.len();

    // Close the window as std does: mark the pipe close-on-exec, then let go
    // of the lock.
    for end in [&read, &write] {
        rustix::io::fcntl_setfd(end, FdFlags::CLOEXEC).expect("mark the pipe close-on-exec");
    }
    drop(window);

    children.extend(rx.iter());
    for thread in threads {
        thread.join().expect("a sibling's thread panicked");
    }
    assert_eq!(children.len(), siblings, "every sibling started");

    drop(write);
    let held = write_end_held(&read);
    Seen {
        started_in_window,
        held,
        read,
        children,
    }
}

/// Kill and reap every child, so the test leaves no process behind.
fn reap(runtime: &tokio::runtime::Runtime, children: Vec<Child>) {
    runtime.block_on(async {
        for mut child in children {
            let _ = child.start_kill();
            tokio::time::timeout(BOUND, child.wait())
                .await
                .expect("a killed sibling was not reaped in time")
                .expect("wait for a killed sibling");
        }
    });
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
    reap(&runtime, seen.children);
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
    reap(&runtime, seen.children);
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
