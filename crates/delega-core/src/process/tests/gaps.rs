//! The descendants that escape, and the ones that no longer do.
//!
//! A descendant in its own process group but the child's session is found by
//! the session sweep, so the first two tests are ordinary regression tests. A
//! descendant that calls `setsid` leaves the session; it is kept when a listing
//! saw it while its parent was a member of the child's session or group or was
//! already tracked, which includes the listing taken when the leader exits, so
//! the next group of tests (a `setsid` descendant with a live parent when the
//! leader exits, ends with an error or crashes, and that no foreign process is
//! signalled) are ordinary tests too.
//!
//! What remains are descendants no process table can follow: one whose parent
//! exits before any listing saw it (its parent was the leader itself, or a
//! member that exited at once), and a fast fork relay, which is never in a
//! listing for long enough to be signalled. Three tests state the wanted
//! behaviour and are ignored because they fail. The first two of them start
//! their descendant after the early listings (`CAPTURE_RAMP`), which see a
//! descendant started sooner, and end the leader before the first periodic
//! listing. Two regression tests cover what the ramp closed: a `setsid`
//! descendant started just after spawn whose leader crashes before the first
//! periodic listing, and the schedule of the ramp itself. Run them with
//! `cargo test -- --ignored`. They leave one process running when they fail,
//! so each stops it itself.

use std::sync::Arc;
use std::time::Duration;

use super::fixtures::{
    AWAIT, Heard, Lifeline, OWN_GROUP, OWN_SESSION, Reaper, Record, Scratch, assert_gone,
    await_held, await_record, is_running, is_running_async, list_off_the_clock, own_group_recorded,
    own_session_recorded, read_record, run_guarded, sh,
};
use crate::contract::result::Cleanup;
use crate::process::Ending;
use crate::process::tree::{Lister, list_processes};
use crate::test_spawn::Guarded;

use super::keeper::{kept, own_session_sleeper};

const LONG: Duration = Duration::from_secs(60);

/// A lister that reads the real table and then drops a marker file, so a
/// script can start its descendant only after the first scan has finished.
/// That puts the descendant after the scan and before the next one.
pub(super) fn marking_lister(marker: std::path::PathBuf) -> Lister {
    Arc::new(move || {
        let marker = marker.clone();
        Box::pin(async move {
            let rows = list_processes().await;
            let _ = std::fs::write(&marker, "scanned");
            rows
        })
    })
}

const WAIT_FOR_SCAN: &str = "while [ ! -e scanned ]; do sleep 0.02; done;";

