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
//! listing for long enough to be signalled. The last three tests state the
//! wanted behaviour and are ignored because they fail. Run them with
//! `cargo test -- --ignored`. They leave one process running when they fail,
//! so each stops it itself.

use std::sync::Arc;
use std::time::Duration;

use super::fixtures::{
    OWN_GROUP, OWN_SESSION, Reaper, Scratch, WAIT_ESCAPED, assert_gone, is_running, run_guarded, sh,
};
use crate::contract::result::Cleanup;
use crate::process::Ending;
use crate::process::tree::{Lister, list_processes};

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

#[tokio::test]
#[ignore = "known gap: the parent was the leader, orphaned before any listing"]
async fn a_setsid_grandchild_whose_parent_exits_at_once_is_not_reported_as_cleaned_up() {
    require_perl!();
    let s = Scratch::new();
    let script = format!(
        "{WAIT_FOR_SCAN} echo $$ > leader; {OWN_SESSION} >/dev/null 2>&1 & echo $! > gc; {WAIT_ESCAPED} exit 0"
    );
    let spec = sh(&s, &script, LONG).with_lister(marking_lister(s.path("scanned")));
    let (report, pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
    assert_no_false_confirmation(pids[0], &report.cleanup);
}

#[tokio::test]
#[ignore = "known gap: the parent was the leader, orphaned before any listing"]
async fn a_setsid_descendant_started_between_two_scans_before_a_crash_is_not_reported_as_cleaned_up()
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
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
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
        .spawn()
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

fn heartbeat_age(scratch: &Scratch) -> Duration {
    std::fs::metadata(scratch.path("beat"))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .unwrap_or(Duration::MAX)
}

#[tokio::test]
#[ignore = "known gap: a relay is never in a listing long enough"]
async fn a_fork_relay_is_not_reported_as_cleaned_up_while_it_runs() {
    require_perl!();
    let s = Scratch::new();
    let mut spec = sh(&s, RELAY, LONG);
    spec.stop_grace = Duration::from_millis(100);
    let handle = crate::process::ProcessHandle::start(spec);
    s.exists("beat").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    handle.interrupt();
    let report = handle.wait().await;
    // The heartbeat is touched by every generation, so a recent one means a
    // relay process is alive now, however short each one lives.
    let alive = heartbeat_age(&s) < Duration::from_millis(150);
    // Let the relay finish by itself before asserting, so nothing is left.
    for _ in 0..500 {
        if heartbeat_age(&s) > Duration::from_millis(300) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        !(alive && report.cleanup == Cleanup::Confirmed),
        "a relay process was alive but cleanup says Confirmed"
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

#[tokio::test]
async fn a_slow_listing_at_exit_is_waited_for_and_the_descendant_is_followed() {
    require_perl!();
    let s = Scratch::new();
    let mut spec = leader_with_a_live_middle_process(&s, "exit 0");
    // Every listing after the first takes 400 ms, beyond the old 150 ms and 300 ms budgets.
    let marker = s.path("scanned");
    let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
    spec = spec.with_lister(Arc::new(move || {
        let marker = marker.clone();
        let call = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move {
            if call > 0 {
                tokio::time::sleep(Duration::from_millis(400)).await;
            }
            let rows = list_processes().await;
            let _ = std::fs::write(&marker, "scanned");
            rows
        })
    }));
    let (report, pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
    assert_no_false_confirmation(pids[0], &report.cleanup);
    assert_eq!(report.cleanup, Cleanup::Confirmed, "{report:#?}");
    assert_gone("descendant", &pids).await;
}

/// Many runs at once, so listings are slow. A run may take long, but none may
/// end `Confirmed` with its descendant alive. The runs are tasks of one
/// single-thread runtime; their listings are separate `ps` processes.
#[tokio::test]
async fn fifty_concurrent_runs_never_confirm_cleanup_with_a_live_descendant() {
    require_perl!();
    let mut tasks = Vec::new();
    for _ in 0..50 {
        tasks.push(tokio::spawn(async {
            let s = Scratch::new();
            let spec = leader_with_a_live_middle_process(&s, "exit 0");
            let (report, pids, reaper) = run_guarded(&s, spec, &["gc"]).await;
            // A descendant still alive after the report is checked once the
            // process has had a moment to receive its signal.
            tokio::time::sleep(Duration::from_millis(300)).await;
            let alive = is_running(pids[0]);
            drop(reaper);
            (report.cleanup, alive)
        }));
    }
    for task in tasks {
        let (cleanup, alive) = task.await.expect("the run finished");
        assert!(
            !(alive && cleanup == Cleanup::Confirmed),
            "a descendant was alive but cleanup says Confirmed"
        );
    }
}
