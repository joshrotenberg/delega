//! The keeper's fork tracker on macOS.
//!
//! macOS has no subreaper, so an orphan leaves the keeper's tree for launchd
//! the moment its parent exits. The tracker follows the tree as it grows
//! instead: a `kqueue` watches every process it holds for `NOTE_FORK` and
//! `NOTE_EXIT`, and on a fork it lists the parent's children at once, while
//! the parent is normally still alive, and takes each new one in turn.
//!
//! - Identity: a process is held by its pid, its unique id (never reused) and
//!   its pid version (see [`darwin`]). A listed child is taken only if the
//!   unique id of the process that forked it is its parent's; every unique id
//!   ever taken stays known after the process exits.
//! - Proven and suspect: a process is proven the tree's when the process that
//!   forked it was, or when it is in the program's session (which the
//!   program's pid pins while the keeper holds it unreaped, and which only
//!   the program's descendants can be in). A suspect is a process that may be
//!   the tree's and cannot be proven either way (below). Suspects are watched
//!   and followed like the rest, and their children are suspects too, but a
//!   suspect is never signalled, and while one is alive every answer reports
//!   it as ambiguous, which makes the owner's verdict `Unconfirmed`.
//! - Signals go by audit token, to proven processes only: the kernel checks
//!   the pid version at delivery, so a pid that names another process by then
//!   is never signalled. Nothing is signalled by pid number. An exec gives a
//!   process a new pid version and keeps its unique id, so a signal refused
//!   as stale is sent again with the version read afresh only when the pid
//!   still has the unique id held.
//! - Order: a process is first read, then watched, then its children are
//!   listed, so a fork between the listing that found it and the watch is
//!   still seen. The program is read and watched on the keeper's main thread
//!   as soon as it started.
//! - A process just forked can be listed before libproc describes it; the
//!   kernel's own record (`sysctl`) tells such a process from a zombie, and
//!   it is read again until it can be.
//! - A loss: the parent had exited by the time its children were listed. The
//!   recovery scan reads every process of this user and takes every one whose
//!   parent unique id is known, wherever it is parented by now and whatever
//!   it did since (a new session, a cleared environment, an exec of an Apple
//!   program).
//! - What the recovery cannot prove: the child of a process that was never
//!   taken, because it was forked, forked in turn and exited before any
//!   listing saw it (a lost process's child that exited before the scan, or a
//!   child that was reaped before its parent's listing). Each such case
//!   leaves a sign: a loss, a listed child that exited before it could be
//!   read, or a fork that no listed child explains. The time around each,
//!   and the unique id of the process that forked, make a suspect window.
//!   At every recovery and at every census and freeze, a process becomes a
//!   suspect when it belongs to this user, has been reparented to launchd,
//!   was forked by a process created after the one the window names, started
//!   inside the window, and is not in a group or session that proves it
//!   someone else's (see [`Tracker::provably_foreign`]).
//! - What leaves no sign: a process taken late (it is found by its parent's
//!   listing, a few hundred microseconds after its fork) that had already
//!   forked a child that forked, exited and was reaped by it before the
//!   process was listed. Its forks before the watch report nothing.
//! - The run is recorded, not stopped: nothing is signalled until the owner
//!   asks for a freeze or the keeper stops the tree by itself. From the first
//!   freeze on, every proven child found is stopped as soon as it is taken.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::mpsc;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::process::limits::KEEPER_FREEZE_PASSES;
use crate::process::sys::darwin::{self, Ident, ProcEvent, ProcQueue, Sent};
use crate::process::sys::{self, Signal};

/// How long the tracker thread waits for events before it looks for a
/// request.
const WAIT: Duration = Duration::from_millis(10);
/// Unique ids kept before the tracker counts itself saturated.
const MAX_KNOWN: usize = 4_000_000;
/// Suspect windows (and remembered zombies) kept before the tracker counts
/// itself saturated.
const MAX_WINDOWS: usize = 100_000;
/// Added to both ends of a suspect window, for the time between a clock read
/// and the kernel's own.
const MARGIN_US: u64 = 1_000;
/// Listings one scan may take while new processes keep appearing before
/// the tracker counts a hole.
const MAX_ROUNDS: u32 = 64;
/// How deep recovery scans may nest before the tracker counts a hole.
const MAX_DEPTH: u32 = 32;
/// How long a process being created is waited for before it counts as a
/// failed read.
const CREATING_WAIT: Duration = Duration::from_millis(20);

fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
}

/// The identity of a process just listed. libproc answers ESRCH both for a
/// zombie and for a process still being created by `fork`; the kernel's own
/// record tells them apart, and a process not yet described is read again
/// until it is. `Ok(None)` when it is gone or a zombie; an error when it
/// could not be read within [`CREATING_WAIT`] or for another reason.
fn read_new(pid: i32) -> Result<Option<Ident>, rustix::io::Errno> {
    let until = Instant::now() + CREATING_WAIT;
    loop {
        if let Some(ident) = darwin::identity(pid)? {
            return Ok(Some(ident));
        }
        match darwin::kern_proc(pid)? {
            None => return Ok(None),
            Some(k) if k.zombie() => return Ok(None),
            Some(_) => {}
        }
        if Instant::now() >= until {
            return Err(rustix::io::Errno::AGAIN);
        }
        std::thread::sleep(Duration::from_micros(100));
    }
}