/// Wait in a lister until `path` exists, for at most [`AWAIT`]; past that the
/// listing goes ahead and the test fails on what it then sees.
async fn listing_waits_for(path: &std::path::Path) {
    let began = std::time::Instant::now();
    while !path.exists() && began.elapsed() < AWAIT {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The listings of a run whose leader (`leader`) starts a descendant that
/// publishes `gc` itself once it has left the session, put in an order that no
/// schedule of listings and no load can change.
///
/// - The first listing reads the table and writes `scanned`; the scripts start
///   the descendant only after it ([`WAIT_FOR_SCAN`]).
/// - With `witness`, the second listing, the first of `CAPTURE_RAMP`, waits
///   until `gc` is published and then reads the table. The test lets the
///   leader end only once the owner tracks the descendant, so this listing
///   sees the descendant while the leader is its parent, and the owner has
///   recorded what it saw before the leader goes. (On Linux the owner reads
///   the descendant's start time from `/proc` after the listing returns, and
///   accepts it only while the descendant's parent is still the leader; a
///   leader that ended as soon as the listing returned would leave a
///   descendant the owner may not signal.)
/// - Every other listing waits until `gc` is published, then reads the table
///   until the leader is no longer a live process in it, and answers with that
///   reading. None of them can see the descendant with the leader as its
///   parent: the ramp and periodic listings are held, not raced. The listing
///   taken when the leader ends is one of them and goes ahead at once.
///
/// With `hide_group_leaders`, a reading taken once the leader is recorded
/// shows no group leader but the leader (see the test that uses it).
fn ordered_listings(s: &Scratch, witness: bool, hide_group_leaders: bool) -> Lister {
    let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let (scanned, gc, leader) = (s.path("scanned"), s.path("gc"), s.path("leader"));
    Arc::new(move || {
        let call = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (scanned, gc, leader) = (scanned.clone(), gc.clone(), leader.clone());
        Box::pin(async move {
            let hide = |rows: Vec<crate::process::tree::ProcessRow>| match read_record(&leader) {
                Record::Pid(leader) if hide_group_leaders => rows
                    .into_iter()
                    .filter(|r| r.group != r.pid || r.pid == leader)
                    .collect(),
                _ => rows,
            };
            if call == 0 {
                let rows = list_processes().await;
                let _ = std::fs::write(&scanned, "scanned");
                return rows.map(hide);
            }
            listing_waits_for(&gc).await;
            if call == 1 && witness {
                return list_processes().await.map(hide);
            }
            let began = std::time::Instant::now();
            loop {
                let rows = list_processes().await;
                let leader_live = match (&rows, read_record(&leader)) {
                    (Ok(rows), Record::Pid(leader)) => {
                        rows.iter().any(|r| r.pid == leader && !r.zombie)
                    }
                    _ => false,
                };
                if !leader_live || began.elapsed() >= AWAIT {
                    return rows.map(hide);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
    })
}

/// The wanted behaviour: when a descendant is left running, the result must not
/// claim cleanup is confirmed.
fn assert_no_false_confirmation(descendant: i32, cleanup: &Cleanup) {
    super::fixtures::assert_documented(cleanup);
    let alive = is_running(descendant);
    assert!(
        !(alive && *cleanup == Cleanup::Confirmed),
        "descendant {descendant} is still running but cleanup says Confirmed"
    );
}

#[tokio::test]
async fn an_own_group_grandchild_whose_parent_exits_at_once_is_cleaned_up() {
    require_perl!();
    let s = Scratch::new();
    let script = format!("{WAIT_FOR_SCAN} record leader $$; {OWN_GROUP} & record gc $!; exit 0");
    let mut spec = sh(&s, &script, LONG).with_lister(marking_lister(s.path("scanned")));
    spec.capture_interval = Some(Duration::from_secs(3600));
    let (report, pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
    assert_eq!(report.cleanup, Cleanup::Confirmed, "{report:#?}");
    super::fixtures::assert_gone("grandchild", &pids).await;
}

#[tokio::test]
async fn an_own_group_descendant_started_between_two_scans_before_a_crash_is_cleaned_up() {
    require_perl!();
    let s = Scratch::new();
    // The crash comes once the descendant has said it left the group. No
    // periodic scan runs after the first, however slow the machine is.
    let script = format!(
        "{WAIT_FOR_SCAN} record leader $$; {} & {} kill -KILL $$; sleep 5",
        own_group_recorded("gc"),
        await_record("gc")
    );
    let mut spec = sh(&s, &script, LONG).with_lister(marking_lister(s.path("scanned")));
    spec.capture_interval = Some(Duration::from_secs(3600));
    let (report, pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
    assert_eq!(report.cleanup, Cleanup::Confirmed, "{report:#?}");
    super::fixtures::assert_gone("descendant", &pids).await;
}

/// Pins the schedule itself. The run's clock is paused from before the spawn,
/// and a lister records when each listing starts and ends on that clock. Each
/// listing is taken on a thread of the blocking pool ([`list_off_the_clock`]),
/// which keeps the paused clock still while it runs, so a listing takes no
/// time on the run's clock however slow `ps` is, and between listings the
/// clock moves only to the next timer. The child waits for the test's `go`,
/// so it is alive through the schedule. The listings must then start at
/// exactly the spawn, each ramp offset from spawn (50, 100, 200 ms), and one
/// periodic interval (500 ms) after the last ramp listing: a missing ramp step
/// would put the next listing 500 ms after the previous one, a shortened or
/// early one would start before its offset.
#[tokio::test]
async fn each_ramp_listing_starts_once_due_and_before_the_first_periodic_listing() {
    use crate::process::ProcessHandle;
    use std::sync::Mutex;
    use tokio::time::Instant;

    let s = Scratch::new();
    let spans: Arc<Mutex<Vec<(Instant, Instant)>>> = Arc::new(Mutex::new(Vec::new()));
    let (seen, mut listed) = tokio::sync::watch::channel(0usize);
    let recorder = spans.clone();
    let seen = Arc::new(seen);
    let lister: Lister = Arc::new(move || {
        let recorder = recorder.clone();
        let seen = seen.clone();
        Box::pin(async move {
            let started = Instant::now();
            let listing = list_off_the_clock().await;
            let count = {
                let mut spans = recorder.lock().expect("lock spans");
                spans.push((started, Instant::now()));
                spans.len()
            };
            seen.send_replace(count);
            listing
        })
    });
    let spec = sh(&s, "until [ -e go ]; do sleep 0.01; done; exit 0", LONG).with_lister(lister);
    tokio::time::pause();
    let spawned = Instant::now();
    let handle = ProcessHandle::start(spec);
    // Nothing real is awaited here: the listings hold the clock while they
    // run, and the child waits for `go`. The clock moves from timer to timer
    // until the first periodic listing has been taken.
    listed
        .wait_for(|count| *count >= 5)
        .await
        .expect("the lister is alive");
    tokio::time::resume();
    std::fs::write(s.path("go"), "").expect("let the child exit");
    let report = tokio::time::timeout(Duration::from_secs(120), handle.wait())
        .await
        .expect("the run did not end after the child was let go");
    assert_eq!(report.ending, Ending::Exited { code: 0 });
    assert_eq!(report.cleanup, Cleanup::Confirmed, "{report:#?}");
    let mut spans = spans.lock().expect("lock spans").clone();
    spans.sort_by_key(|(started, _)| *started);
    let offsets: Vec<(Duration, Duration)> = spans
        .iter()
        .take(5)
        .map(|(a, b)| (*a - spawned, *b - spawned))
        .collect();
    // Listing 0 is the one at spawn, then the ramp, then the first periodic
    // listing, due one interval after the last ramp listing ended. The
    // offsets are written out here, not read from `CAPTURE_RAMP` or
    // `CAPTURE_INTERVAL`, so that changing the schedule fails the test.
    // Tokio's timer wheel rounds a deadline up to its next millisecond tick,
    // so a listing starts at its due time or within one tick after it.
    const TICK: Duration = Duration::from_millis(1);
    assert!(
        offsets.iter().all(|(start, end)| start == end),
        "a listing took time on the run's clock: {offsets:?}"
    );
    assert_eq!(offsets.len(), 5, "{offsets:?}");
    assert_eq!(
        offsets[0].0,
        Duration::ZERO,
        "the listing at spawn: {offsets:?}"
    );
    let periodic_due = offsets[3].1 + Duration::from_millis(500);
    for (step, due) in [
        (1, Duration::from_millis(50)),
        (2, Duration::from_millis(100)),
        (3, Duration::from_millis(200)),
        (4, periodic_due),
    ] {
        let start = offsets[step].0;
        assert!(
            due <= start && start <= due + TICK,
            "listing {step} started at {start:?}, due at {due:?}: {offsets:?}"
        );
    }
}

/// The periodic interval is the default here, so the listings are the one at
/// spawn, the early ramp (`CAPTURE_RAMP`) and then the 500 ms interval. The
/// descendant calls `setsid` straight after the first listing. The first ramp
/// listing is held until it has; the test waits until the owner tracks the
/// descendant and then lets the leader crash (`go`); every later listing is
/// held until the leader is gone ([`ordered_listings`]). So that ramp
/// listing, and no other, sees the descendant while the leader is its parent,
/// and what it saw must be enough to stop the descendant after the crash. That the ramp listing comes
/// before the first periodic one is a matter of the schedule, which
/// `each_ramp_listing_starts_once_due_and_before_the_first_periodic_listing`
/// pins.
#[tokio::test]
async fn a_setsid_descendant_started_just_after_spawn_is_cleaned_up_when_the_leader_crashes_before_the_first_periodic_listing()
 {
    require_perl!();
    let s = Scratch::new();
    let script = format!(
        "{WAIT_FOR_SCAN} record leader $$; {} >/dev/null 2>&1 & {} kill -KILL $$; sleep 5",
        own_session_recorded("gc"),
        await_record("go")
    );
    let spec = sh(&s, &script, LONG).with_lister(ordered_listings(&s, true, false));
    let handle = crate::process::ProcessHandle::start(spec);
    let mut reaper = Reaper::default();
    let gc = reaper.watch(&s, "gc").await;
    super::fixtures::wait_tracked(&handle, gc).await;
    std::fs::write(s.path("go"), "go").expect("write the go file");
    let report = handle.wait().await;
    assert_no_false_confirmation(gc, &report.cleanup);
    assert_eq!(report.cleanup, Cleanup::Confirmed, "{report:#?}");
    assert_gone("descendant", &[gc]).await;
}

/// Gap 1 without a keeper: a `setsid` descendant whose parent, the leader,
/// exits before any listing saw the two together. With a keeper it is found
/// (Linux: it becomes the keeper's child; elsewhere: it carries the run
/// marker), stopped, and the verdict is `Confirmed` only once it is gone. The
/// descendant runs this test binary, not `sleep`: on macOS `ps -E` shows no
/// environment for Apple's own programs, so an orphan running one of them is
/// the documented residual there. No owner listing after the first can see
/// the descendant with the leader alive ([`ordered_listings`]).
#[tokio::test]
async fn a_setsid_grandchild_whose_parent_exits_at_once_is_stopped_in_a_kept_run() {
    require_perl!();
    let s = Scratch::new();
    let script = format!(
        "{WAIT_FOR_SCAN} record leader $$; {} >/dev/null 2>&1 & {} exit 0",
        own_session_sleeper("gc"),
        await_record("gc")
    );
    let spec = kept(sh(&s, &script, LONG).with_lister(ordered_listings(&s, false, false)));
    let (report, pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
    assert_no_false_confirmation(pids[0], &report.cleanup);
    assert_eq!(report.cleanup, Cleanup::Confirmed, "{report:#?}");
    assert_gone("orphaned setsid descendant", &pids).await;
}

/// The descendant leaves the session after the first listing and the leader
/// crashes once it has; every owner listing after the first is held until the
/// leader is gone ([`ordered_listings`]), so none sees the two together and
/// only the keeper can find the descendant.
#[tokio::test]
async fn a_setsid_descendant_started_between_two_scans_before_a_crash_is_stopped_in_a_kept_run() {
    require_perl!();
    let s = Scratch::new();
    let script = format!(
        "{WAIT_FOR_SCAN} record leader $$; {} >/dev/null 2>&1 & {} kill -KILL $$; sleep 5",
        own_session_sleeper("gc"),
        await_record("gc")
    );
    let spec = kept(sh(&s, &script, LONG).with_lister(ordered_listings(&s, false, false)));
    let (report, pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
    assert_no_false_confirmation(pids[0], &report.cleanup);
    assert_eq!(report.cleanup, Cleanup::Confirmed, "{report:#?}");
    assert_gone("orphaned setsid descendant", &pids).await;
}

/// Gap 1 when the descendant also clears its environment: on Linux the keeper
/// is its reaper and it is stopped; on macOS it carries no marker and no
/// listing ties it to the run, the documented residual, so only the absence of
/// a false confirmation could be asked there, and even that is not promised.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn an_orphan_that_clears_its_environment_is_stopped_by_the_keeper_on_linux() {
    require_perl!();
    let s = Scratch::new();
    let detach = format!(
        "perl -e 'use POSIX; {} {} %ENV = (); \
         exec {{ q(/bin/sleep) }} q(sleep), q(60)'",
        super::fixtures::PERL_LEAVE_SESSION,
        super::fixtures::perl_record_self("gc")
    );
    let script = format!(
        "{WAIT_FOR_SCAN} record leader $$; {detach} >/dev/null 2>&1 & {} exit 0",
        await_record("gc")
    );
    // The owner's listings never show it: once the leader is known they hide
    // every group leader but the leader, which the orphan is from its
    // `setsid` on, and no listing after the first sees it with the leader
    // alive ([`ordered_listings`]). Only the keeper's own walk of its tree
    // can find it, so a broken walk ends Confirmed with the orphan alive and
    // fails here.
    let spec = kept(sh(&s, &script, LONG).with_lister(ordered_listings(&s, false, true)));
    let (report, pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
    assert_no_false_confirmation(pids[0], &report.cleanup);
    assert_eq!(report.cleanup, Cleanup::Confirmed, "{report:#?}");
    assert_gone("orphan without the marker", &pids).await;
}

/// A middle process that stays alive in the child's session and group and
/// starts a descendant that calls `setsid` and then writes its own pid to `gc`.
/// The descendant's parent is the middle process, not the leader.
const MIDDLE: &str = "perl -e 'use POSIX; POSIX::setsid() == $$ or die; \
    open F, q(>), q(gc.tmp); print F qq($$\\n); close F; rename q(gc.tmp), q(gc); \
    exec q(sleep), q(60)' &\nwait\n";

/// The script of a leader that starts the middle process after the first scan
/// and ends as soon as the descendant has left the session, which is before the
/// next scan. No periodic scan can have seen the descendant with its parent
/// alive, so only the listing taken when the leader exits can.
fn leader_with_a_live_middle_process(s: &Scratch, ending: &str) -> crate::process::ProcessSpec {
    std::fs::write(s.path("middle.sh"), MIDDLE).expect("write the middle script");
    let script = format!(
        "{WAIT_FOR_SCAN} record leader $$; sh middle.sh & \
         while [ ! -e gc ]; do sleep 0.02; done; {ending}"
    );
    let mut spec = sh(s, &script, LONG).with_lister(marking_lister(s.path("scanned")));
    // After the first scan no periodic scan runs, however slow the machine is.
    spec.capture_interval = Some(Duration::from_secs(3600));
    spec
}

async fn assert_descendant_and_middle_are_gone(s: &Scratch, descendant: i32) {
    let leader = s.pid_now("leader");
    assert_gone("descendant that left the session", &[descendant]).await;
    // The middle process is the leader's child: whatever it was, it is gone.
    assert!(!is_running(leader));
}

#[tokio::test]
async fn a_setsid_descendant_with_a_live_parent_is_cleaned_up_when_the_leader_exits_normally() {
    require_perl!();
    let s = Scratch::new();
    let spec = leader_with_a_live_middle_process(&s, "exit 0");
    let (report, pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
    assert_eq!(report.ending, Ending::Exited { code: 0 }, "{report:#?}");
    assert_eq!(report.cleanup, Cleanup::Confirmed, "{report:#?}");
    assert_descendant_and_middle_are_gone(&s, pids[0]).await;
}

#[tokio::test]
async fn a_setsid_descendant_with_a_live_parent_is_cleaned_up_when_the_leader_exits_with_an_error()
{
    require_perl!();
    let s = Scratch::new();
    let spec = leader_with_a_live_middle_process(&s, "exit 3");
    let (report, pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
    assert_eq!(report.ending, Ending::Exited { code: 3 }, "{report:#?}");
    assert_eq!(report.cleanup, Cleanup::Confirmed, "{report:#?}");
    assert_descendant_and_middle_are_gone(&s, pids[0]).await;
}

#[tokio::test]
async fn a_setsid_descendant_with_a_live_parent_is_cleaned_up_when_the_leader_crashes() {
    require_perl!();
    let s = Scratch::new();
    let spec = leader_with_a_live_middle_process(&s, "kill -KILL $$; sleep 5");
    let (report, pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
    assert!(
        matches!(report.ending, Ending::Signaled { signal: 9, .. }),
        "{report:#?}"
    );
    assert_eq!(report.cleanup, Cleanup::Confirmed, "{report:#?}");
    assert_descendant_and_middle_are_gone(&s, pids[0]).await;
}

/// A process the test starts that is not a descendant of the child, as a pid
/// the test must see alive at the end.
fn foreign(kind: &str) -> i32 {
    // `kind` is a shell line that prints the pid of the process it leaves.
    let out = std::process::Command::new("/bin/sh")
        .args(["-c", kind])
        .guarded_output()
        .expect("start a foreign process");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .expect("the foreign process printed its pid")
}

#[tokio::test]
async fn processes_outside_the_childs_tree_are_never_adopted_or_signalled() {
    require_perl!();
    // Five processes that belong to the test: a plain background process, one
    // in a session of its own, and an orphan of each kind (reparented to pid 1,
    // with and without its own session). The run beside them adopts a setsid
    // descendant of its own, so the adoption rule is exercised while they are
    // in the process table. None of them may be signalled.
    let alive = foreign("sleep 60 </dev/null >/dev/null 2>&1 & echo $!");
    let own_session = foreign(&format!(
        "{OWN_SESSION} </dev/null >/dev/null 2>&1 & echo $!"
    ));
    let orphan = foreign("(sleep 60 </dev/null >/dev/null 2>&1 & echo $!)");
    let orphan_own_session = foreign(&format!(
        "({OWN_SESSION} </dev/null >/dev/null 2>&1 & echo $!)"
    ));
    // And a process whose parent is the test process itself.
    let mut own_child = std::process::Command::new("sleep")
        .arg("60")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .guarded_spawn()
        .expect("start a child of the test");
    let child_of_test = i32::try_from(own_child.id()).expect("a pid");
    let foreigners = vec![
        alive,
        own_session,
        orphan,
        orphan_own_session,
        child_of_test,
    ];
    let _foreign_reaper = Reaper::new(foreigners.clone());
    for pid in &foreigners {
        assert!(is_running(*pid), "foreign process {pid} did not start");
    }

    // Confirmed depends on the exit-time listing seeing the detached
    // descendant before its middle parent exits; a late or failed exit listing
    // leaves cleanup Unconfirmed.
    let s = Scratch::new();
    let spec = leader_with_a_live_middle_process(&s, "exit 0");
    let (report, pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
    assert_eq!(report.cleanup, Cleanup::Confirmed, "{report:#?}");
    assert_descendant_and_middle_are_gone(&s, pids[0]).await;

    for pid in &foreigners {
        assert!(
            is_running(*pid),
            "foreign process {pid} was signalled: {report:#?}"
        );
    }
    // Nothing in the report names a foreign pid either.
    let tracked = report.diagnostics.session_members_swept.clone();
    for pid in &foreigners {
        assert!(!tracked.contains(&(*pid as u32)), "{tracked:?}");
    }
    let _ = own_child.kill();
    let _ = own_child.wait();
}

/// A relay: each process ignores TERM, leads a group of its own, lives a few
/// milliseconds, forks a successor and exits, so no process stays alive for
/// longer than one listing. It stops itself after 300 generations.
///
/// What a test can see of it ([`RelayLife`]): every generation checks that it
/// leads its own group, publishes its pid in `beat` as a complete record
/// (renamed into place), and then writes `h` on the case's lifeline (`life`,
/// a [`Lifeline`] the test opened). It writes `h` once: a generation whose
/// `setpgid` or successor `fork` fails writes `x` and exits, instead of going
/// round the loop again. The last generation writes `e` before it exits by
/// itself. The perl process opens the lifeline before its first fork, so it
/// and every generation hold it: the lifeline ends exactly when the last of
/// them is gone. A second `h` is one completed handoff; an `h` read after a
/// moment is a generation that ran after it; the end of the lifeline without
/// an `e` or an `x` is a relay that something stopped.
const RELAY: &str = "exec perl -e 'use POSIX; \
    open L, q(>), q(life) or die; \
    my $p = fork(); defined $p or die; \
    if ($p == 0) { $SIG{TERM} = q(IGNORE); \
      for (1..300) { setpgid(0, 0); getpgrp() == $$ or do { syswrite L, q(x); exit 1 }; \
        open H, q(>), q(beat.tmp); print H qq($$\n); close H; rename q(beat.tmp), q(beat); \
        syswrite L, q(h); \
        select(undef, undef, undef, 0.005); \
        my $q = fork(); defined $q or do { syswrite L, q(x); exit 1 }; \
        if ($q) { exit 0 } } \
      syswrite L, q(e); \
      exit 0 } \
    sleep 60'";

/// What became of a relay after a moment the test chose (see
/// [`RelayLife::fate_by`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RelayFate {
    /// Every relay process is gone, and the relay never reached its end by
    /// itself or broke: something stopped it.
    Stopped,
    /// A generation ran after the moment.
    Running,
    /// The relay ran all its generations and ended by itself (`e`), so
    /// nothing stopped it.
    RanItsCourse,
    /// A generation could not lead its own group or fork its successor (`x`),
    /// so the relay ended without being stopped.
    Broke,
    /// No generation ran and the lifeline did not end by the deadline: a
    /// relay process is alive but not running (stopped, or never scheduled).
    Silent,
}

/// A relay's lifeline and the terminal byte (`e` or `x`) read from it so
/// far. The terminal byte is kept across reads, so a relay that wrote `e`
/// while one call was waiting and then closed the lifeline is still known to
/// have run its course when a later call reads the end.
pub(super) struct RelayLife {
    pub(super) lifeline: Lifeline,
    terminal: Option<u8>,
}

impl RelayLife {
    /// Open the lifeline `life` in `s`, before the relay starts.
    pub(super) fn new(s: &Scratch) -> Self {
        Self {
            lifeline: Lifeline::new(s, "life"),
            terminal: None,
        }
    }

    fn note(&mut self, byte: u8) {
        if matches!(byte, b'e' | b'x') {
            self.terminal = Some(byte);
        }
    }

    fn ended(&self) -> RelayFate {
        match self.terminal {
            Some(b'e') => RelayFate::RanItsCourse,
            Some(_) => RelayFate::Broke,
            None => RelayFate::Stopped,
        }
    }

    /// Wait until the relay has made `handoffs` complete handoffs: one more
    /// `h` on its lifeline than that, each from a new generation.
    pub(super) async fn await_handoffs(&mut self, handoffs: usize) {
        let mut generations = 0;
        while generations <= handoffs {
            match self.lifeline.hear("a relay generation starting").await {
                Heard::Byte(b'h') => generations += 1,
                other => panic!("the relay ended before {handoffs} handoffs: {other:?}"),
            }
        }
    }

    /// What became of the relay from now on, deciding by `deadline` at the
    /// latest. Every byte already on the lifeline was written before now;
    /// then the first thing heard decides: the end of the lifeline is
    /// [`RelayFate::Stopped`] (unless a terminal byte came first), any byte
    /// is a generation that ran after now. A generation killed just before
    /// now has written nothing since and ends the lifeline as soon as it is
    /// gone.
    pub(super) async fn fate_by(&mut self, deadline: std::time::Instant) -> RelayFate {
        while let Some(heard) = self.lifeline.try_hear() {
            match heard {
                Heard::Ended => return self.ended(),
                Heard::Byte(byte) => self.note(byte),
            }
        }
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match self.lifeline.hear_within(left).await {
            None => RelayFate::Silent,
            Some(Heard::Ended) => self.ended(),
            Some(Heard::Byte(byte)) => {
                self.note(byte);
                RelayFate::Running
            }
        }
    }
}

/// When a relay's terminal byte reaches the reader, relative to a call of
/// [`RelayLife::fate_by`].
#[derive(Clone, Copy)]
enum TerminalByte {
    /// Written while the call waits on an empty lifeline: the call reads it
    /// as `Running`, and a later call reads the end.
    WhileWaiting,
    /// Written, and the lifeline closed, before the call: the call reads both
    /// at once.
    AlreadyBuffered,
}

/// A relay's last generation stood in for by a write end of the lifeline
/// that the test holds, so the test decides when each byte is written: `h`,
/// then the terminal byte `last` at the point `when` names, then the close.
/// Whichever branch reads the terminal byte, the relay must never be taken
/// for a stopped one.
async fn fate_of_a_relay_that_ends_with(last: u8, when: TerminalByte) -> RelayFate {
    use std::future::Future;
    use std::io::Write;
    use std::task::Poll;
    let s = Scratch::new();
    let mut relay = RelayLife::new(&s);
    // The read end is open, so opening the write end does not block.
    let mut writer = std::fs::OpenOptions::new()
        .write(true)
        .open(s.path("life"))
        .expect("open the lifeline for writing");
    writer.write_all(b"h").expect("write a handoff");
    relay.await_handoffs(0).await;
    let deadline = std::time::Instant::now() + AWAIT;
    let first = match when {
        TerminalByte::WhileWaiting => {
            let mut call = std::pin::pin!(relay.fate_by(deadline));
            // Poll once: the lifeline is empty and still open, so the call
            // must be waiting, not answering.
            let polled = std::future::poll_fn(|cx| Poll::Ready(call.as_mut().poll(cx))).await;
            assert!(
                polled.is_pending(),
                "fate_by answered on an empty lifeline: {polled:?}"
            );
            writer.write_all(&[last]).expect("write the terminal byte");
            drop(writer);
            call.await
        }
        TerminalByte::AlreadyBuffered => {
            writer.write_all(&[last]).expect("write the terminal byte");
            drop(writer);
            relay.fate_by(deadline).await
        }
    };
    if first != RelayFate::Running {
        return first;
    }
    relay.fate_by(deadline).await
}

#[tokio::test]
async fn a_relay_that_ran_its_course_while_its_fate_was_awaited_is_never_taken_for_a_stopped_one() {
    assert_eq!(
        fate_of_a_relay_that_ends_with(b'e', TerminalByte::WhileWaiting).await,
        RelayFate::RanItsCourse
    );
}

#[tokio::test]
async fn a_relay_that_broke_while_its_fate_was_awaited_is_never_taken_for_a_stopped_one() {
    assert_eq!(
        fate_of_a_relay_that_ends_with(b'x', TerminalByte::WhileWaiting).await,
        RelayFate::Broke
    );
}

#[tokio::test]
async fn a_relay_that_ran_its_course_before_its_fate_was_asked_is_never_taken_for_a_stopped_one() {
    assert_eq!(
        fate_of_a_relay_that_ends_with(b'e', TerminalByte::AlreadyBuffered).await,
        RelayFate::RanItsCourse
    );
}

/// The same relay with 4000 generations, so it outlives any stop by many
/// seconds and only a stop can explain its end. The case directory `dir` is
/// passed to `perl` as an argument it ignores, so every generation's command
/// line names it (a fork keeps the command line) and `case_processes` finds
/// any that survive.
pub(super) fn long_relay(dir: &std::path::Path) -> String {
    let dir = dir.to_str().expect("a scratch directory named in UTF-8");
    assert!(!dir.contains('\''), "{dir}");
    format!("{} '{dir}'", RELAY.replace("(1..300)", "(1..4000)"))
}

/// What one interrupted kept run of the long relay came to: whether a relay
/// process was alive when the report arrived, the verdict, how long the stop
/// took from the interrupt to the report, and the report's diagnostics for a
/// failure message (see [`relay_diagnostics`]).
struct RelayRun {
    alive: bool,
    cleanup: Cleanup,
    took: Duration,
    diagnostics: String,
}

async fn relay_run() -> (bool, Cleanup, Duration) {
    let run = relay_run_with_diagnostics().await;
    (run.alive, run.cleanup, run.took)
}

async fn relay_run_with_diagnostics() -> RelayRun {
    let s = Scratch::new();
    let mut relay = RelayLife::new(&s);
    let mut spec = kept(sh(&s, &long_relay(&s.dir()), LONG));
    spec.stop_grace = Duration::from_millis(100);
    // The relay holds the output pipes; the owner closes them a second after
    // the kill.
    spec.drain_bound = Duration::from_secs(1);
    let handle = crate::process::ProcessHandle::start(spec);
    // The interrupt comes once the relay is relaying: one generation has
    // started its successor.
    relay.await_handoffs(1).await;
    let asked = std::time::Instant::now();
    handle.interrupt();
    let report = handle.wait().await;
    let took = asked.elapsed();
    // A relay process alive after the report either runs (an `h` after it)
    // or holds the lifeline open without running; only the end of the
    // lifeline says every one is gone.
    let fate = relay.fate_by(std::time::Instant::now() + AWAIT).await;
    // On Linux, any process that names the case directory on its command
    // line counts as alive too: a census that does not depend on the
    // lifeline.
    #[cfg(target_os = "linux")]
    let survivors = super::fixtures::case_processes(&s.dir())
        .unwrap_or_else(|e| panic!("survivors unknown: {e}"));
    #[cfg(not(target_os = "linux"))]
    let survivors: Vec<i32> = Vec::new();
    let alive = fate != RelayFate::Stopped || !survivors.is_empty();
    let diagnostics = format!("relay {fate:?}\n{}", relay_diagnostics(&survivors, &report));
    // A relay that survived is ended here, so nothing is left, without
    // signalling a foreign process (see `end_relay_leftovers`).
    super::fixtures::end_relay_leftovers(&s.dir(), &mut relay.lifeline).await;
    RelayRun {
        alive,
        cleanup: report.cleanup,
        took,
        diagnostics,
    }
}

/// What a relay assertion prints on failure: the survivors, the verdict, and
/// every answer of the keeper in `tree_reports`, one per line, so a failure
/// shows which answer was unproven (a refused hold, a failed read, or a
/// freeze that did not complete) and whether it arrived after its request
/// timed out (`late`); then `diagnostics.keeper` whole.
fn relay_diagnostics(survivors: &[i32], report: &crate::process::ProcessReport) -> String {
    let keeper = report.diagnostics.keeper.as_ref();
    let answers = keeper.map_or_else(
        || "  (not a kept run)".to_string(),
        |k| {
            k.tree_reports
                .iter()
                .map(|r| format!("  {r}"))
                .collect::<Vec<_>>()
                .join("\n")
        },
    );
    format!(
        "survivors {survivors:?}\ncleanup {:?}\nkeeper tree_reports:\n{answers}\n\
         diagnostics.keeper {:#?}\nwarnings {:?}\nsignals {:?}",
        report.cleanup, keeper, report.diagnostics.warnings, report.diagnostics.signals
    )
}

/// Gap 2. On Linux the keeper's own walk of its tree stops the relay: every
/// generation becomes the keeper's child as its parent exits, and the walk
/// stops it before it reads its children. On macOS a freeze of listings does
/// not reliably catch it (see the measurement below and the module
/// documentation), so the test is ignored there.
#[tokio::test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "macOS residual: in 50 interrupted kept runs on macOS 27.2 the freeze stopped the relay in 0 (all 50 ended Unconfirmed because the relay held the output); see process/mod.rs"
)]
async fn a_fork_relay_is_stopped_in_a_kept_run() {
    require_perl!();
    let run = relay_run_with_diagnostics().await;
    assert!(
        !run.alive,
        "a relay process was alive after the report: {}",
        run.diagnostics
    );
    assert_eq!(run.cleanup, Cleanup::Confirmed, "{}", run.diagnostics);
}

/// Whatever the platform, a kept run of the relay never claims `Confirmed`
/// while a relay process is alive. On Linux the keeper stops it; on macOS it
/// is not stopped, but it holds the output pipes past the drain bound, and a
/// held pipe makes the verdict `Unconfirmed` (or, where `ps -E` shows the
/// marker of perl, a freeze that keeps finding new generations does).
#[tokio::test]
async fn a_fork_relay_is_never_confirmed_while_it_runs_in_a_kept_run() {
    require_perl!();
    let run = relay_run_with_diagnostics().await;
    assert!(
        !(run.alive && run.cleanup == Cleanup::Confirmed),
        "a relay process was alive but cleanup says Confirmed: {}",
        run.diagnostics
    );
    if run.alive && !super::fixtures::marker_visible_for_system_programs() {
        match &run.cleanup {
            Cleanup::Unconfirmed { reason, .. } => {
                assert_eq!(
                    reason,
                    crate::process::PIPES_HELD_REASON,
                    "{}",
                    run.diagnostics
                );
            }
            other => panic!("{other:?}: {}", run.diagnostics),
        }
    }
}

/// The measurement behind the macOS residual: 50 interrupted kept runs of the
/// relay, counting those that stopped it and those that claimed `Confirmed`
/// with a relay process alive. Run with `cargo test -- --ignored
/// measure_the_relay_freeze --nocapture`.
#[tokio::test]
#[ignore = "a measurement, not a check"]
async fn measure_the_relay_freeze() {
    if !super::fixtures::perl_available() {
        return;
    }
    let mut stopped = 0;
    let mut false_confirmations = 0;
    let mut unconfirmed = 0;
    let mut pipes_held = 0;
    let mut took = Vec::new();
    for _ in 0..50 {
        let (alive, cleanup, stop) = relay_run().await;
        super::fixtures::assert_documented(&cleanup);
        took.push(stop);
        if !alive {
            stopped += 1;
        }
        if alive && cleanup == Cleanup::Confirmed {
            false_confirmations += 1;
        }
        if cleanup != Cleanup::Confirmed {
            unconfirmed += 1;
        }
        if matches!(&cleanup, Cleanup::Unconfirmed { reason, .. } if reason == crate::process::PIPES_HELD_REASON)
        {
            pipes_held += 1;
        }
    }
    took.sort();
    eprintln!(
        "relay: stopped in {stopped} of 50 runs; Confirmed with a relay alive in \
         {false_confirmations}; Unconfirmed in {unconfirmed} ({pipes_held} because a process \
         held the output); interrupt to report median {:?}, max {:?}",
        took[25], took[49]
    );
}

/// A lister that fails once, on its second call: with no periodic scan after the
/// first, that is the listing taken when the leader exits.
fn failing_exit_listing(marker: std::path::PathBuf) -> Lister {
    let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
    Arc::new(move || {
        let marker = marker.clone();
        let call = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        Box::pin(async move {
            if call == 2 {
                return Err(crate::process::tree::InspectError::Failed(
                    "injected failure at exit".into(),
                ));
            }
            let rows = list_processes().await;
            let _ = std::fs::write(&marker, "scanned");
            rows
        })
    })
}

#[tokio::test]
async fn a_missed_listing_at_exit_is_unconfirmed_whatever_later_sweeps_find() {
    require_perl!();
    let s = Scratch::new();
    let mut spec = leader_with_a_live_middle_process(&s, "exit 0");
    spec = spec.with_lister(failing_exit_listing(s.path("scanned")));
    let (report, pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
    // The listing that could have seen the descendant with its parent never
    // completed, so the descendant is orphaned and cannot be followed.
    assert_no_false_confirmation(pids[0], &report.cleanup);
    match &report.cleanup {
        Cleanup::Unconfirmed { reason, .. } => assert!(
            reason.contains("before the first signal did not complete"),
            "{reason}"
        ),
        other => panic!("a missed exit listing must not confirm cleanup: {other:?}"),
    }
}

/// A lister that reads the real table after `delay` on every call but the
/// first, drops the marker file, and records the longest a call took (the delay
/// and the real `ps` together, in milliseconds).
fn delayed_lister(
    marker: std::path::PathBuf,
    delay: Duration,
    slowest_ms: Arc<std::sync::atomic::AtomicU64>,
) -> Lister {
    let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
    Arc::new(move || {
        let marker = marker.clone();
        let slowest_ms = slowest_ms.clone();
        let call = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move {
            let began = std::time::Instant::now();
            if call > 0 {
                tokio::time::sleep(delay).await;
            }
            let rows = list_processes().await;
            let _ = std::fs::write(&marker, "scanned");
            let took = u64::try_from(began.elapsed().as_millis()).unwrap_or(u64::MAX);
            slowest_ms.fetch_max(took, std::sync::atomic::Ordering::SeqCst);
            rows
        })
    })
}

/// The exit budget is the real one (3.1 s). Every listing after the first takes
/// a fixed 400 ms on the run's clock, well inside the budget, so the exit
/// listing must be waited for and the descendant that left the session
/// followed; with the budget the old tests used (150 ms) this run is
/// `Unconfirmed`. The run's clock is paused from before the spawn and held
/// (`await_held`) until the leader has ended and the exit listing has begun, so
/// the start of the run and the leader's end cost the clock nothing. The 400 ms
/// then pass on the paused clock, and the real `ps` after them is taken off it
/// (`list_off_the_clock`), so however slow `ps` is under load, the listing
/// takes exactly 400 ms of the budget. The test resumes the real clock once
/// the listing has answered.
#[tokio::test]
async fn a_slow_listing_at_exit_is_waited_for_and_the_descendant_is_followed() {
    require_perl!();
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio::time::Instant;

    const DELAY: Duration = Duration::from_millis(400);
    let s = Scratch::new();
    let mut spec = leader_with_a_live_middle_process(&s, "exit 0");
    spec.timings.exit_listing = crate::process::limits::EXIT_LISTING_BUDGET;
    let calls = Arc::new(AtomicU32::new(0));
    let spans: Arc<Mutex<Vec<(u32, Instant, Instant)>>> = Arc::default();
    let (answered, mut answers) = tokio::sync::watch::channel(0u32);
    let answered = Arc::new(answered);
    let (lister_calls, recorder, marker) = (calls.clone(), spans.clone(), s.path("scanned"));
    spec = spec.with_lister(Arc::new(move || {
        let call = lister_calls.fetch_add(1, Ordering::SeqCst);
        let (recorder, answered, marker) = (recorder.clone(), answered.clone(), marker.clone());
        Box::pin(async move {
            let began = Instant::now();
            if call > 0 {
                tokio::time::sleep(DELAY).await;
            }
            let rows = list_off_the_clock().await;
            let _ = std::fs::write(&marker, "scanned");
            recorder
                .lock()
                .expect("lock spans")
                .push((call, began, Instant::now()));
            answered.send_modify(|n| *n += 1);
            rows
        })
    }));
    tokio::time::pause();
    let handle = crate::process::ProcessHandle::start(spec);
    let (gc, counted) = (s.path("gc"), calls.clone());
    // The leader ends once the descendant has published `gc`; the exit
    // listing is the first call after the one at spawn.
    let descendant = await_held(
        "the descendant's record and the exit listing beginning",
        move || {
            Ok(match read_record(&gc) {
                Record::Pid(pid) if counted.load(Ordering::SeqCst) >= 2 => Some(pid),
                _ => None,
            })
        },
    )
    .await;
    let mut reaper = Reaper::default();
    reaper.register(descendant, &s);
    // Only the listing's 400 ms are left to pass on the clock; nothing real
    // is awaited until it has answered.
    answers
        .wait_for(|n| *n >= 2)
        .await
        .expect("the lister is alive");
    tokio::time::resume();
    let report = tokio::time::timeout(Duration::from_secs(120), handle.wait())
        .await
        .expect("the run did not end after its exit listing");
    let exit_listing = spans
        .lock()
        .expect("lock spans")
        .iter()
        .find(|(call, ..)| *call == 1)
        .map(|(_, began, ended)| *ended - *began)
        .expect("the exit listing was recorded");
    // Tokio's timer wheel rounds the delay's deadline up to its next
    // millisecond tick.
    assert!(
        DELAY <= exit_listing && exit_listing <= DELAY + Duration::from_millis(1),
        "the exit listing took {exit_listing:?} of the run's clock, not its delay of {DELAY:?}"
    );
    assert_no_false_confirmation(descendant, &report.cleanup);
    assert_eq!(report.ending, Ending::Exited { code: 0 }, "{report:#?}");
    assert_eq!(report.cleanup, Cleanup::Confirmed, "{report:#?}");
    assert_gone("descendant", &[descendant]).await;
}

/// The exit budget of the concurrent runs below, far under the real one.
const FAST_EXIT_BUDGET: Duration = Duration::from_millis(250);
/// How long a held run keeps its listings after the first, a second past the budget.
const HELD_LISTING: Duration = Duration::from_millis(1250);

/// Many runs at once, so listings are slow. A run may take long, but none may
/// end `Confirmed` with its descendant alive. The runs are tasks of one
/// single-thread runtime; their listings are separate `ps` processes, and the
/// checks the test makes on the runtime thread are not blocking ones, so a run
/// is never held past its deadline by another run's `ps`. The exit budget is
/// [`FAST_EXIT_BUDGET`], not the real one (the real value is pinned in
/// `budgets.rs`). A missed listing can leave a descendant holding both pipes,
/// so the fixture lets the owner close them 100 ms after KILL instead of
/// waiting the default 10 s. Every fifth run has its listings after the first
/// held for [`HELD_LISTING`], a full second more than that budget, so the
/// late-listing branch is reached every time whatever the load: those must end
/// `Unconfirmed` for the late listing while their descendant is alive. The
/// others may meet the late listing only when the machine makes them.
#[tokio::test]
async fn fifty_concurrent_runs_never_confirm_cleanup_with_a_live_descendant() {
    require_perl!();
    let mut tasks = Vec::new();
    for n in 0..50 {
        let delayed = n % 5 == 0;
        tasks.push(tokio::spawn(async move {
            let s = Scratch::new();
            let mut spec = leader_with_a_live_middle_process(&s, "exit 0");
            spec.timings.exit_listing = FAST_EXIT_BUDGET;
            // The assertion checks the verdict and descendant, so there is
            // no output to preserve with a longer drain.
            spec.drain_bound = Duration::from_millis(100);
            if delayed {
                spec = spec.with_lister(delayed_lister(
                    s.path("scanned"),
                    HELD_LISTING,
                    Arc::default(),
                ));
            }
            let (report, pids, reaper) = run_guarded(&s, spec, &["gc"]).await;
            // A descendant still alive after the report is checked once the
            // process has had a moment to receive its signal.
            tokio::time::sleep(Duration::from_millis(300)).await;
            let alive = is_running_async(pids[0]).await;
            drop(reaper);
            (report.cleanup, alive, delayed)
        }));
    }
    for task in tasks {
        let (cleanup, alive, delayed) = task.await.expect("the run finished");
        assert!(
            !(alive && cleanup == Cleanup::Confirmed),
            "a descendant was alive but cleanup says Confirmed"
        );
        if delayed {
            assert!(
                crate::process::is_late_listing_only(&cleanup),
                "a run whose exit listing was held past the budget must report the late listing: {cleanup:?}"
            );
        }
    }
}
