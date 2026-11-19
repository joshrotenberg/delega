//! Descendants, held pipes and the cleanup verdict.

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::fixtures::{
    OWN_GROUP, OWN_SESSION, Reaper, Scratch, WAIT_ESCAPED, assert_confirmed, assert_gone,
    await_event, is_running, run_guarded, sh, wait_own_group, wait_tracked,
};
use crate::contract::result::Cleanup;
use crate::process::sys::{self, Signal};
use crate::process::tree::{InspectError, Lister, lister_for};
use crate::process::{Ending, PIPE_WARNING, ProcessHandle, run};

const LONG: Duration = Duration::from_secs(60);
/// A bound for a run that must not wait for a child that sleeps a minute, or
/// out a grace of that order: only a run that does wait reaches it, and no
/// load adds this much.
const BOUND: Duration = Duration::from_secs(30);

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
    // its parent lived can track it. The leader stays until the test has seen
    // the scan track the descendant, however long that takes, then exits.
    let script = format!(
        "echo $$ > leader; {OWN_SESSION} & echo $! > gc; while [ ! -e go ]; do sleep 0.02; done; exit 0"
    );
    let handle = ProcessHandle::start(sh(&s, &script, LONG));
    let gc = s.pid("gc").await;
    let _reaper = Reaper(vec![gc]);
    wait_tracked(&handle, gc).await;
    std::fs::write(s.path("go"), "go").expect("write the go file");
    let report = handle.wait().await;
    assert_eq!(report.ending, Ending::Exited { code: 0 }, "{report:#?}");
    assert_confirmed(&report);
    assert!(report.diagnostics.inspection.retained_descendant_identities >= 1);
    assert_gone("descendant in its own session", &[gc]).await;
}