/// What a census or a freeze found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Report {
    /// Live processes proven the tree's.
    pub live: usize,
    pub passes: u32,
    /// Whether `live` is proven (see [`Tracker::census`] and
    /// [`Tracker::freeze_kill`]). An answer that is not is never zero.
    pub complete: bool,
    /// Signals the kernel refused, and processes that could not be watched.
    pub refused: u32,
    /// Reads that failed for a reason other than the process being gone,
    /// and a tracker that failed or saturated.
    pub read_failures: u32,
    /// Forks whose parent had exited before its children were listed.
    pub losses: u32,
    /// Processes the recovery and second-source scans took.
    pub recovered: u32,
    /// Live suspects: processes that may be the tree's and cannot be proven
    /// either way. Never signalled.
    pub ambiguous: u32,
    /// Where the first failed read happened, when one did.
    pub hole: Option<&'static str>,
}

impl Report {
    pub(super) fn detail(&self) -> String {
        format!(
            "passes={} complete={} refused={} read_failures={} losses={} recovered={} \
             ambiguous={}",
            self.passes,
            self.complete,
            self.refused,
            self.read_failures,
            self.losses,
            self.recovered,
            self.ambiguous
        ) + &self.hole.map(|h| format!(" hole={h}")).unwrap_or_default()
    }

    /// The count to act on: an unproven result is never zero.
    pub(super) fn effective_live(&self) -> usize {
        if self.complete {
            self.live
        } else {
            self.live.max(1)
        }
    }
}

enum Request {
    /// The program: its pid, its identity read right after it started, and
    /// whether watching it succeeded.
    Start(i32, Option<Ident>, Result<(), rustix::io::Errno>),
    Census(mpsc::Sender<Report>),
    Freeze(mpsc::Sender<Report>),
    Signal(Signal, mpsc::Sender<()>),
}

/// The tracker as the keeper's other threads reach it.
#[derive(Clone)]
pub(super) struct Handle {
    tx: mpsc::Sender<Request>,
    queue: Arc<ProcQueue>,
}

static TRACKER: OnceLock<Handle> = OnceLock::new();

/// The tracker of this keeper, once [`prepare`] succeeded.
pub(super) fn get() -> Option<&'static Handle> {
    TRACKER.get()
}

/// A test-only seam: when the keeper's environment has this variable,
/// whatever its value, the keeper does not track forks, as when its
/// self-test fails. It can only make cleanup weaker.
pub(crate) const NO_TRACKING_ENV: &str = "DELEGA_TEST_NO_FORK_TRACKING";

/// Check the calls the tracker relies on, open its queue and start its
/// thread, which waits for [`Handle::start`]. The keeper says `tracker on`
/// only when this succeeds.
pub(super) fn prepare() -> Result<(), String> {
    if std::env::var_os(NO_TRACKING_ENV).is_some() {
        return Err(format!("{NO_TRACKING_ENV} is set"));
    }
    darwin::self_test()?;
    let queue = Arc::new(ProcQueue::new().map_err(|e| format!("kqueue failed: {e}"))?);
    let me = darwin::identity(rustix::process::getpid().as_raw_nonzero().get())
        .ok()
        .flatten()
        .ok_or("the keeper's own identity could not be read")?;
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("keeper-track".into())
        .spawn({
            let queue = queue.clone();
            move || Tracker::new(queue, me.uniqueid).run(&rx)
        })
        .map_err(|e| format!("the tracker thread could not start: {e}"))?;
    TRACKER
        .set(Handle { tx, queue })
        .map_err(|_| "the tracker was prepared twice".to_string())
}

impl Handle {
    /// Track `program` from now on. Called right after it started, on the
    /// thread that started it: the program is read and watched here, so a
    /// fork from then on is reported however soon the tracker thread runs,
    /// and children it forks before that are known by its unique id.
    pub(super) fn start(&self, program: i32) {
        let ident = darwin::identity(program).ok().flatten();
        let watched = self.queue.watch(program);
        let _ = self.tx.send(Request::Start(program, ident, watched));
    }

    fn ask(&self, make: impl FnOnce(mpsc::Sender<Report>) -> Request) -> Report {
        let (tx, rx) = mpsc::channel();
        let failed = Report {
            live: 1,
            read_failures: 1,
            ..Report::default()
        };
        if self.tx.send(make(tx)).is_err() {
            return failed;
        }
        rx.recv().unwrap_or(failed)
    }

    pub(super) fn census(&self) -> Report {
        self.ask(Request::Census)
    }

    pub(super) fn freeze_kill(&self) -> Report {
        self.ask(Request::Freeze)
    }

    /// Send `signal`, then SIGCONT, to every live proven process held.
    pub(super) fn signal_all(&self, signal: Signal) {
        let (tx, rx) = mpsc::channel();
        if self.tx.send(Request::Signal(signal, tx)).is_ok() {
            let _ = rx.recv();
        }
    }
}

struct Entry {
    ident: Ident,
    start_us: u64,
    /// When the latest complete listing of its children began: every child
    /// it forked before then was in that listing.
    listed_us: u64,
    /// Proven the tree's; otherwise a suspect, never signalled.
    proven: bool,
}

/// A time in which a fork may have been missed, and the unique id of the
/// process that may have forked it: whatever was missed was created after
/// `since`, before `until`, and has a unique id above `above`, and so does
/// whatever it forked before it exited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Window {
    since: u64,
    until: u64,
    above: u64,
    /// Recovery scans in progress for it: while one runs, the window extends
    /// to when the scan read its table.
    open: u32,
}

