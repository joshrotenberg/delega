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
    OWN_GROUP, OWN_SESSION, Reaper, Scratch, WAIT_ESCAPED, assert_gone, is_running,
    is_running_async, run_guarded, sh,
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

/// Waits until the last listing of `CAPTURE_RAMP` (200 ms after spawn) has
/// passed, so a descendant started after it can only be seen by the periodic
/// listing, 500 ms after the ramp ends. The first listing ends just after
/// spawn, so 0.3 s from there is after 200 ms with room to spare.
const AFTER_RAMP: &str = "sleep 0.3;";

/// The wanted behaviour: when a descendant is left running, the result must not
/// claim cleanup is confirmed.
fn assert_no_false_confirmation(descendant: i32, cleanup: &Cleanup) {
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
    let script = format!("{WAIT_FOR_SCAN} echo $$ > leader; {OWN_GROUP} & echo $! > gc; exit 0");
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
    // The crash lands 0.2 s after the descendant starts. No periodic scan runs
    // after the first, however slow the machine is.
    let script = format!(
        "{WAIT_FOR_SCAN} echo $$ > leader; {OWN_GROUP} & echo $! > gc; \
         sleep 0.2; kill -KILL $$; sleep 5"
    );
    let mut spec = sh(&s, &script, LONG).with_lister(marking_lister(s.path("scanned")));
    spec.capture_interval = Some(Duration::from_secs(3600));
    let (report, pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
    assert_eq!(report.cleanup, Cleanup::Confirmed, "{report:#?}");
    super::fixtures::assert_gone("descendant", &pids).await;
}

/// Pins the schedule itself, not one timing. A lister records when each
/// listing starts and ends. The child lives 1 s, so the first periodic listing
/// is not the one under test. Each of the three ramp listings must start
/// promptly once it is due: no later than its offset from spawn (50, 100, 200
/// ms) or the end of the listing before it, whichever is later, plus a margin
/// for scheduling. Without that step the next listing would come one periodic
/// interval (500 ms) after the previous one ended, far past the margin. Timing
/// each start from the previous end, and not from spawn, keeps the test honest
/// when `ps` is slow under load and the ramp listings run back to back. A ramp
/// listing is never early, so each also starts no sooner than its offset.
#[tokio::test]
async fn each_ramp_listing_starts_once_due_and_before_the_first_periodic_listing() {
    use crate::process::ProcessHandle;
    use std::sync::Mutex;
    use std::time::Instant;

    /// Far below the 500 ms a missing ramp step would cost, and far above
    /// ordinary task scheduling delay.
    const MARGIN: Duration = Duration::from_millis(250);

    let s = Scratch::new();
    let spans: Arc<Mutex<Vec<(Instant, Instant)>>> = Arc::new(Mutex::new(Vec::new()));
    let recorder = spans.clone();
    let lister: Lister = Arc::new(move || {
        let recorder = recorder.clone();
        Box::pin(async move {
            let started = Instant::now();
            let listing = list_processes().await;
            recorder
                .lock()
                .expect("lock spans")
                .push((started, Instant::now()));
            listing
        })
    });
    let spec = sh(&s, "sleep 1", LONG).with_lister(lister);
    let spawned = Instant::now();
    let report = ProcessHandle::start(spec).wait().await;
    assert_eq!(report.cleanup, Cleanup::Confirmed, "{report:#?}");
    let mut spans = spans.lock().expect("lock spans").clone();
    spans.sort_by_key(|(started, _)| *started);
    let offsets: Vec<(Duration, Duration)> = spans
        .iter()
        .map(|(a, b)| {
            (
                a.saturating_duration_since(spawned),
                b.saturating_duration_since(spawned),
            )
        })
        .collect();
    assert!(offsets.len() >= 4, "fewer than four listings: {offsets:?}");
    // Listing 0 is the one at spawn. The offsets are written out here, not
    // read from `CAPTURE_RAMP`, so that shortening the ramp fails the test.
    for (step, due_ms) in [50u64, 100, 200].into_iter().enumerate() {
        let due = Duration::from_millis(due_ms);
        let (start, _) = offsets[step + 1];
        let (_, previous_end) = offsets[step];
        let ready = due.max(previous_end);
        assert!(
            start + Duration::from_millis(5) >= due,
            "ramp listing {step} started at {start:?}, before its offset {due:?}: {offsets:?}"
        );
        assert!(
            start <= ready + MARGIN,
            "ramp listing {step} started at {start:?}, more than {MARGIN:?} after it was due at \
             {ready:?}: {offsets:?}"
        );
    }
}

/// The periodic interval is the default here, so the listings are the one at
/// spawn, the early ramp (`CAPTURE_RAMP`) and then the 500 ms interval. The
/// descendant calls `setsid` straight after the first listing and the leader
/// crashes about 0.2 s later, long before the first periodic listing. Only a
/// ramp listing can see the descendant while the leader is its parent. Without
/// the ramp the descendant outlives the run, and the result says Confirmed.
#[tokio::test]
async fn a_setsid_descendant_started_just_after_spawn_is_cleaned_up_when_the_leader_crashes_before_the_first_periodic_listing()
 {
    require_perl!();
    let s = Scratch::new();
    let script = format!(
        "{WAIT_FOR_SCAN} echo $$ > leader; {OWN_SESSION} >/dev/null 2>&1 & echo $! > gc; {WAIT_ESCAPED} \
         sleep 0.2; kill -KILL $$; sleep 5"
    );
    let spec = sh(&s, &script, LONG).with_lister(marking_lister(s.path("scanned")));
    let (report, pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
    assert_no_false_confirmation(pids[0], &report.cleanup);
    assert_eq!(report.cleanup, Cleanup::Confirmed, "{report:#?}");
    assert_gone("descendant", &pids).await;
}

/// Gap 1 without a keeper: a `setsid` descendant whose parent, the leader,
/// exits before any listing saw the two together. With a keeper it is found
/// (Linux: it becomes the keeper's child; elsewhere: it carries the run
/// marker), stopped, and the verdict is `Confirmed` only once it is gone. The
/// descendant runs this test binary, not `sleep`: on macOS `ps -E` shows no
/// environment for Apple's own programs, so an orphan running one of them is
/// the documented residual there.
#[tokio::test]
async fn a_setsid_grandchild_whose_parent_exits_at_once_is_stopped_in_a_kept_run() {
    require_perl!();
    let s = Scratch::new();
    let script = format!(
        "{WAIT_FOR_SCAN} echo $$ > leader; {AFTER_RAMP} {} >/dev/null 2>&1 & echo $! > gc; {WAIT_ESCAPED} exit 0",
        own_session_sleeper()
    );
    let spec = kept(sh(&s, &script, LONG).with_lister(marking_lister(s.path("scanned"))));
    let (report, pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
    assert_no_false_confirmation(pids[0], &report.cleanup);
    assert_eq!(report.cleanup, Cleanup::Confirmed, "{report:#?}");
    assert_gone("orphaned setsid descendant", &pids).await;
}

#[tokio::test]
async fn a_setsid_descendant_started_between_two_scans_before_a_crash_is_stopped_in_a_kept_run() {
    require_perl!();
    let s = Scratch::new();
    let script = format!(
        "{WAIT_FOR_SCAN} echo $$ > leader; {AFTER_RAMP} {} >/dev/null 2>&1 & echo $! > gc; {WAIT_ESCAPED} \
         sleep 0.15; kill -KILL $$; sleep 5",
        own_session_sleeper()
    );
    let spec = kept(sh(&s, &script, LONG).with_lister(marking_lister(s.path("scanned"))));
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
    let detach =
        "perl -e 'use POSIX; POSIX::setsid(); %ENV = (); exec { q(/bin/sleep) } q(sleep), q(60)'";
    let script = format!(
        "{WAIT_FOR_SCAN} echo $$ > leader; {AFTER_RAMP} {detach} >/dev/null 2>&1 & echo $! > gc; {WAIT_ESCAPED} exit 0"
    );
    // The owner's listings never show it: once the leader is known they hide
    // every group leader but the leader, which the orphan is from its
    // `setsid` on. Only the keeper's own walk of its tree can find it, so a
    // broken walk ends Confirmed with the orphan alive and fails here.
    let marker = s.path("scanned");
    let leader_file = s.path("leader");
    let hiding: Lister = Arc::new(move || {
        let marker = marker.clone();
        let leader_file = leader_file.clone();
        Box::pin(async move {
            let rows = list_processes().await;
            let _ = std::fs::write(&marker, "scanned");
            let leader: Option<i32> = std::fs::read_to_string(&leader_file)
                .ok()
                .and_then(|t| t.trim().parse().ok());
            rows.map(|rows| match leader {
                Some(leader) => rows
                    .into_iter()
                    .filter(|r| r.group != r.pid || r.pid == leader)
                    .collect(),
                None => rows,
            })
        })
    });
    let spec = kept(sh(&s, &script, LONG).with_lister(hiding));
    let (report, pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
    assert_no_false_confirmation(pids[0], &report.cleanup);
    assert_eq!(report.cleanup, Cleanup::Confirmed, "{report:#?}");
    assert_gone("orphan without the marker", &pids).await;
}

/// A middle process that stays alive in the child's session and group and
/// starts a descendant that calls `setsid` and then writes its own pid to `gc`.
/// The descendant's parent is the middle process, not the leader.
const MIDDLE: &str = "perl -e 'use POSIX; POSIX::setsid(); \
    open F, q(>), q(gc.tmp); print F $$; close F; rename q(gc.tmp), q(gc); \
    exec q(sleep), q(60)' &\nwait\n";

/// The script of a leader that starts the middle process after the first scan
/// and ends as soon as the descendant has left the session, which is before the
/// next scan. No periodic scan can have seen the descendant with its parent
/// alive, so only the listing taken when the leader exits can.
fn leader_with_a_live_middle_process(s: &Scratch, ending: &str) -> crate::process::ProcessSpec {
    std::fs::write(s.path("middle.sh"), MIDDLE).expect("write the middle script");
    let script = format!(
        "{WAIT_FOR_SCAN} echo $$ > leader; sh middle.sh & \
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
    let _foreign_reaper = Reaper(foreigners.clone());
    for pid in &foreigners {
        assert!(is_running(*pid), "foreign process {pid} did not start");
    }

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
/// milliseconds, forks a successor and exits, and touches a heartbeat file, so
/// no process stays alive for longer than one listing. It stops itself after
/// 300 generations.
const RELAY: &str = "exec perl -e 'use POSIX; \
    my $p = fork(); \
    if ($p == 0) { $SIG{TERM} = q(IGNORE); \
      for (1..300) { setpgid(0,0); open H, q(>), q(beat); print H $$; close H; \
        select(undef, undef, undef, 0.005); \
        my $q = fork(); \
        if ($q) { exit 0 } } \
      exit 0 } \
    sleep 60'";

/// The same relay with 4000 generations, so it outlives any stop by many
/// seconds and only a stop can explain its end.
pub(super) fn long_relay() -> String {
    RELAY.replace("(1..300)", "(1..4000)")
}

pub(super) fn heartbeat_age(scratch: &Scratch) -> Duration {
    std::fs::metadata(scratch.path("beat"))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .unwrap_or(Duration::MAX)
}

/// What one interrupted kept run of the long relay came to: whether a relay
/// process was alive when the report arrived, the verdict, and how long the
/// stop took from the interrupt to the report.
/// What one interrupted kept run of the long relay came to, with the report's
/// diagnostics for a failure message.
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
    let mut spec = kept(sh(&s, &long_relay(), LONG));
    spec.stop_grace = Duration::from_millis(100);
    // The relay holds the output pipes; the owner closes them a second after
    // the kill.
    spec.drain_bound = Duration::from_secs(1);
    let handle = crate::process::ProcessHandle::start(spec);
    s.exists("beat").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let asked = std::time::Instant::now();
    handle.interrupt();
    let report = handle.wait().await;
    let reported = std::time::SystemTime::now();
    let took = asked.elapsed();
    // Every generation writes its pid to the heartbeat about every 5 ms, so a
    // relay process alive after the report writes it after the report: a
    // heartbeat written since then, or one that changes over 300 ms, means a
    // relay process is alive. A fresh heartbeat at the report alone does not:
    // a relay killed just before the report left it fresh.
    let first = std::fs::read_to_string(s.path("beat")).unwrap_or_default();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let second = std::fs::read_to_string(s.path("beat")).unwrap_or_default();
    let written_after = std::fs::metadata(s.path("beat"))
        .and_then(|m| m.modified())
        .is_ok_and(|modified| modified > reported);
    let alive = first != second || written_after;
    let diagnostics = format!(
        "cleanup {:?}; keeper {:?}; warnings {:?}; signals {:?}",
        report.cleanup,
        report.diagnostics.keeper,
        report.diagnostics.warnings,
        report.diagnostics.signals
    );
    // A relay that survived is ended here, so nothing is left: the last
    // writer of the heartbeat is the live generation, and the relay is
    // killed generation by generation until the heartbeat goes stale.
    for _ in 0..3000 {
        if heartbeat_age(&s) > Duration::from_millis(300) {
            break;
        }
        if let Ok(pid) = std::fs::read_to_string(s.path("beat"))
            && let Ok(pid) = pid.trim().parse::<i32>()
        {
            let _ = crate::process::sys::signal_process(pid, crate::process::sys::Signal::KILL);
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    RelayRun {
        alive,
        cleanup: report.cleanup,
        took,
        diagnostics,
    }
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

#[tokio::test]
async fn a_slow_listing_at_exit_is_waited_for_and_the_descendant_is_followed() {
    require_perl!();
    // The exit budget is the real one (3.1 s). Every listing after the first
    // takes a fixed 400 ms and then the real `ps`, well inside the budget, so the
    // exit listing must be waited for and the descendant that left the session
    // followed. With the budget the old tests used (150 ms) this run is
    // `Unconfirmed`. The only thing load can change is the real `ps`: when the
    // longest listing was itself near the budget the premise is gone and the run
    // is repeated, and only when the verdict is exactly the late listing's
    // (`is_late_listing_only`). A listing under 2 s with any verdict but
    // `Confirmed`, or a slow listing with any other `Unconfirmed` (a failed stop,
    // a pid left over), fails.
    for attempt in 1..=5 {
        let s = Scratch::new();
        let mut spec = leader_with_a_live_middle_process(&s, "exit 0");
        spec.timings.exit_listing = crate::process::limits::EXIT_LISTING_BUDGET;
        let slowest = Arc::new(std::sync::atomic::AtomicU64::new(0));
        spec = spec.with_lister(delayed_lister(
            s.path("scanned"),
            Duration::from_millis(400),
            slowest.clone(),
        ));
        let (report, pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
        assert_no_false_confirmation(pids[0], &report.cleanup);
        let slowest = slowest.load(std::sync::atomic::Ordering::SeqCst);
        if report.cleanup == Cleanup::Confirmed {
            assert_gone("descendant", &pids).await;
            return;
        }
        assert!(
            slowest > 2000 && crate::process::is_late_listing_only(&report.cleanup),
            "the slowest listing took {slowest} ms; a run that is not Confirmed must have a \
             listing over 2000 ms and the late listing's verdict alone: {report:#?}"
        );
        eprintln!("attempt {attempt}: a listing took {slowest} ms under load; repeating");
    }
    panic!(
        "five runs in a row had a listing of more than 2 s; the machine is too loaded to test this"
    );
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
