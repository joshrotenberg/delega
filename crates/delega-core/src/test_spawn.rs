//! std's `Command::spawn`, `output` and `status` for tests, under the process
//! layer's spawn lock (#104).
//!
//! The unit tests run in one process, beside process-layer runs of other
//! tests. A fixture that spawns `ps` or `mkfifo` without the lock can, on
//! macOS, hand a gated fake provider the write end of its output pipe (so the
//! fixture waits until that provider exits) or give its own child a provider's
//! standard input (so that run waits for the fixture's child). clippy's
//! `disallowed_methods` (the workspace `clippy.toml`) rejects a direct
//! `spawn`, `output` or `status` call, so test spawns use these methods.

use std::io;
use std::process::{Child, Command, ExitStatus, Output, Stdio};

use crate::process::spawn_guard;

pub(crate) trait Guarded {
    /// `spawn` while holding the spawn lock.
    fn guarded_spawn(&mut self) -> io::Result<Child>;
    /// `output` while holding the spawn lock for the spawn only. Like `output`,
    /// standard input is `/dev/null` and both outputs are captured; this
    /// replaces any standard streams set before.
    fn guarded_output(&mut self) -> io::Result<Output>;
    /// `status` while holding the spawn lock for the spawn only.
    fn guarded_status(&mut self) -> io::Result<ExitStatus>;
}

impl Guarded for Command {
    #[allow(clippy::disallowed_methods)]
    fn guarded_spawn(&mut self) -> io::Result<Child> {
        // A process the test makes itself is no keeper's; a short helper
        // (`ps`) in a test that already holds a room changes nothing.
        rooms::enter_if_none(rooms::Room::Unkept);
        let _held = spawn_guard();
        self.spawn()
    }

    fn guarded_output(&mut self) -> io::Result<Output> {
        self.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .guarded_spawn()?
            .wait_with_output()
    }

    fn guarded_status(&mut self) -> io::Result<ExitStatus> {
        self.guarded_spawn()?.wait()
    }
}

/// Which processes a test may leave as orphans while it runs (macOS).
///
/// The macOS keeper counts an orphan of a process created during its run as
/// possibly its own unless another live keeper published its parent. A test
/// that runs a program without a keeper, or makes processes of its own, can
/// leave orphans no keeper published, which a concurrent kept run would
/// rightly count as ambiguous. So each test thread enters a room before its
/// first process-layer run: tests without a keeper share one room, kept runs
/// the other, and the two never overlap. The room is held until the test's
/// thread ends. A thread that already holds the other room (a test that mixes
/// both, or `--test-threads=1`, where tests share the main thread one after
/// another) gives it up first.
pub(crate) mod rooms {
    use std::cell::RefCell;
    use std::sync::{Condvar, Mutex};

    /// The room a test uses.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Room {
        /// Runs without a keeper, and processes the test makes itself.
        Unkept,
        /// Kept runs.
        Kept,
        /// A test that both leaves processes no keeper published and asserts
        /// a kept run's exact verdict: it runs with no other test.
        Alone,
    }

    static STATE: Mutex<(Option<Room>, usize)> = Mutex::new((None, 0));
    static CHANGED: Condvar = Condvar::new();

    struct Held(Room, bool);

    impl Drop for Held {
        fn drop(&mut self) {
            let mut state = STATE.lock().unwrap_or_else(|p| p.into_inner());
            state.1 -= 1;
            if state.1 == 0 {
                state.0 = None;
            }
            CHANGED.notify_all();
        }
    }

    thread_local! {
        static HELD: RefCell<Option<Held>> = const { RefCell::new(None) };
    }

    /// Enter `room` for the whole test, whatever runs it does later: for a
    /// test that leaves a process no keeper published (it kills a keeper, or
    /// keeps a refused process alive after its kept run), so no kept run of
    /// another test overlaps it.
    pub(crate) fn hold(room: Room) {
        enter_inner(room, true);
    }

    /// [`enter`], unless this thread already holds a room.
    pub(crate) fn enter_if_none(room: Room) {
        if HELD.with(|held| held.borrow().is_none()) {
            enter(room);
        }
    }

    /// Enter `room` for the rest of this test thread (macOS only).
    pub(crate) fn enter(room: Room) {
        enter_inner(room, false);
    }

    fn enter_inner(room: Room, sticky: bool) {
        // Only a test's own thread, which libtest names after the test and
        // which ends with it, holds a room: a runtime's worker or blocking
        // thread outlives the test, and the main thread (`--test-threads=1`)
        // runs one test at a time anyway.
        let on_test_thread = std::thread::current()
            .name()
            .is_some_and(|name| name.contains("::"));
        if !cfg!(target_os = "macos") || !on_test_thread {
            return;
        }
        HELD.with(|held| {
            let mut held = held.borrow_mut();
            if held.as_ref().is_some_and(|h| h.1 && !sticky) {
                return;
            }
            if held.as_ref().is_some_and(|h| h.0 == room) {
                if let Some(h) = held.as_mut() {
                    h.1 |= sticky;
                }
                return;
            }
            *held = None;
            let mut state = STATE.lock().unwrap_or_else(|p| p.into_inner());
            while state.1 > 0 && (state.0 != Some(room) || room == Room::Alone) {
                state = CHANGED.wait(state).unwrap_or_else(|p| p.into_inner());
            }
            state.0 = Some(room);
            state.1 += 1;
            *held = Some(Held(room, sticky));
        });
    }
}