/// What became of an attempt to take a process.
enum Taken {
    New,
    Already,
    /// Exited (a zombie included) before it could be read or watched.
    Gone,
    /// Not a child of the parent it was listed under, unreadable, or not
    /// watchable.
    No,
}

/// What a scan of processes found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Scanned {
    /// Proven processes taken.
    proven: u32,
    /// Whether every process listed could be read.
    complete: bool,
}

struct Tracker {
    queue: Arc<ProcQueue>,
    /// The program's pid, once started.
    program: Option<i32>,
    /// Unique ids above this were created after the keeper.
    floor: u64,
    /// When the keeper started, in microseconds since the epoch.
    keeper_start_us: u64,
    /// Processes held and not known to have exited, by unique id.
    live: HashMap<u64, Entry>,
    by_pid: HashMap<i32, u64>,
    /// Held processes that exited and may not be reaped yet: pid to the
    /// unique id of the parent that forked them. A zombie keeps its pid until
    /// it is reaped, so a zombie listed under that parent is this one.
    zombies: HashMap<i32, u64>,
    /// Every proven unique id ever taken.
    known: HashSet<u64>,
    /// Every suspect unique id ever taken.
    suspected: HashSet<u64>,
    /// Where a fork may have been missed.
    windows: Vec<Window>,
    freezing: bool,
    /// When the previous wait for events returned.
    last_wait_us: u64,
    my_uid: u32,
    failed: bool,
    saturated: bool,
    /// How deep recovery scans are nested (a loss met during a recovery
    /// starts another).
    depth: u32,
    /// Where the first failed read since the previous answer happened.
    first_hole: Option<&'static str>,
    /// How old each process was when it was taken, in microseconds, for the
    /// measurement in the tests.
    #[cfg(test)]
    ages: Vec<u64>,
    refused: u32,
    read_failures: u32,
    losses: u32,
    recovered: u32,
}

impl Tracker {
    fn new(queue: Arc<ProcQueue>, floor: u64) -> Self {
        let me = rustix::process::getpid().as_raw_nonzero().get();
        Self {
            queue,
            program: None,
            floor,
            // Without it nothing is proven foreign by its group or session.
            keeper_start_us: darwin::start_of(me).ok().flatten().unwrap_or(0),
            live: HashMap::new(),
            by_pid: HashMap::new(),
            zombies: HashMap::new(),
            known: HashSet::new(),
            suspected: HashSet::new(),
            windows: Vec::new(),
            freezing: false,
            last_wait_us: now_us(),
            my_uid: rustix::process::getuid().as_raw(),
            failed: false,
            saturated: false,
            depth: 0,
            first_hole: None,
            #[cfg(test)]
            ages: Vec::new(),
            refused: 0,
            read_failures: 0,
            losses: 0,
            recovered: 0,
        }
    }

    fn run(mut self, rx: &mpsc::Receiver<Request>) {
        let mut events = Vec::new();
        loop {
            loop {
                match rx.try_recv() {
                    Ok(request) => self.serve(request),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => return,
                }
            }
            if self.failed {
                std::thread::sleep(WAIT);
                continue;
            }
            self.collect(WAIT, &mut events);
        }
    }

    fn serve(&mut self, request: Request) {
        match request {
            Request::Start(program, ident, watched) => self.start(program, ident, watched),
            Request::Census(tx) => {
                let _ = tx.send(self.census());
            }
            Request::Freeze(tx) => {
                let _ = tx.send(self.freeze_kill());
            }
            Request::Signal(signal, tx) => {
                self.signal_live(signal);
                self.signal_live(Signal::CONT);
                let _ = tx.send(());
            }
        }
    }

    /// Wait up to `timeout` for events and handle them.
    fn collect(&mut self, timeout: Duration, events: &mut Vec<ProcEvent>) {
        events.clear();
        let waited_from = self.last_wait_us;
        if self.queue.wait(timeout, events).is_err() {
            self.failed = true;
            return;
        }
        let returned = now_us();
        for event in events.drain(..) {
            if event.forked {
                self.forked(event.pid, waited_from);
            }
            if event.exited {
                self.exited(event.pid);
            }
        }
        self.last_wait_us = returned;
    }

    fn start(
        &mut self,
        program: i32,
        ident: Option<Ident>,
        watched: Result<(), rustix::io::Errno>,
    ) {
        self.program = Some(program);
        // Read at once after the exec returned; a program gone by then may
        // have forked something no unique id ties to it.
        let Some(ident) = ident else {
            self.recover(self.keeper_start_us, self.floor);
            return;
        };
        self.known.insert(ident.uniqueid);
        let start_us = match darwin::bsd_info(program) {
            Ok(Some(info)) => info.start_us,
            Ok(None) | Err(_) => self.keeper_start_us,
        };
        match watched {
            Ok(()) => {
                self.live.insert(
                    ident.uniqueid,
                    Entry {
                        ident,
                        start_us,
                        listed_us: start_us,
                        proven: true,
                    },
                );
                self.by_pid.insert(program, ident.uniqueid);
                self.discover(vec![(program, ident.uniqueid)]);
            }
            // It exited before it was watched: its children are known by its
            // unique id.
            Err(rustix::io::Errno::SRCH) => self.lost(start_us, ident.uniqueid),
            Err(_) => self.refused += 1,
        }
    }

    /// Whether `pid` is in the program's session, which proves it the
    /// tree's: the keeper holds the program unreaped, so the session id is
    /// still the program's, and only its descendants can be in it.
    fn in_program_session(&self, pid: i32) -> bool {
        self.program.is_some_and(
            |program| matches!(sys::session_lookup(pid), sys::SessionLookup::In(s) if s == program),
        )
    }