#[tokio::test]
async fn interrupting_stops_a_tracked_descendant_that_holds_the_pipes_without_waiting_for_the_grace()
 {
    require_perl!();
    let s = Scratch::new();
    // The descendant is in its own session, inherits the pipes and is tracked.
    // It must get TERM when the group does, not when cleanup starts: with a
    // long grace and drain it would otherwise hold the report for a minute.
    let script = format!("echo $$ > leader; {OWN_SESSION} & echo $! > gc; wait");
    let mut spec = sh(&s, &script, LONG);
    spec.stop_grace = Duration::from_secs(60);
    spec.drain_bound = Duration::from_secs(60);
    spec.cleanup_deadline = Duration::from_secs(180);
    let handle = ProcessHandle::start(spec);
    let gc = s.pid("gc").await;
    let _reaper = Reaper(vec![gc]);
    // Let the periodic scan see the descendant.
    wait_tracked(&handle, gc).await;
    let stopped = Instant::now();
    handle.interrupt();
    let report = handle.wait().await;
    assert_eq!(report.ending, Ending::Interrupted, "{report:#?}");
    assert!(
        stopped.elapsed() < BOUND,
        "stopping took {:?}, which is the grace",
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
    let mut spec =
        sh(&s, &script, LONG).with_lister(super::gaps::marking_lister(s.path("scanned")));
    // No periodic scan after the first, however slow the machine is.
    spec.capture_interval = Some(Duration::from_secs(3600));
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
    // Wait until it has started several, however slowly it forks.
    let started = || std::fs::read_to_string(s.path("kids")).map_or(0, |text| text.lines().count());
    await_event("three children being started", || started() >= 3).await;
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

/// `OWN_GROUP` with its output and input detached. A child that held the pipes
/// the owner reads would keep the run from ending until the grace, a minute
/// here, whenever the leader was still alive at the sweep's snapshot, so the run
/// would take 0.1 s or the grace by timing alone.
fn detached_own_group() -> String {
    format!("{OWN_GROUP} >/dev/null 2>&1 </dev/null")
}

#[tokio::test]
async fn a_terminate_handler_that_starts_a_child_in_its_own_group_leaves_none_behind() {
    require_perl!();
    let s = Scratch::new();
    // The handler starts the child and then ends the leader. The leader is gone
    // as soon as the handler has run, so the stop does not wait for the grace (a
    // minute here). The child does not hold the output pipes, so the run ends
    // when the leader does, whether the sweep saw the leader alive or not. When
    // the leader is gone before the sweep takes its snapshot the child is born
    // after it; when it is alive the sweep sends the child a TERM. A KILL pass
    // after the grace is not involved in either order, but the cleanup pass
    // that follows the exit finds a child born after the snapshot.
    let child = detached_own_group();
    let script = format!(
        "trap \"{child} & echo \\$! > gc; exit 0\" TERM; echo $$ > leader; \
         while :; do sleep 0.05; done"
    );
    let mut spec = sh(&s, &script, LONG);
    spec.stop_grace = Duration::from_secs(60);
    let handle = ProcessHandle::start(spec);
    let leader = s.pid("leader").await;
    let _reaper = Reaper(vec![leader]);
    let asked = Instant::now();
    handle.interrupt();
    let report = handle.wait().await;
    assert!(asked.elapsed() < BOUND, "took {:?}", asked.elapsed());
    assert_eq!(report.ending, Ending::Interrupted, "{report:#?}");
    let gc = s.pid_now("gc");
    let _reaper_gc = Reaper(vec![gc]);
    assert_confirmed(&report);
    assert_gone("child started by the handler", &[leader, gc]).await;
}

#[tokio::test]
async fn a_leader_that_survives_its_terminate_handler_and_starts_a_child_on_each_term_leaves_none_behind()
 {
    require_perl!();
    let s = Scratch::new();
    // The scenario of the test above with a leader that keeps running after its
    // handler, so the handler runs again for every TERM it receives and each run
    // starts another child in a group of its own and records it. The stop sends
    // one TERM. The test sends the leader another, and again until two children
    // are recorded, so at least two children exist by construction whatever the
    // order of the sweep and the leader. A TERM that arrives while the handler
    // runs is merged with it, which is why the test repeats rather than counts
    // on one. The leader ends when the test says so (a file it writes), not
    // after a grace that load could make too short: the grace is a minute. The
    // children do not hold the output pipes.
    let child = detached_own_group();
    let script = format!(
        "trap \"{child} & echo \\$! >> gcs\" TERM; echo $$ > leader; \
         while [ ! -e quit ]; do sleep 0.05; done"
    );
    let mut spec = sh(&s, &script, LONG);
    spec.stop_grace = Duration::from_secs(60);
    let handle = ProcessHandle::start(spec);
    let leader = s.pid("leader").await;
    let _reaper = Reaper(vec![leader]);
    handle.interrupt();
    let recorded = || {
        std::fs::read_to_string(s.path("gcs")).map_or(0, |text| {
            text.lines()
                .filter(|l| l.trim().parse::<i32>().is_ok())
                .count()
        })
    };
    s.exists("gcs").await;
    let mut sent_at = Instant::now();
    await_event("a second child started by a second TERM", || {
        if recorded() >= 2 {
            return true;
        }
        if sent_at.elapsed() >= Duration::from_millis(300) {
            let _ = sys::signal_process(leader, Signal::TERM);
            sent_at = Instant::now();
        }
        false
    })
    .await;
    std::fs::write(s.path("quit"), "quit").expect("write the quit file");
    let quit_at = Instant::now();
    let report = handle.wait().await;
    // Nothing waits for the grace once the leader has ended.
    assert!(quit_at.elapsed() < BOUND, "took {:?}", quit_at.elapsed());
    assert_eq!(report.ending, Ending::Interrupted, "{report:#?}");
    let children: Vec<i32> = std::fs::read_to_string(s.path("gcs"))
        .expect("the children file")
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect();
    let _reaper_children = Reaper(children.clone());
    assert!(
        children.len() >= 2,
        "{} children were started",
        children.len()
    );
    assert_confirmed(&report);
    assert_gone("children started by the handler", &children).await;
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
    let mut spec =
        sh(&s, &script, LONG).with_lister(super::gaps::marking_lister(s.path("scanned")));
    // After the first scan no periodic scan runs, however slow the machine is,
    // so none can see the descendant with its parent alive.
    spec.capture_interval = Some(Duration::from_secs(3600));
    // The descendant holds the pipes for as long as it lives, so the owner
    // always closes them at the drain bound; the bound does not matter here.
    spec.drain_bound = Duration::from_secs(1);
    let started = Instant::now();
    let (report, pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
    assert_eq!(report.ending, Ending::Exited { code: 0 });
    // The report did not wait for the process that holds the pipes: it is
    // alive now, and it sleeps a minute.
    assert!(is_running(pids[0]), "{report:#?}");
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
    // A deadline that would show if cleanup waited it out.
    spec.cleanup_deadline = Duration::from_secs(600);
    let started = Instant::now();
    let report = run(spec).await;
    assert_eq!(report.ending, Ending::Exited { code: 0 }, "{report:#?}");
    assert!(
        started.elapsed() < Duration::from_secs(60),
        "took {:?}, which is the deadline",
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
    // A deadline that would show if cleanup waited it out.
    spec.cleanup_deadline = Duration::from_secs(600);
    let started = Instant::now();
    let handle = ProcessHandle::start(spec);
    let gc = s.pid("gc").await;
    let _reaper = Reaper(vec![gc]);
    let report = handle.wait().await;
    assert!(
        started.elapsed() < Duration::from_secs(60),
        "took {:?}, which is the deadline",
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

/// A `ps` stand-in that prints `output` and exits 0, at a path in `s`.
fn fake_ps(s: &Scratch, output: &str) -> Lister {
    use std::os::unix::fs::PermissionsExt;
    let path = s.path("fake-ps");
    // This script is written and then executed, so on Linux its first exec can
    // fail with ETXTBSY while a fork on another test thread still holds the
    // write descriptor (renaming would not help: the check is on the inode).
    // Every caller expects an unconfirmed cleanup, which a failed listing also
    // gives, so that race cannot fail these tests.
    std::fs::write(&path, format!("#!/bin/sh\nprintf '%s' '{output}'\n")).expect("write fake ps");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    lister_for(vec![path])
}

/// A run whose listing is `ps_output` while a TERM-ignoring child is alive
/// must not be confirmed.
async fn assert_bad_listing_is_unconfirmed(ps_output: &str) {
    let s = Scratch::new();
    let lister = fake_ps(&s, ps_output);
    let mut spec = sh(
        &s,
        "echo $$ > leader; sh -c 'trap \"\" TERM; echo $$ > gc; while :; do sleep 1; done' & \
         while [ ! -e gc ]; do sleep 0.02; done; exit 0",
        LONG,
    )
    .with_lister(lister);
    spec.cleanup_deadline = Duration::from_secs(30);
    let handle = ProcessHandle::start(spec);
    let gc = s.pid("gc").await;
    let _reaper = Reaper(vec![gc]);
    let report = handle.wait().await;
    match &report.cleanup {
        Cleanup::Unconfirmed { reason, .. } => {
            assert!(reason.contains("process inspection"), "{reason}");
        }
        Cleanup::Confirmed => panic!("a bad listing must not confirm cleanup: {report:#?}"),
    }
}

#[tokio::test]
async fn a_listing_program_that_prints_nothing_does_not_confirm_cleanup() {
    assert_bad_listing_is_unconfirmed("").await;
}

#[tokio::test]
async fn a_listing_without_the_owning_process_does_not_confirm_cleanup() {
    assert_bad_listing_is_unconfirmed("1 0 1 S Mon Nov 18 10:00:00 2026\n").await;
}

#[tokio::test]
async fn a_listing_that_is_garbage_does_not_confirm_cleanup() {
    assert_bad_listing_is_unconfirmed("this is not a process table\n").await;
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
    // can stop the child; the first two KILLs are made to do nothing. The stop
    // is an interrupt after the script's files exist (a time limit would race
    // the script's start on a loaded machine); the stop path is the same.
    let hung: Lister = Arc::new(|| {
        Box::pin(async {
            tokio::time::sleep(Duration::from_secs(600)).await;
            Err(InspectError::TimedOut)
        })
    });
    let mut spec = sh(
        &s,
        "trap '' TERM; echo $$ > leader; sleep 60 & echo $! > gc; wait",
        LONG,
    )
    .with_lister(hung);
    spec.swallow_group_kills = 2;
    spec.stop_grace = Duration::from_millis(100);
    spec.drain_bound = Duration::from_secs(2);
    // Raised to the least allowed (grace, drain and a margin), 2.6 s.
    spec.cleanup_deadline = Duration::from_millis(1);
    // The listing before the stop never comes either; the stop goes ahead after
    // this budget.
    spec.timings.stop_listing = Duration::from_millis(300);
    // The sweep that follows each signal waits for the hung listing only this
    // long (the production value), so the owner is not held for minutes.
    spec.timings.descendant_signal = crate::process::limits::DESCENDANT_SIGNAL_BUDGET;
    // The final pass is the real one, one second. This test keeps a wall-clock
    // margin and has no event to wait on instead: the repeated KILL is a timer
    // (every 100 ms) and the pass ends at a deadline, and both are measured by
    // the same clock. Of the second, 250 ms goes to the sweep in `kill_all` (a
    // hung listing, held to the production signal budget) before the repeat
    // timer starts, which leaves about 650 ms of margin for the repeated KILLs
    // to land after the swallowed ones. Load that delays the runtime thread by
    // more than that can end the pass first; the leader is then not killed and
    // the signal assertion below fails.
    spec.timings.final_cleanup_pass = crate::process::limits::FINAL_CLEANUP_PASS;
    let handle = ProcessHandle::start(spec);
    let leader = s.pid("leader").await;
    let gc = s.pid("gc").await;
    let pids = vec![leader, gc];
    let _reaper = Reaper(pids.clone());
    handle.interrupt();
    let report = handle.wait().await;
    assert_eq!(
        report.outcome(),
        crate::contract::outcome::Outcome::Interrupted
    );
    // The owner reaped the leader before it gave up, so a KILL reached it.
    assert!(
        report.exit.as_ref().is_some_and(|e| e.signal == Some(9)),
        "{report:#?}"
    );
    assert_gone("leader and sleep", &pids).await;
    assert!(matches!(report.cleanup, Cleanup::Unconfirmed { .. }));
}

#[tokio::test]
async fn churn_of_short_lived_orphans_does_not_keep_a_plain_run_from_being_confirmed() {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    // Other things on a machine start and orphan short-lived processes all the
    // time. Two threads do that in a loop while plain runs of `true` go on.
    let stop = Arc::new(AtomicBool::new(false));
    let spawned = Arc::new(AtomicU64::new(0));
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let (stop, spawned) = (stop.clone(), spawned.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let ok = std::process::Command::new("/bin/sh")
                        .args(["-c", "(/usr/bin/true &)"])
                        .status()
                        .is_ok_and(|s| s.success());
                    if ok {
                        spawned.fetch_add(1, Ordering::SeqCst);
                    } else {
                        break;
                    }
                }
            })
        })
        .collect();
    // Wait until the churn has started something, or every thread has given up.
    await_event("the churn starting a process or giving up", || {
        spawned.load(Ordering::SeqCst) > 0
            || threads.iter().all(std::thread::JoinHandle::is_finished)
    })
    .await;
    if spawned.load(Ordering::SeqCst) == 0 {
        stop.store(true, Ordering::SeqCst);
        for t in threads {
            let _ = t.join();
        }
        eprintln!("SKIPPED: could not create process churn with /bin/sh and /usr/bin/true");
        return;
    }
    let mut results = Vec::new();
    for _ in 0..5 {
        let spec = crate::process::ProcessSpec::new("/usr/bin/true", LONG)
            .env(crate::process::EnvPolicy::empty());
        let started = Instant::now();
        let report = run(spec).await;
        results.push((report, started.elapsed()));
    }
    stop.store(true, Ordering::SeqCst);
    for t in threads {
        let _ = t.join();
    }
    for (report, took) in results {
        assert_eq!(report.ending, Ending::Exited { code: 0 });
        assert_confirmed(&report);
        assert!(took < BOUND, "took {took:?}");
    }
}
