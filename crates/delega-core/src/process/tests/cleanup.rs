//! Descendants, held pipes and the cleanup verdict.

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::fixtures::{Reaper, Scratch, assert_confirmed, assert_gone, sh};
use crate::contract::result::Cleanup;
use crate::process::tree::{InspectError, Lister};
use crate::process::{Ending, PIPE_WARNING, run};

const LONG: Duration = Duration::from_secs(60);
const BOUND: Duration = Duration::from_secs(6);

#[tokio::test]
async fn a_crash_while_a_descendant_holds_the_output_pipe_does_not_hang() {
    let s = Scratch::new();
    // The background sleep inherits both pipes and outlives its parent.
    let spec = sh(
        &s,
        "echo $$ > leader; sleep 60 & echo $! > gc; kill -KILL $$; sleep 5",
        LONG,
    );
    let started = Instant::now();
    let report = run(spec).await;
    assert!(
        matches!(report.ending, Ending::Signaled { signal: 9, .. }),
        "{report:#?}"
    );
    assert!(started.elapsed() < BOUND, "took {:?}", started.elapsed());
    assert!(!report.diagnostics.pipes_closed_by_owner, "{report:#?}");
    assert_confirmed(&report);
    assert_gone(
        "leader and descendant",
        &[s.pid_now("leader"), s.pid_now("gc")],
    )
    .await;
}

#[tokio::test]
async fn a_grandchild_that_outlives_its_parent_in_the_same_group_is_cleaned_up() {
    let s = Scratch::new();
    let spec = sh(
        &s,
        "echo $$ > leader; sleep 60 & echo $! > gc; exit 0",
        LONG,
    );
    let report = run(spec).await;
    assert_eq!(report.ending, Ending::Exited { code: 0 });
    assert_confirmed(&report);
    assert_gone("grandchild", &[s.pid_now("gc")]).await;
}

#[tokio::test]
async fn a_grandchild_that_ignores_terminate_is_killed_after_its_parent_exits() {
    let s = Scratch::new();
    let spec = sh(
        &s,
        "echo $$ > leader; (trap '' TERM; exec sleep 60) & echo $! > gc; exit 0",
        LONG,
    );
    let started = Instant::now();
    let report = run(spec).await;
    assert_eq!(report.ending, Ending::Exited { code: 0 });
    assert!(started.elapsed() < BOUND, "took {:?}", started.elapsed());
    assert_confirmed(&report);
    assert_gone("grandchild", &[s.pid_now("gc")]).await;
}

#[tokio::test]
async fn a_descendant_in_another_process_group_is_found_by_the_scan_and_cleaned_up() {
    let s = Scratch::new();
    // Job control puts the background sleep in a group of its own, so the
    // group signal cannot reach it. The leader lives long enough for two scans.
    let spec = sh(
        &s,
        "set -m; echo $$ > leader; sleep 60 & echo $! > gc; sleep 1.3; exit 0",
        LONG,
    );
    let report = run(spec).await;
    assert_eq!(report.ending, Ending::Exited { code: 0 }, "{report:#?}");
    assert_confirmed(&report);
    assert!(report.diagnostics.inspection.retained_descendant_identities >= 1);
    assert_gone("descendant in its own group", &[s.pid_now("gc")]).await;
}

#[tokio::test]
async fn interrupting_stops_a_descendant_in_another_process_group() {
    let s = Scratch::new();
    let handle = crate::process::ProcessHandle::start(sh(
        &s,
        "set -m; echo $$ > leader; sleep 60 & echo $! > gc; wait",
        LONG,
    ));
    let gc = s.pid("gc").await;
    let _reaper = Reaper(vec![gc]);
    // Give the periodic scan time to see the descendant. Interrupting at once
    // races the scan, which is a known gap (delega issue 1).
    tokio::time::sleep(Duration::from_millis(1500)).await;
    handle.interrupt();
    let report = handle.wait().await;
    assert_eq!(report.ending, Ending::Interrupted, "{report:#?}");
    assert_confirmed(&report);
    assert_gone("descendant in its own group", &[gc]).await;
}

#[tokio::test]
async fn a_pipe_held_by_an_unobserved_process_does_not_hold_the_report() {
    let s = Scratch::new();
    // The sleep is in its own group and its parent exits at once, so it is not
    // known to the tracker. It keeps the pipes open: the report still comes
    // back, within the drain bound, and says the owner closed the pipes. The
    // process itself is the known gap (delega issue 1) and is stopped by the
    // test.
    let spec = sh(
        &s,
        "set -m; echo $$ > leader; (exec sleep 60) & echo $! > gc; exit 0",
        LONG,
    );
    let started = Instant::now();
    let handle = tokio::spawn(run(spec));
    let gc = s.pid("gc").await;
    let _reaper = Reaper(vec![gc]);
    let report = handle.await.expect("run task");
    assert_eq!(report.ending, Ending::Exited { code: 0 });
    assert!(started.elapsed() < BOUND, "took {:?}", started.elapsed());
    assert!(report.diagnostics.pipes_closed_by_owner, "{report:#?}");
    assert!(
        report
            .diagnostics
            .warnings
            .iter()
            .any(|w| w == PIPE_WARNING),
        "{:?}",
        report.diagnostics.warnings
    );
}

#[tokio::test]
async fn cleanup_that_cannot_be_verified_is_reported_unconfirmed_within_the_deadline() {
    let s = Scratch::new();
    let broken: Lister =
        Arc::new(|| Box::pin(async { Err(InspectError::Failed("listing unavailable".into())) }));
    let mut spec = sh(&s, "echo $$ > leader; exit 0", LONG).with_lister(broken);
    spec.cleanup_deadline = Duration::from_millis(600);
    let started = Instant::now();
    let report = run(spec).await;
    assert_eq!(report.ending, Ending::Exited { code: 0 }, "{report:#?}");
    assert!(started.elapsed() < BOUND, "took {:?}", started.elapsed());
    let leader = s.pid_now("leader") as u32;
    match &report.cleanup {
        Cleanup::Unconfirmed {
            process_groups,
            reason,
            ..
        } => {
            assert_eq!(process_groups, &[leader]);
            assert!(reason.contains("process inspection failed"), "{reason}");
        }
        Cleanup::Confirmed => panic!("a failed listing must not confirm cleanup: {report:#?}"),
    }
    assert!(report.diagnostics.capture_gap_observed);
    assert!(report.diagnostics.inspection.failures > 0);
    assert_gone("leader", &[leader as i32]).await;
}

#[tokio::test]
async fn without_descendant_tracking_the_child_group_is_still_cleaned_up() {
    let s = Scratch::new();
    let mut spec = sh(
        &s,
        "echo $$ > leader; sleep 60 & echo $! > gc; exit 0",
        LONG,
    );
    spec.track_descendants = false;
    let report = run(spec).await;
    assert_eq!(report.ending, Ending::Exited { code: 0 });
    assert!(!report.diagnostics.tracking_enabled);
    assert!(report.diagnostics.capture_gap_observed);
    assert_gone("grandchild", &[s.pid_now("gc")]).await;
}

#[tokio::test]
async fn the_report_always_says_unobserved_descendants_are_possible() {
    let s = Scratch::new();
    let report = run(sh(&s, "exit 0", LONG)).await;
    assert!(report.diagnostics.unobserved_descendants_possible);
}