    /// Take `pid` if it is alive and, when `parent` is given, was forked by
    /// the process with that unique id; `proven` says whether that parent was
    /// proven the tree's. A new process is watched and queued for its
    /// children; while freezing a proven one is stopped.
    fn take(
        &mut self,
        pid: i32,
        parent: Option<u64>,
        proven: bool,
        queue: &mut Vec<(i32, u64)>,
    ) -> Taken {
        let ident = match read_new(pid) {
            Ok(Some(ident)) => ident,
            Ok(None) => return Taken::Gone,
            Err(_) => {
                self.hole("take1");
                return Taken::No;
            }
        };
        if parent.is_some_and(|p| p != ident.puniqueid) {
            return Taken::No;
        }
        if self.live.contains_key(&ident.uniqueid) {
            return Taken::Already;
        }
        let proven = proven || self.in_program_session(pid);
        if proven {
            self.known.insert(ident.uniqueid);
        } else {
            self.suspected.insert(ident.uniqueid);
        }
        if self.known.len() + self.suspected.len() > MAX_KNOWN {
            self.saturated = true;
        }
        // The start time only explains forks (see `forked`); one that cannot
        // be read (a setuid program the kernel will not describe) explains
        // none, which is the cautious answer, and is not a hole.
        let start_us = match darwin::bsd_info(pid) {
            Ok(Some(info)) => info.start_us,
            Ok(None) | Err(_) => 0,
        };
        match self.queue.watch(pid) {
            Ok(()) => {}
            // Exited since it was read: its children, if any, are known by
            // its unique id.
            Err(rustix::io::Errno::SRCH) => {
                let since = if start_us > 0 {
                    start_us
                } else {
                    self.keeper_start_us
                };
                self.lost(since, ident.uniqueid);
                return Taken::Gone;
            }
            // Recorded but not followed: a hole either way.
            Err(_) => {
                if proven {
                    self.refused += 1;
                } else {
                    self.hole("take2");
                }
                return Taken::No;
            }
        }
        let listed_us = if start_us > 0 {
            start_us
        } else {
            self.keeper_start_us
        };
        self.live.insert(
            ident.uniqueid,
            Entry {
                ident,
                start_us,
                listed_us,
                proven,
            },
        );
        self.by_pid.insert(pid, ident.uniqueid);
        #[cfg(test)]
        if start_us > 0 {
            self.ages.push(now_us().saturating_sub(start_us));
        }
        if self.freezing && proven {
            let _ = self.send(ident.uniqueid, Signal::STOP);
        }
        queue.push((pid, ident.uniqueid));
        Taken::New
    }

    /// List the children of every queued process, taking each new one, until
    /// the queue is empty.
    fn discover(&mut self, mut queue: Vec<(i32, u64)>) {
        while let Some((pid, uniqueid)) = queue.pop() {
            let _ = self.list_under(pid, uniqueid, &mut queue);
        }
    }

    /// Take the children of the held process `pid`. Returns the start times
    /// of the children listed that are held, new or not, or `None` when the
    /// parent had exited by the end of the listing (a loss, recovered here).
    /// A listed child that exited before it could be read may have forked:
    /// that is a suspect window.
    fn list_under(
        &mut self,
        pid: i32,
        uniqueid: u64,
        queue: &mut Vec<(i32, u64)>,
    ) -> Option<Vec<u64>> {
        let began = now_us();
        let (previous, proven) = self.live.get(&uniqueid).map_or_else(
            || (self.keeper_start_us, self.known.contains(&uniqueid)),
            |e| (e.listed_us, e.proven),
        );
        let kids = match darwin::children(pid) {
            Ok(kids) => kids,
            Err(_) => {
                self.hole("list_under1");
                Vec::new()
            }
        };
        let mut starts = Vec::new();
        let mut vanished = false;
        let mut zombies_seen = Vec::new();
        for kid in kids {
            match self.by_pid.get(&kid).copied() {
                Some(held) if self.live.contains_key(&held) => {}
                _ => {
                    if matches!(self.take(kid, Some(uniqueid), proven, queue), Taken::Gone) {
                        if self.zombies.get(&kid) == Some(&uniqueid) {
                            zombies_seen.push(kid);
                        } else {
                            vanished = true;
                        }
                    }
                }
            }
            if let Some(entry) = self.by_pid.get(&kid).and_then(|u| self.live.get(u))
                && entry.ident.puniqueid == uniqueid
            {
                starts.push(entry.start_us);
            }
        }
        // The parent's zombies not listed any more were reaped.
        self.zombies
            .retain(|pid, parent| *parent != uniqueid || zombies_seen.contains(pid));
        if vanished {
            self.recover(previous, uniqueid);
        }
        let alive = matches!(darwin::identity(pid), Ok(Some(i)) if i.uniqueid == uniqueid);
        if alive {
            if let Some(entry) = self.live.get_mut(&uniqueid) {
                entry.listed_us = began;
            }
            Some(starts)
        } else {
            self.lost(previous, uniqueid);
            None
        }
    }

