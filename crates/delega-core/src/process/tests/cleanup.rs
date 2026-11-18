//! Descendants, held pipes and the cleanup verdict.

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::fixtures::{
    OWN_GROUP, OWN_SESSION, Reaper, Scratch, WAIT_ESCAPED, assert_confirmed, assert_gone,
    run_guarded, sh, wait_own_group,
};
use crate::contract::result::Cleanup;
use crate::process::tree::{InspectError, Lister, lister_for};
use crate::process::{Ending, PIPE_WARNING, ProcessHandle, run};

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
    let (report, pids, _reaper) = run_guarded(&s, spec, &["leader", "gc"]).await;
    assert!(
        matches!(report.ending, Ending::Signaled { signal: 9, .. }),
        "{report:#?}"
    );
    assert!(started.elapsed() < BOUND, "took {:?}", started.elapsed());
    assert!(!report.diagnostics.pipes_closed_by_owner, "{report:#?}");
    assert_confirmed(&report);
    assert_gone("leader and descendant", &pids).await;
}

#[tokio::test]
async fn a_grandchild_that_outlives_its_parent_in_the_same_group_is_cleaned_up() {
    let s = Scratch::new();
    let spec = sh(
        &s,
        "echo $$ > leader; sleep 60 & echo $! > gc; exit 0",
        LONG,
    );
    let (report, pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
    assert_eq!(report.ending, Ending::Exited { code: 0 });
    assert_confirmed(&report);
    assert_gone("grandchild", &pids).await;
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
    let (report, pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
    assert_eq!(report.ending, Ending::Exited { code: 0 });
    assert!(started.elapsed() < BOUND, "took {:?}", started.elapsed());
    assert_confirmed(&report);
    assert_gone("grandchild", &pids).await;
}

#[tokio::test]
async fn a_descendant_in_another_process_group_is_cleaned_up_by_the_session_sweep() {
    require_perl!();
    let s = Scratch::new();
    // Its own group, the child's session: the group signal cannot reach it, the
    // session sweep can, whether or not a scan ever saw it.
    let script = format!("echo $$ > leader; {OWN_GROUP} & echo $! > gc; exit 0");
    let (report, pids, _reaper) = run_guarded(&s, sh(&s, &script, LONG), &["gc"]).await;
    assert_eq!(report.ending, Ending::Exited { code: 0 }, "{report:#?}");
    assert_confirmed(&report);
    assert_gone("descendant in its own group", &pids).await;
}

#[tokio::test]
async fn a_descendant_in_its_own_session_is_found_by_the_scan_and_cleaned_up() {
    require_perl!();
    let s = Scratch::new();
    // A session of its own is beyond the sweep. Only a scan that saw it while
    // its parent lived can track it. The leader lives long enough for two.
    let script = format!("echo $$ > leader; {OWN_SESSION} & echo $! > gc; sleep 1.3; exit 0");
    let (report, pids, _reaper) = run_guarded(&s, sh(&s, &script, LONG), &["gc"]).await;
    assert_eq!(report.ending, Ending::Exited { code: 0 }, "{report:#?}");
    assert_confirmed(&report);
    assert!(report.diagnostics.inspection.retained_descendant_identities >= 1);
    assert_gone("descendant in its own session", &pids).await;
}

#[tokio::test]
async fn interrupting_stops_a_tracked_descendant_that_holds_the_pipes_without_waiting_for_the_grace()
 {
    require_perl!();
    let s = Scratch::new();
    // The descendant is in its own session, inherits the pipes and is tracked.
    // It must get TERM when the group does, not when cleanup starts: with a
    // long grace and drain it would otherwise hold the report for seconds.
    let script = format!("echo $$ > leader; {OWN_SESSION} & echo $! > gc; wait");
    let mut spec = sh(&s, &script, LONG);
    spec.stop_grace = Duration::from_secs(3);
    spec.drain_bound = Duration::from_secs(3);
    spec.cleanup_deadline = Duration::from_secs(20);
    let handle = ProcessHandle::start(spec);
    let gc = s.pid("gc").await;
    let _reaper = Reaper(vec![gc]);
    // Let the periodic scan see the descendant.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let stopped = Instant::now();
    handle.interrupt();
    let report = handle.wait().await;
    assert_eq!(report.ending, Ending::Interrupted, "{report:#?}");
    assert!(
        stopped.elapsed() < Duration::from_millis(1500),
        "stopping took {:?}",
        stopped.elapsed()
    );
    assert_confirmed(&report);
    assert_gone("tracked descendant", &[gc]).await;
}

#[tokio::test]
async fn a_descendant_in_its_own_session_started_just_before_an_interrupt_needs_the_pre_stop_scan()
{
    require_perl!();
    let s = Scratch::new();
    // The descendant starts after the first scan and the interrupt comes before
    // the next periodic one, so only the scan a stop makes first can see it.
    let script = format!(
        "while [ ! -e scanned ]; do sleep 0.02; done; echo $$ > leader; \
         {OWN_SESSION} & echo $! > gc; wait"
    );
    let spec = sh(&s, &script, LONG).with_lister(super::gaps::marking_lister(s.path("scanned")));
    let handle = ProcessHandle::start(spec);
    let gc = s.pid("gc").await;
    let _reaper = Reaper(vec![gc]);
    // Without this the sweep could catch it before it has left the session.
    wait_own_group(gc).await;
    handle.interrupt();
    let report = handle.wait().await;
    assert_eq!(report.ending, Ending::Interrupted, "{report:#?}");
    assert_gone("descendant started just before the interrupt", &[gc]).await;
}

#[tokio::test]
async fn an_own_group_descendant_started_just_before_an_interrupt_is_stopped_every_time() {
    require_perl!();
    for round in 0..5 {
        let s = Scratch::new();
        let script = format!("echo $$ > leader; {OWN_GROUP} & echo $! > gc; wait");
        let handle = ProcessHandle::start(sh(&s, &script, LONG));
        let gc = s.pid("gc").await;
        let _reaper = Reaper(vec![gc]);
        handle.interrupt();
        let report = handle.wait().await;
        assert_eq!(
            report.ending,
            Ending::Interrupted,
            "round {round}: {report:#?}"
        );
        assert_confirmed(&report);
        assert_gone(&format!("round {round}"), &[gc]).await;
    }
}

#[tokio::test]
async fn a_leader_that_keeps_forking_own_group_children_leaves_none_behind() {
    require_perl!();
    let s = Scratch::new();
    let script = "exec perl -e 'use POSIX; for (1..400) { my $p = fork(); \
        if (!$p) { setpgid(0,0); exec q(sleep), q(60) } \
        open F, q(>>), q(kids); print F qq($p\\n); close F; \
        select(undef, undef, undef, 0.05); }'";
    let handle = ProcessHandle::start(sh(&s, script, LONG));
    let _ = s.pid("kids").await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    handle.interrupt();
    let report = handle.wait().await;
    assert_eq!(report.ending, Ending::Interrupted, "{report:#?}");
    let kids: Vec<i32> = std::fs::read_to_string(s.path("kids"))
        .expect("kids file")
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect();
    let _reaper = Reaper(kids.clone());
    assert!(kids.len() >= 3, "only {} children were started", kids.len());
    assert_confirmed(&report);
    assert_gone("children forked until the stop", &kids).await;
}

#[tokio::test]
async fn a_terminate_handler_that_starts_a_child_in_its_own_group_leaves_none_behind() {
    require_perl!();
    let s = Scratch::new();
    let script = format!(
        "trap \"{OWN_GROUP} & echo \\$! > gc\" TERM; echo $$ > leader; \
         while :; do sleep 0.05; done"
    );
    let handle = ProcessHandle::start(sh(&s, &script, LONG));
    let leader = s.pid("leader").await;
    let _reaper = Reaper(vec![leader]);
    handle.interrupt();
    let report = handle.wait().await;
    assert_eq!(report.ending, Ending::Interrupted, "{report:#?}");
    let gc = s.pid_now("gc");
    let _reaper_gc = Reaper(vec![gc]);
    assert_confirmed(&report);
    assert_gone("child started by the handler", &[leader, gc]).await;
}

#[tokio::test]
async fn a_pipe_held_by_a_process_that_left_the_session_does_not_hold_the_report() {
    require_perl!();
    let s = Scratch::new();
    // The sleep calls setsid and its parent exits at once, so neither the
    // sweep nor a scan can know it. It keeps the pipes open: the report still
    // comes back within the drain bound and says the owner closed the pipes.
    // The process itself is the known gap (delega issue 1) and the test stops
    // it. Today's verdict for it is Confirmed, which is that gap; the ignored
    // tests in gaps.rs state the wanted one.
    // It starts after the first scan, and the leader exits before the next, so
    // no scan can have seen it with its parent alive.
    let script = format!(
        "while [ ! -e scanned ]; do sleep 0.02; done; echo $$ > leader; \
         {OWN_SESSION} & echo $! > gc; {WAIT_ESCAPED} exit 0"
    );
    let spec = sh(&s, &script, LONG).with_lister(super::gaps::marking_lister(s.path("scanned")));
    let started = Instant::now();
    let (report, _pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
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
    assert_eq!(report.cleanup, Cleanup::Confirmed, "{report:#?}");
}

#[tokio::test]
async fn failing_inspection_finishes_quickly_as_unconfirmed_and_says_what_was_checked() {
    let s = Scratch::new();
    let broken: Lister =
        Arc::new(|| Box::pin(async { Err(InspectError::Failed("listing unavailable".into())) }));
    let mut spec = sh(&s, "echo $$ > leader; exit 0", LONG).with_lister(broken);
    spec.cleanup_deadline = Duration::from_secs(30);
    let started = Instant::now();
    let report = run(spec).await;
    assert_eq!(report.ending, Ending::Exited { code: 0 }, "{report:#?}");
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "took {:?}",
        started.elapsed()
    );
    match &report.cleanup {
        Cleanup::Unconfirmed {
            process_groups,
            reason,
            ..
        } => {
            assert!(process_groups.is_empty(), "{process_groups:?}");
            assert!(
                reason.contains("process inspection was unavailable"),
                "{reason}"
            );
            assert!(reason.contains("kill(-pgid, 0)"), "{reason}");
            assert!(
                reason.contains("no process has the child's group id"),
                "{reason}"
            );
        }
        Cleanup::Confirmed => panic!("a failed listing must not confirm cleanup: {report:#?}"),
    }
    assert!(report.diagnostics.capture_gap_observed);
    assert_gone("leader", &[s.pid_now("leader")]).await;
}

#[tokio::test]
async fn a_missing_ps_program_finishes_fast_and_still_stops_the_group() {
    let s = Scratch::new();
    let missing = lister_for(vec!["/nonexistent/delega-ps".into()]);
    let mut spec = sh(
        &s,
        "echo $$ > leader; sleep 60 & echo $! > gc; exit 0",
        LONG,
    )
    .with_lister(missing);
    spec.cleanup_deadline = Duration::from_secs(30);
    let started = Instant::now();
    let handle = ProcessHandle::start(spec);
    let gc = s.pid("gc").await;
    let _reaper = Reaper(vec![gc]);
    let report = handle.wait().await;
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "took {:?}",
        started.elapsed()
    );
    match &report.cleanup {
        Cleanup::Unconfirmed { reason, .. } => {
            assert!(reason.contains("ps could not be started"), "{reason}");
            assert!(
                reason.contains("only the child's process group was checked"),
                "{reason}"
            );
        }
        Cleanup::Confirmed => panic!("no inspection cannot confirm: {report:#?}"),
    }
    assert_gone("same-group grandchild", &[gc]).await;
}

#[tokio::test]
async fn without_descendant_tracking_the_group_and_session_are_still_swept() {
    let s = Scratch::new();
    let mut spec = sh(
        &s,
        "echo $$ > leader; sleep 60 & echo $! > gc; exit 0",
        LONG,
    );
    spec.track_descendants = false;
    let (report, pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
    assert_eq!(report.ending, Ending::Exited { code: 0 });
    assert!(!report.diagnostics.tracking_enabled);
    assert!(report.diagnostics.capture_gap_observed);
    // The sweep found the group and session clean, which is what confirms.
    assert_confirmed(&report);
    assert_gone("grandchild", &pids).await;
}

#[tokio::test]
async fn without_descendant_tracking_a_session_member_outside_the_group_is_swept() {
    require_perl!();
    let s = Scratch::new();
    // Wait until the descendant has left the group, so the group signal cannot
    // be what reaches it.
    let script = format!(
        "echo $$ > leader; {OWN_GROUP} & echo $! > gc; \
         until [ \"$(ps -o pgid= -p $! | tr -d ' ')\" != \"$$\" ]; do sleep 0.02; done; exit 0"
    );
    let mut spec = sh(&s, &script, LONG);
    spec.track_descendants = false;
    let (report, pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
    assert_confirmed(&report);
    assert_eq!(
        report.diagnostics.session_members_swept,
        vec![pids[0] as u32]
    );
    assert_gone("session member", &pids).await;
}

#[tokio::test]
async fn the_report_does_not_claim_more_than_the_sweep_found() {
    let s = Scratch::new();
    let report = run(sh(&s, "exit 0", LONG)).await;
    assert!(report.diagnostics.session_members_swept.is_empty());
    assert_confirmed(&report);
}

#[tokio::test]
async fn the_repeating_group_kill_stops_a_child_when_inspection_hangs_and_early_kills_are_lost() {
    let s = Scratch::new();
    // Inspection never returns, so only the timer that repeats the group KILL
    // can stop the child; the first two KILLs are made to do nothing.
    let hung: Lister = Arc::new(|| {
        Box::pin(async {
            tokio::time::sleep(Duration::from_secs(60)).await;
            Err(InspectError::TimedOut)
        })
    });
    let mut spec = sh(
        &s,
        "trap '' TERM; echo $$ > leader; sleep 60 & echo $! > gc; wait",
        Duration::from_millis(300),
    )
    .with_lister(hung);
    spec.swallow_group_kills = 2;
    spec.cleanup_deadline = Duration::from_secs(3);
    let (report, pids, _reaper) = run_guarded(&s, spec, &["leader", "gc"]).await;
    assert_eq!(
        report.outcome(),
        crate::contract::outcome::Outcome::TimedOut
    );
    // The owner reaped the leader before it gave up, so a KILL reached it.
    assert!(
        report.exit.as_ref().is_some_and(|e| e.signal == Some(9)),
        "{report:#?}"
    );
    assert_gone("leader and sleep", &pids).await;
    assert!(matches!(report.cleanup, Cleanup::Unconfirmed { .. }));
}