    /// A fork of the held process `pid`, reported by a wait that began at
    /// `waited_from`: the child was created after that, and after the
    /// latest listing of the parent.
    fn forked(&mut self, pid: i32, waited_from: u64) {
        let Some(uniqueid) = self.by_pid.get(&pid).copied() else {
            return;
        };
        let previous = self.live.get(&uniqueid).map_or(0, |e| e.listed_us);
        let since = waited_from.max(previous);
        let mut queue = Vec::new();
        let listed = self.list_under(pid, uniqueid, &mut queue);
        self.discover(queue);
        // A child created since then explains the fork. Without one, the
        // child exited and was reaped before the listing, and may have
        // forked first.
        match listed {
            Some(starts) if starts.iter().any(|s| *s >= since.saturating_sub(MARGIN_US)) => {}
            Some(_) => self.recover(since, uniqueid),
            // The loss has its window already.
            None => {}
        }
    }

    fn exited(&mut self, pid: i32) {
        if let Some(uniqueid) = self.by_pid.remove(&pid)
            && let Some(entry) = self.live.remove(&uniqueid)
            && self.zombies.len() < MAX_WINDOWS
        {
            self.zombies.insert(pid, entry.ident.puniqueid);
        }
    }

    /// A parent, the process with unique id `above`, exited before all its
    /// children since `since` were listed.
    fn lost(&mut self, since: u64, above: u64) {
        self.losses += 1;
        self.recover(since, above);
    }

    /// A fork by the process with unique id `above` since `since` may have
    /// been missed: open a window for it and run a recovery scan at once,
    /// which takes every process of this user whose parent unique id is
    /// known and every orphan that may come from the missed fork. The window
    /// stays open until the scan has read its table, so whatever the missed
    /// process forked before then is in the table, and whatever it forks
    /// after is forked by a process the scan took.
    fn recover(&mut self, since: u64, above: u64) {
        let index = self.suspect(since.saturating_sub(MARGIN_US), above);
        // A relay can lose one generation after another while the scans
        // run; past this depth the tracker stops chasing and says so.
        if self.depth >= MAX_DEPTH {
            self.hole("recover1");
            return;
        }
        self.depth += 1;
        if let Some(window) = self.windows.get_mut(index) {
            window.open += 1;
        }
        match darwin::user_pids(self.my_uid) {
            Ok(pids) => {
                let _ = self.classify(pids);
            }
            Err(_) => self.hole("recover2"),
        }
        if let Some(window) = self.windows.get_mut(index) {
            window.open = window.open.saturating_sub(1);
        }
        self.depth -= 1;
    }

    /// Record a window from `since` to now in which a process with a unique
    /// id above `above` may have been missed. Returns its index.
    fn suspect(&mut self, since: u64, above: u64) -> usize {
        let until = now_us().saturating_add(MARGIN_US);
        if let Some(last) = self.windows.last_mut()
            && last.above == above
            && since <= last.until
        {
            last.since = last.since.min(since);
            last.until = last.until.max(until);
            return self.windows.len() - 1;
        }
        self.windows.push(Window {
            since,
            until,
            above,
            open: 0,
        });
        if self.windows.len() > MAX_WINDOWS {
            self.saturated = true;
        }
        self.windows.len() - 1
    }

    /// Whether a process cannot be the tree's by its group or session. A
    /// process of the tree is in a group and a session that a process of the
    /// tree created, so each id is the pid of a process that started after
    /// the keeper. If a process alive or a zombie now, which started before
    /// the keeper, has that pid, the two would have held one pid at the same
    /// time, so the group or session is someone else's, and so is the
    /// process.
    fn provably_foreign(&self, pid: i32, pgid: i32) -> bool {
        let session = match sys::session_lookup(pid) {
            sys::SessionLookup::In(session) => session,
            sys::SessionLookup::Gone | sys::SessionLookup::Unknown => 0,
        };
        [pgid, session].into_iter().any(|id| {
            id > 0
                && id != pid
                && matches!(darwin::start_of(id), Ok(Some(start)) if start < self.keeper_start_us)
        })
    }

    /// Read the processes in `pids` that are not held. Take every one forked
    /// by a known process (proven) or by a suspect (a suspect), and their
    /// children, until nothing new is found; then take as a suspect every
    /// orphan that may come from a missed fork.
    fn classify(&mut self, pids: Vec<i32>) -> Scanned {
        let mut scanned = Scanned {
            proven: 0,
            complete: true,
        };
        let mut table = Vec::with_capacity(pids.len());
        // The kernel's record of every process that could be a suspect, read
        // with its identity: it may exit before it is looked at.
        let mut records: HashMap<i32, darwin::KernProc> = HashMap::new();
        // A listing is a snapshot: a process created after it, whose parent
        // in it exited before it was read, would be in neither. So this
        // user's processes are listed again after each round of reads, and
        // the new ones read, until a listing brings nothing new.
        let mut seen: HashSet<i32> = pids.iter().copied().collect();
        let mut pending = pids;
        let mut rounds = 0;
        while !pending.is_empty() {
            for pid in pending.drain(..) {
                if self
                    .by_pid
                    .get(&pid)
                    .is_some_and(|u| self.live.contains_key(u))
                {
                    continue;
                }
                match read_new(pid) {
                    Ok(Some(ident)) => {
                        if ident.puniqueid > self.floor
                            && !self.known.contains(&ident.puniqueid)
                            && !self.suspected.contains(&ident.puniqueid)
                        {
                            match darwin::kern_proc(pid) {
                                Ok(Some(record)) => {
                                    records.insert(pid, record);
                                }
                                Ok(None) => {}
                                Err(_) => {
                                    self.hole("classify1");
                                    scanned.complete = false;
                                }
                            }
                        }
                        table.push(ident);
                    }
                    Ok(None) => {}
                    Err(_) => {
                        self.hole("classify2");
                        scanned.complete = false;
                    }
                }
            }
            rounds += 1;
            match darwin::user_pids(self.my_uid) {
                Ok(again) => {
                    pending = again.into_iter().filter(|p| seen.insert(*p)).collect();
                }
                Err(_) => {
                    self.hole("classify3");
                    scanned.complete = false;
                }
            }
            if rounds >= MAX_ROUNDS && !pending.is_empty() {
                // Processes kept appearing faster than they were read.
                self.hole("classify4");
                scanned.complete = false;
                break;
            }
        }
        let read_at = now_us().saturating_add(MARGIN_US);
        for window in self.windows.iter_mut().filter(|w| w.open > 0) {
            window.until = window.until.max(read_at);
        }
        let unseen = |t: &Self, ident: &Ident| {
            !t.live.contains_key(&ident.uniqueid)
                && !t.known.contains(&ident.uniqueid)
                && !t.suspected.contains(&ident.uniqueid)
        };
        loop {
            let mut queue = Vec::new();
            let mut round = 0;
            for ident in &table {
                if !unseen(self, ident) {
                    continue;
                }
                let proven = self.known.contains(&ident.puniqueid);
                if !proven && !self.suspected.contains(&ident.puniqueid) {
                    continue;
                }
                match self.take(ident.pid, Some(ident.puniqueid), proven, &mut queue) {
                    Taken::New => {
                        round += 1;
                        if proven {
                            scanned.proven += 1;
                        }
                    }
                    // It was the tree's when read (its parent's unique id
                    // is known) and exited since: what it forked is known
                    // by its unique id, and anything that forked in turn
                    // and exited is in a window.
                    Taken::Gone => {
                        if proven {
                            self.known.insert(ident.uniqueid);
                        } else {
                            self.suspected.insert(ident.uniqueid);
                        }
                        self.lost(self.keeper_start_us, ident.uniqueid);
                    }
                    Taken::Already | Taken::No => {}
                }
            }
            self.discover(queue);
            self.recovered += round;
            if round == 0 {
                break;
            }
        }
        // A backstop that needs no unique id: a process in the program's
        // session is the tree's, whatever the tracker saw of its forks.
        let mut queue = Vec::new();
        for ident in &table {
            if !self.live.contains_key(&ident.uniqueid)
                && self.in_program_session(ident.pid)
                && matches!(self.take(ident.pid, None, true, &mut queue), Taken::New)
            {
                scanned.proven += 1;
                self.recovered += 1;
            }
        }
        self.discover(queue);
        let mut queue = Vec::new();
        for ident in &table {
            if !unseen(self, ident) {
                continue;
            }
            // Reparented to launchd (its parent has exited), this user's,
            // started in a window after a process the window names, and not
            // proven someone else's by its group or session.
            let Some(record) = records.get(&ident.pid).copied() else {
                continue;
            };
            let suspect = !record.zombie()
                && record.ppid == 1
                && record.ruid == self.my_uid
                && !self.provably_foreign(ident.pid, record.pgid);
            if !suspect {
                continue;
            }
            match self.take(ident.pid, None, false, &mut queue) {
                Taken::New => self.recovered += 1,
                // It exited since it was read: what it forked is a suspect
                // by its unique id.
                Taken::Gone => {
                    self.suspected.insert(ident.uniqueid);
                    self.recover(record.start_us, ident.uniqueid);
                }
                Taken::Already | Taken::No => {}
            }
        }
        self.discover(queue);
        scanned
    }

    /// The second source: every process on the system.
    fn scan(&mut self) -> Scanned {
        match darwin::all_pids() {
            Ok(pids) => self.classify(pids),
            Err(_) => {
                self.hole("scan1");
                Scanned::default()
            }
        }
    }

    /// Drop held processes that have exited (an exit not yet handled). A
    /// process whose identity cannot be read is kept, and the read counts as
    /// failed.
    fn refresh(&mut self) {
        let mut gone = Vec::new();
        let mut failures = 0;
        for entry in self.live.values() {
            match darwin::identity(entry.ident.pid) {
                Ok(Some(now)) if now.uniqueid == entry.ident.uniqueid => {}
                Ok(_) => gone.push(entry.ident.pid),
                Err(_) => failures += 1,
            }
        }
        if failures > 0 {
            self.hole("refresh");
            self.read_failures += failures - 1;
        }
        for pid in gone {
            self.exited(pid);
        }
    }

    fn live_proven(&self) -> BTreeSet<u64> {
        self.live
            .iter()
            .filter(|(_, e)| e.proven)
            .map(|(u, _)| *u)
            .collect()
    }

    fn live_suspects(&self) -> u32 {
        u32::try_from(self.live.values().filter(|e| !e.proven).count()).unwrap_or(u32::MAX)
    }

    /// The answer to a request, with the holes counted since the previous
    /// answer (here and in the background), which it then clears: each hole
    /// is reported once, and the owner keeps the run unproven from then on.
    /// A tracker that failed or saturated reports a hole in every answer.
    fn report(&mut self, live: usize, passes: u32, complete: bool) -> Report {
        let report = Report {
            live,
            passes,
            complete,
            refused: self.refused,
            read_failures: self.read_failures + u32::from(self.failed || self.saturated),
            losses: self.losses,
            recovered: self.recovered,
            ambiguous: self.live_suspects(),
            hole: if self.broken() {
                Some(if self.failed { "kevent" } else { "saturated" })
            } else {
                self.first_hole
            },
        };
        self.first_hole = None;
        self.refused = 0;
        self.read_failures = 0;
        self.losses = 0;
        self.recovered = 0;
        report
    }

    /// Count a failed read, and remember where the first one since the
    /// previous answer happened, for the answer's details.
    fn hole(&mut self, site: &'static str) {
        self.read_failures += 1;
        if self.first_hole.is_none() {
            self.first_hole = Some(site);
        }
    }

    /// The holes counted so far, to tell whether a pass added any.
    fn holes(&self) -> (u32, u32) {
        (self.refused, self.read_failures)
    }

    fn broken(&self) -> bool {
        self.failed || self.saturated
    }

    /// Handle whatever events are pending, without waiting.
    fn drain(&mut self) {
        let mut events = Vec::new();
        self.collect(Duration::ZERO, &mut events);
    }

    /// How many proven processes the tree holds. Complete only when two
    /// passes in a row hold the same live set, the second source took no new
    /// proven process, and neither pass met a refusal or a failed read. A
    /// tree that keeps changing never meets that. Live suspects are reported
    /// beside it.
    fn census(&mut self) -> Report {
        let (live, passes, complete) = self.census_passes();
        self.report(live, passes, complete)
    }

    /// The passes of [`Tracker::census`]: `(live, passes, complete)`, with
    /// `live` never zero when not complete.
    fn census_passes(&mut self) -> (usize, u32, bool) {
        let mut previous: Option<BTreeSet<u64>> = None;
        let mut last = 0;
        for pass in 1..=KEEPER_FREEZE_PASSES {
            let before = self.holes();
            self.drain();
            self.refresh();
            let scanned = self.scan();
            self.refresh();
            let set = self.live_proven();
            last = set.len();
            let sound =
                scanned.complete && scanned.proven == 0 && self.holes() == before && !self.broken();
            if sound && previous.as_ref() == Some(&set) {
                return (set.len(), pass, true);
            }
            previous = sound.then_some(set);
            std::thread::sleep(Duration::from_millis(2));
        }
        (last.max(1), KEEPER_FREEZE_PASSES, false)
    }

    /// Signal the held proven process with this unique id, by token. A stale
    /// pid version (an exec since it was read) is refreshed and the signal
    /// sent again, only while the pid still has this unique id. A process
    /// found gone is dropped; a refusal is counted. A suspect is never
    /// signalled.
    fn send(&mut self, uniqueid: u64, signal: Signal) -> Sent {
        for _ in 0..4 {
            let Some(ident) = self
                .live
                .get(&uniqueid)
                .filter(|e| e.proven)
                .map(|e| e.ident)
            else {
                return Sent::Gone;
            };
            match darwin::signal(&ident, signal) {
                Sent::Delivered => return Sent::Delivered,
                Sent::Refused => {
                    self.refused += 1;
                    return Sent::Refused;
                }
                Sent::Gone => match darwin::identity(ident.pid) {
                    Ok(Some(now)) if now.uniqueid == uniqueid => {
                        if let Some(entry) = self.live.get_mut(&uniqueid) {
                            entry.ident.version = now.version;
                        }
                    }
                    Ok(_) => {
                        self.exited(ident.pid);
                        return Sent::Gone;
                    }
                    Err(_) => {
                        self.hole("send1");
                        return Sent::Refused;
                    }
                },
            }
        }
        // It kept exec'ing between the read and the signal.
        self.refused += 1;
        Sent::Refused
    }

    fn signal_live(&mut self, signal: Signal) {
        for uniqueid in self.live_proven() {
            let _ = self.send(uniqueid, signal);
        }
    }

    /// Stop every proven process held, and every one found from now on,
    /// until a pass proves the tree frozen: everything proven shows stopped,
    /// the second source took no new proven process, nothing was refused and
    /// no read failed. Then SIGKILL and SIGCONT by token, and a complete
    /// census must find nothing proven alive. Bounded by
    /// `KEEPER_FREEZE_PASSES` passes and one second; a freeze that runs out
    /// is not complete, and its count is never zero. Suspects are not
    /// signalled and are reported as ambiguous.
    fn freeze_kill(&mut self) -> Report {
        let deadline = Instant::now() + Duration::from_secs(1);
        let refused_before = self.refused;
        self.freezing = true;
        if let Some(program) = self.program {
            // The keeper holds the program unreaped, so its pid is its group.
            let _ = sys::signal_group(program, Signal::STOP);
        }
        self.signal_live(Signal::STOP);
        let mut passes = 0;
        let mut frozen = false;
        while passes < KEEPER_FREEZE_PASSES {
            passes += 1;
            let before = self.holes();
            self.drain();
            self.refresh();
            let mut all_stopped = true;
            for uniqueid in self.live_proven() {
                let Some(pid) = self.live.get(&uniqueid).map(|e| e.ident.pid) else {
                    continue;
                };
                match darwin::bsd_info(pid) {
                    Ok(Some(info)) if info.stopped() => {}
                    Ok(Some(_)) => {
                        all_stopped = false;
                        let _ = self.send(uniqueid, Signal::STOP);
                    }
                    Ok(None) => self.exited(pid),
                    Err(_) => {
                        all_stopped = false;
                        self.hole("freeze_kill1");
                    }
                }
            }
            // The pid may name another process by now; only one that still
            // has the identity held counts as stopped.
            self.refresh();
            let scanned = self.scan();
            if all_stopped
                && scanned.complete
                && scanned.proven == 0
                && self.holes() == before
                && !self.broken()
            {
                frozen = true;
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        self.signal_live(Signal::KILL);
        self.signal_live(Signal::CONT);
        // A refusal anywhere in this freeze leaves a process that was never
        // stopped: the freeze is not proven, whatever later passes found.
        let refused_here = self.refused != refused_before;
        let until = Instant::now() + Duration::from_millis(500);
        loop {
            let (live, _, counted) = self.census_passes();
            let complete = frozen && !refused_here && counted;
            if (complete && live == 0) || Instant::now() >= until {
                let live = if complete { live } else { live.max(1) };
                return self.report(live, passes, complete);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker() -> Tracker {
        Tracker::new(Arc::new(ProcQueue::new().expect("a kqueue")), 0)
    }

    #[test]
    fn an_unproven_report_is_never_zero() {
        let report = Report {
            live: 0,
            complete: false,
            ..Report::default()
        };
        assert_eq!(report.effective_live(), 1);
        assert!(report.detail().contains("ambiguous=0"));
    }

    #[test]
    fn suspect_windows_merge_and_bound_the_parent_and_the_start() {
        let mut t = tracker();
        let start = now_us();
        let _ = t.suspect(start, 100);
        let _ = t.suspect(start + 10, 100);
        assert_eq!(t.windows.len(), 1);
        assert!(t.windows[0].since <= start && t.windows[0].until >= start + 10);
    }

    #[test]
    fn a_child_of_a_held_process_is_taken_and_an_unrelated_process_is_not() {
        use crate::test_spawn::Guarded;
        let mut t = tracker();
        let me = rustix::process::getpid().as_raw_nonzero().get();
        let mut queue = Vec::new();
        assert!(matches!(t.take(me, None, true, &mut queue), Taken::New));
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("5")
            .guarded_spawn()
            .expect("start a child");
        let pid = i32::try_from(child.id()).expect("a pid");
        let own = darwin::identity(me).unwrap().unwrap();
        let mut queue = Vec::new();
        let listed = t.list_under(me, own.uniqueid, &mut queue);
        // launchd was not forked by this process.
        let launchd = t.take(1, Some(own.uniqueid), true, &mut queue);
        let _ = child.kill();
        let _ = child.wait();
        assert!(listed.is_some());
        assert!(t.by_pid.contains_key(&pid));
        assert!(t.live.values().all(|e| e.proven));
        assert!(matches!(launchd, Taken::No));
    }

    /// How old each generation of a fork relay is when the tracker takes
    /// it, and what the recovery and second-source scans cost, with the
    /// tracker driven on this thread. Run with `cargo test --release --
    /// --ignored measure_the_tracker --nocapture`.
    #[test]
    #[ignore = "a measurement, not a check"]
    fn measure_the_tracker() {
        use crate::test_spawn::Guarded;
        let mut t = tracker();
        let script = "use POSIX; my $p = fork(); if ($p == 0) { \
            for (1..300) { setpgid(0,0); select(undef, undef, undef, 0.005); \
            my $q = fork(); if ($q) { exit 0 } } exit 0 } sleep 3";
        let mut child = std::process::Command::new("/usr/bin/perl")
            .args(["-e", script])
            .guarded_spawn()
            .expect("start perl");
        let pid = i32::try_from(child.id()).expect("a pid");
        let ident = darwin::identity(pid).ok().flatten();
        let watched = t.queue.watch(pid);
        t.start(pid, ident, watched);
        let until = Instant::now() + Duration::from_secs(3);
        let mut events = Vec::new();
        while Instant::now() < until {
            t.collect(Duration::from_millis(10), &mut events);
        }
        let losses = t.losses;
        let mut scans = Vec::new();
        for _ in 0..20 {
            let began = Instant::now();
            let _ = t.scan();
            scans.push(began.elapsed());
        }
        let mut recoveries = Vec::new();
        for _ in 0..20 {
            let began = Instant::now();
            let pids = darwin::user_pids(t.my_uid).expect("listed");
            let _ = t.classify(pids);
            recoveries.push(began.elapsed());
        }
        let _ = child.kill();
        let _ = child.wait();
        let mut ages = t.ages.clone();
        ages.sort_unstable();
        scans.sort();
        recoveries.sort();
        let at = |q: usize| ages[(ages.len() * q / 100).min(ages.len() - 1)];
        let processes = darwin::all_pids().map(|p| p.len()).unwrap_or(0);
        eprintln!(
            "taken {}, losses {losses}; age at take p50 {} us, p90 {} us, p99 {} us, max {} us; \
             second-source scan median {:?}, recovery scan median {:?}, at {processes} processes",
            ages.len(),
            at(50),
            at(90),
            at(99),
            ages[ages.len() - 1],
            scans[10],
            recoveries[10]
        );
    }

    #[test]
    fn a_suspect_is_never_signalled() {
        use crate::test_spawn::Guarded;
        let mut t = tracker();
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("5")
            .guarded_spawn()
            .expect("start a child");
        let pid = i32::try_from(child.id()).expect("a pid");
        let mut queue = Vec::new();
        let taken = t.take(pid, None, false, &mut queue);
        let uniqueid = t.by_pid.get(&pid).copied();
        let sent = uniqueid.map(|u| t.send(u, Signal::KILL));
        t.signal_live(Signal::KILL);
        let alive = darwin::identity(pid).unwrap().is_some();
        let _ = child.kill();
        let _ = child.wait();
        assert!(matches!(taken, Taken::New));
        assert_eq!(sent, Some(Sent::Gone));
        assert!(alive, "a suspect was signalled");
        assert_eq!(t.live_suspects(), 1);
    }
}
