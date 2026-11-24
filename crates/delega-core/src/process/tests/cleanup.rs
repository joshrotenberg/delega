//! Descendants, held pipes and the cleanup verdict.

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::fixtures::{
    Heard, Lifeline, OWN_GROUP, Reaper, Scratch, assert_confirmed, assert_gone, await_event,
    await_record, is_running, own_group_recorded, own_session_recorded, run_guarded, sh,
    wait_tracked,
};
use crate::contract::result::Cleanup;
use crate::process::sys::Signal;
use crate::process::tree::{InspectError, Lister, lister_for};
use crate::process::{Ending, PIPE_WARNING, ProcessHandle, ProcessReport, run};
use crate::test_spawn::Guarded;

const LONG: Duration = Duration::from_secs(60);
/// The stop and cleanup must beat the one-minute child lifetime or grace.
/// The handler path has several real process listings and no completion
/// event for a grace wait when there is nothing left to signal. It took under
/// one second at load 200 in issue 61, leaving over 29 seconds for scheduling
/// and listing delays.
const BOUND: Duration = Duration::from_secs(30);

/// The report, or a failure once [`BOUND`] has passed since `began`: a
/// cleanup that does not end (a descendant holding the report, say) fails
/// the test within its bound, not at the run's cleanup deadline.
async fn wait_for_cleanup_report(handle: ProcessHandle, began: Instant) -> ProcessReport {
    tokio::time::timeout_at(tokio::time::Instant::from_std(began + BOUND), handle.wait())
        .await
        .unwrap_or_else(|_| panic!("no report within {BOUND:?} of the stop"))
}

async fn assert_stopped_in_time(what: &str, pids: &[i32], began: Instant) {
    let deadline = tokio::time::Instant::from_std(began + BOUND);
    assert!(
        began.elapsed() < BOUND,
        "{what}: report took {:?}",
        began.elapsed()
    );
    tokio::time::timeout_at(deadline, assert_gone(what, pids))
        .await
        .unwrap_or_else(|_| panic!("{what}: still running {BOUND:?} after the stop"));
}

/// Release the children even if waiting for a PID or the report panics.
struct ChildLifetime<'a>(&'a Scratch);

impl<'a> ChildLifetime<'a> {
    fn new(scratch: &'a Scratch) -> Self {
        std::fs::write(scratch.path("keep"), "").expect("write the child lifetime file");
        Self(scratch)
    }
}

impl Drop for ChildLifetime<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.0.path("keep"));
    }
}

/// The fixture keeps these children alive until signalled or torn down.
const WAIT_WHILE_GUARDED: &str = "while (-e q(keep)) { select(undef, undef, undef, 0.1) }";

#[tokio::test]
async fn a_crash_while_a_descendant_holds_the_output_pipe_does_not_hang() {
    let s = Scratch::new();
    // The background sleep inherits both pipes and outlives its parent.
    let spec = sh(
        &s,
        "record leader $$; sleep 60 & record gc $!; kill -KILL $$; sleep 5",
        LONG,
    );
    let started = Instant::now();
    let (report, pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
    let leader = s.pid_now("leader");
    assert!(
        matches!(report.ending, Ending::Signaled { signal: 9, .. }),
        "{report:#?}"
    );
    assert_stopped_in_time("leader and descendant", &[leader, pids[0]], started).await;
    assert!(!report.diagnostics.pipes_closed_by_owner, "{report:#?}");
    assert_confirmed(&report);
}

#[tokio::test]
async fn a_grandchild_that_outlives_its_parent_in_the_same_group_is_cleaned_up() {
    let s = Scratch::new();
    let spec = sh(
        &s,
        "record leader $$; sleep 60 & record gc $!; exit 0",
        LONG,
    );
    let started = Instant::now();
    let (report, pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
    assert_eq!(report.ending, Ending::Exited { code: 0 });
    assert_confirmed(&report);
    assert_stopped_in_time("grandchild", &pids, started).await;
}

#[tokio::test]
async fn a_grandchild_that_ignores_terminate_is_killed_after_its_parent_exits() {
    let s = Scratch::new();
    let spec = sh(
        &s,
        "record leader $$; (trap '' TERM; exec sleep 60) & record gc $!; exit 0",
        LONG,
    );
    let started = Instant::now();
    let (report, pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
    assert_eq!(report.ending, Ending::Exited { code: 0 });
    assert_confirmed(&report);
    assert_stopped_in_time("grandchild", &pids, started).await;
}

both!(
    a_descendant_in_another_process_group_is_cleaned_up_by_the_session_sweep,
    a_descendant_in_another_process_group_is_cleaned_up_by_the_session_sweep_kept,
    async {
        require_perl!();
        let s = Scratch::new();
        // Its own group, the child's session: the group signal cannot reach it, the
        // session sweep can, whether or not a scan ever saw it. The leader exits
        // once the descendant has said it left the group.
        let script = format!(
            "record leader $$; {} & {} exit 0",
            own_group_recorded("gc"),
            await_record("gc")
        );
        let started = Instant::now();
        let (report, pids, _reaper) = run_guarded(&s, sh(&s, &script, LONG), &["gc"]).await;
        assert_eq!(report.ending, Ending::Exited { code: 0 }, "{report:#?}");
        assert_confirmed(&report);
        assert_stopped_in_time("descendant in its own group", &pids, started).await;
    }
);

both!(
    a_descendant_in_its_own_session_is_found_by_the_scan_and_cleaned_up,
    a_descendant_in_its_own_session_is_found_by_the_scan_and_cleaned_up_kept,
    async {
        require_perl!();
        let s = Scratch::new();
        // A session of its own is beyond the sweep. Only a scan that saw it while
        // its parent lived can track it. The leader stays until the test has seen
        // the scan track the descendant, however long that takes, then exits.
        // The descendant publishes `gc` only once it has left the session.
        let script = format!(
            "record leader $$; {} & while [ ! -e go ]; do sleep 0.02; done; exit 0",
            own_session_recorded("gc")
        );
        let handle = ProcessHandle::start(sh(&s, &script, LONG));
        let mut reaper = Reaper::default();
        let gc = reaper.watch(&s, "gc").await;
        wait_tracked(&handle, gc).await;
        let stopped = Instant::now();
        std::fs::write(s.path("go"), "go").expect("write the go file");
        let report = wait_for_cleanup_report(handle, stopped).await;
        assert_eq!(report.ending, Ending::Exited { code: 0 }, "{report:#?}");
        assert_confirmed(&report);
        assert!(report.diagnostics.inspection.retained_descendant_identities >= 1);
        assert_stopped_in_time("descendant in its own session", &[gc], stopped).await;
    }
);

both!(
    interrupting_stops_a_tracked_descendant_that_holds_the_pipes_without_waiting_for_the_grace,
    interrupting_stops_a_tracked_descendant_that_holds_the_pipes_without_waiting_for_the_grace_kept,
    async {
        require_perl!();
        let s = Scratch::new();
        // The descendant is in its own session, inherits the pipes and is tracked.
        // It must get TERM when the group does, not when cleanup starts: with a
        // long grace and drain it would otherwise hold the report for a minute.
        // It publishes `gc` only once it has left the session.
        let script = format!("record leader $$; {} & wait", own_session_recorded("gc"));
        let mut spec = sh(&s, &script, LONG);
        spec.stop_grace = Duration::from_secs(60);
        spec.drain_bound = Duration::from_secs(60);
        spec.cleanup_deadline = Duration::from_secs(180);
        let handle = ProcessHandle::start(spec);
        let mut reaper = Reaper::default();
        let gc = reaper.watch(&s, "gc").await;
        // Let the periodic scan see the descendant.
        wait_tracked(&handle, gc).await;
        let stopped = Instant::now();
        handle.interrupt();
        let report = wait_for_cleanup_report(handle, stopped).await;
        assert_eq!(report.ending, Ending::Interrupted, "{report:#?}");
        assert_confirmed(&report);
        assert_stopped_in_time("tracked descendant", &[gc], stopped).await;
    }
);

both!(
    a_descendant_in_its_own_session_started_just_before_an_interrupt_needs_the_pre_stop_scan,
    a_descendant_in_its_own_session_started_just_before_an_interrupt_needs_the_pre_stop_scan_kept,
    async {
        require_perl!();
        let s = Scratch::new();
        // The descendant starts after the first scan and the interrupt comes before
        // the next periodic one, so only the scan a stop makes first can see it.
        // It publishes `gc` only once it has left the session, so the sweep
        // cannot catch it in the session.
        let script = format!(
            "while [ ! -e scanned ]; do sleep 0.02; done; record leader $$; {} & wait",
            own_session_recorded("gc")
        );
        let mut spec =
            sh(&s, &script, LONG).with_lister(super::gaps::marking_lister(s.path("scanned")));
        // No periodic scan after the first, however slow the machine is.
        spec.capture_interval = Some(Duration::from_secs(3600));
        let handle = ProcessHandle::start(spec);
        let mut reaper = Reaper::default();
        let gc = reaper.watch(&s, "gc").await;
        let stopped = Instant::now();
        handle.interrupt();
        let report = wait_for_cleanup_report(handle, stopped).await;
        assert_eq!(report.ending, Ending::Interrupted, "{report:#?}");
        assert_stopped_in_time(
            "descendant started just before the interrupt",
            &[gc],
            stopped,
        )
        .await;
    }
);

both!(
    an_own_group_descendant_started_just_before_an_interrupt_is_stopped_every_time,
    an_own_group_descendant_started_just_before_an_interrupt_is_stopped_every_time_kept,
    async {
        require_perl!();
        for round in 0..5 {
            let s = Scratch::new();
            let script = format!("record leader $$; {OWN_GROUP} & record gc $!; wait");
            let handle = ProcessHandle::start(sh(&s, &script, LONG));
            let mut reaper = Reaper::default();
            let gc = reaper.watch(&s, "gc").await;
            // Interrupt as soon as the fork is recorded. The child may still
            // be in the leader's group or may already have left it.
            let stopped = Instant::now();
            handle.interrupt();
            let report = handle.wait().await;
            assert_eq!(
                report.ending,
                Ending::Interrupted,
                "round {round}: {report:#?}"
            );
            assert_confirmed(&report);
            assert_stopped_in_time(&format!("round {round}"), &[gc], stopped).await;
        }
    }
);

both!(
    a_leader_that_keeps_forking_own_group_children_leaves_none_behind,
    a_leader_that_keeps_forking_own_group_children_leaves_none_behind_kept,
    async {
        require_perl!();
        let s = Scratch::new();
        // The leader records each child after it forks it, so a leader stopped
        // between the fork and the record leaves a child without one. The
        // leader opens the lifeline before its first fork, so every child holds
        // it from the fork on: the lifeline ends only once the leader and every
        // child it forked are gone, recorded or not.
        let mut lifeline = Lifeline::new(&s, "life");
        let script = format!(
            "exec perl -e 'use POSIX; open W, q(>), q(life) or die; \
         open L, q(>), q(leader); print L qq($$\n); close L; \
         for (1..400) {{ my $p = fork(); \
         if (!defined $p) {{ next }} \
         if (!$p) {{ setpgid(0,0); {WAIT_WHILE_GUARDED}; exit 0 }} \
         open F, q(>), qq(kids/$p); print F qq($p\n); close F; \
         select(undef, undef, undef, 0.05); }}'"
        );
        std::fs::create_dir(s.path("kids")).expect("create the record directory");
        let _reaper = ChildLifetime::new(&s);
        let handle = ProcessHandle::start(sh(&s, &script, LONG));
        await_event("three children being started", || {
            s.published_pids_in("kids").0.len() >= 3
        })
        .await;
        lifeline
            .await_opened("the leader opening the lifeline")
            .await;
        let stopped = Instant::now();
        handle.interrupt();
        let report = wait_for_cleanup_report(handle, stopped).await;
        assert_eq!(report.ending, Ending::Interrupted, "{report:#?}");
        let (kids, _) = s.published_pids_in("kids");
        assert!(kids.len() >= 3, "only {} children were started", kids.len());
        assert_confirmed(&report);
        assert_stopped_in_time("children forked until the stop", &kids, stopped).await;
        // The children wait until the lifetime file is removed, so one that
        // outlived the cleanup holds the lifeline open until the bound.
        let left = stopped + BOUND;
        let ended = lifeline
            .hear_within(left.saturating_duration_since(Instant::now()))
            .await;
        assert_eq!(
            ended,
            Some(Heard::Ended),
            "a process the leader forked, recorded or not, was still running {BOUND:?} after the stop"
        );
    }
);

/// A child in its own group with no pipe shared with the leader. It stays
/// alive until signalled or the reaper removes its lifetime file.
fn detached_own_group() -> String {
    format!("perl -e 'use POSIX; setpgid(0,0); {WAIT_WHILE_GUARDED}' >/dev/null 2>&1 </dev/null")
}

both!(
    a_terminate_handler_that_starts_a_child_in_its_own_group_leaves_none_behind,
    a_terminate_handler_that_starts_a_child_in_its_own_group_leaves_none_behind_kept,
    async {
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
            "trap \"{child} & record gc \\$!; exit 0\" TERM; record leader $$; \
         while :; do sleep 0.05; done"
        );
        let mut spec = sh(&s, &script, LONG);
        spec.stop_grace = Duration::from_secs(60);
        let _reaper = ChildLifetime::new(&s);
        let handle = ProcessHandle::start(spec);
        let leader = s.pid("leader").await;
        let asked = Instant::now();
        handle.interrupt();
        let report = handle.wait().await;
        assert_eq!(report.ending, Ending::Interrupted, "{report:#?}");
        let gc = s.pid_now("gc");
        assert_confirmed(&report);
        assert_stopped_in_time("child started by the handler", &[leader, gc], asked).await;
    }
);

/// How many runs of its TERM handler the leader of the test below has
/// acknowledged: the highest `N` of the `handled-N` files its loop creates
/// between handlers. Two handlers can run back to back (a TERM that arrives
/// during one is delivered as it ends), so a count can be skipped.
fn handlers_finished(s: &Scratch) -> u32 {
    std::fs::read_dir(s.dir())
        .expect("read the scratch directory")
        .filter_map(|entry| {
            let name = entry.expect("read a scratch entry").file_name();
            name.to_str()?.strip_prefix("handled-")?.parse().ok()
        })
        .max()
        .unwrap_or(0)
}

both!(a_leader_that_survives_its_terminate_handler_and_starts_a_child_on_each_term_leaves_none_behind, a_leader_that_survives_its_terminate_handler_and_starts_a_child_on_each_term_leaves_none_behind_kept, async {
    require_perl!();
    let s = Scratch::new();
    // The scenario of the test above with a leader that keeps running after its
    // handler, so the handler runs again for every TERM it receives and each run
    // starts another child in a group of its own and records it. The stop sends
    // one TERM. The test sends the leader another once the first handler has
    // finished, so at least two children exist by construction whatever the
    // order of the sweep and the leader. A TERM that arrives while the handler
    // runs is delivered only as it ends, so the handler counts its runs in
    // `n` and the leader's loop, which runs only between handlers,
    // acknowledges the runs finished so far by creating `handled-N`
    // (`handlers_finished`). A TERM sent after that acknowledgement runs the
    // handler again. The leader ends when the test says so (a file it
    // writes), not after a grace that load could make too short: the grace is
    // a minute. The children do not hold the output pipes.
    let child = detached_own_group();
    let script = format!(
        "n=0; trap \"{child} & record gcs/\\$! \\$!; n=\\$((n + 1))\" TERM; record leader $$; \
         while [ ! -e quit ]; do : > handled-$n; sleep 0.05; done"
    );
    let mut spec = sh(&s, &script, LONG);
    spec.stop_grace = Duration::from_secs(60);
    std::fs::create_dir(s.path("gcs")).expect("create the record directory");
    let _reaper = ChildLifetime::new(&s);
    let handle = ProcessHandle::start(spec);
    let leader = s.pid("leader").await;
    let mut reaper = Reaper::default();
    let child = reaper.register(leader, &s).expect("identify the leader in the test scratch directory");
    handle.interrupt();
    await_event("the leader finishing the handler of the stop's TERM", || {
        handlers_finished(&s) >= 1
    })
    .await;
    assert!(child.signal(Signal::TERM), "send the leader a second TERM");
    await_event("the leader finishing the handler of a second TERM", || {
        handlers_finished(&s) >= 2
    })
    .await;
    let quit_at = Instant::now();
    std::fs::write(s.path("quit"), "quit").expect("write the quit file");
    let report = handle.wait().await;
    assert_eq!(report.ending, Ending::Interrupted, "{report:#?}");
    // The leader ran every handler to its end before it saw the quit file, so
    // every record it began is complete.
    let (children, incomplete) = s.published_pids_in("gcs");
    assert!(incomplete.is_empty(), "incomplete records: {incomplete:?}");
    assert!(
        children.len() >= 2,
        "{} children were started",
        children.len()
    );
    assert_confirmed(&report);
    assert_stopped_in_time("children started by the handler", &children, quit_at).await;
});

both!(
    a_pipe_held_by_a_process_that_left_the_session_does_not_hold_the_report,
    a_pipe_held_by_a_process_that_left_the_session_does_not_hold_the_report_kept,
    async {
        require_perl!();
        let s = Scratch::new();
        // The sleep calls setsid and its parent exits at once, so neither the
        // sweep nor a scan can know it. It keeps the pipes open: the report still
        // comes back within the drain bound and says the owner closed the pipes,
        // and the verdict is Unconfirmed, because something still held the
        // output that no sweep found. On Linux a kept run finds it anyway (it
        // becomes the keeper's child), kills it, and the pipes close by
        // themselves. On macOS the same holds where `ps -E` shows the run marker
        // of `sleep`; where the system hides the environment of its own
        // programs (System Integrity Protection) it is the gap, and the held
        // pipe makes it Unconfirmed. Never Confirmed with the orphan alive.
        // It starts after the first scan, and the leader exits before the next, so
        // no scan can have seen it with its parent alive.
        // It publishes `gc` once it has left the session, and the leader exits
        // then.
        let script = format!(
            "while [ ! -e scanned ]; do sleep 0.02; done; record leader $$; {} & {} exit 0",
            own_session_recorded("gc"),
            await_record("gc")
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
        let kept = super::fixtures::KEEP
            .try_with(|keep| *keep)
            .unwrap_or(false);
        assert!(
            !(report.cleanup == Cleanup::Confirmed && is_running(pids[0])),
            "Confirmed with the orphan alive: {report:#?}"
        );
        if kept && cfg!(target_os = "linux") {
            assert_eq!(report.cleanup, Cleanup::Confirmed, "{report:#?}");
            assert!(!report.diagnostics.pipes_closed_by_owner, "{report:#?}");
            super::fixtures::assert_gone("the orphan holding the pipes", &pids).await;
            return;
        }
        if kept && super::fixtures::marker_visible_for_system_programs() {
            // Found by its marker and stopped. Whether it was killed before
            // or after the drain bound decides between the two honest verdicts.
            super::fixtures::assert_gone("the orphan holding the pipes", &pids).await;
            assert!(
                report.cleanup == Cleanup::Confirmed
                    || matches!(&report.cleanup, Cleanup::Unconfirmed { reason, .. }
                        if reason == crate::process::PIPES_HELD_REASON),
                "{report:#?}"
            );
            return;
        }
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
        match &report.cleanup {
            Cleanup::Unconfirmed { reason, .. } => {
                assert_eq!(reason, crate::process::PIPES_HELD_REASON, "{report:#?}");
            }
            other => panic!("a held output pipe must not confirm cleanup: {other:?}"),
        }
    }
);

both!(
    failing_inspection_finishes_quickly_as_unconfirmed_and_says_what_was_checked,
    failing_inspection_finishes_quickly_as_unconfirmed_and_says_what_was_checked_kept,
    async {
        let s = Scratch::new();
        let broken: Lister = Arc::new(|| {
            Box::pin(async { Err(InspectError::Failed("listing unavailable".into())) })
        });
        let mut spec = sh(&s, "record leader $$; exit 0", LONG).with_lister(broken);
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
);

#[tokio::test]
async fn a_missing_ps_program_finishes_fast_and_still_stops_the_group() {
    let s = Scratch::new();
    let missing = lister_for(vec!["/nonexistent/delega-ps".into()]);
    let mut spec = sh(
        &s,
        "record leader $$; sleep 60 & record gc $!; exit 0",
        LONG,
    )
    .with_lister(missing);
    // A deadline that would show if cleanup waited it out.
    spec.cleanup_deadline = Duration::from_secs(600);
    let started = Instant::now();
    let handle = ProcessHandle::start(spec);
    let mut reaper = Reaper::default();
    let gc = reaper.watch(&s, "gc").await;
    let report = handle.wait().await;
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
    assert_stopped_in_time("same-group grandchild", &[gc], started).await;
}

/// A file in `dir` that exists but may not be executed (mode 0644).
fn unexecutable(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, "#!/bin/sh\nexit 0\n").expect("write refused ps");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
}

fn os_error(errno: rustix::io::Errno) -> String {
    std::io::Error::from_raw_os_error(errno.raw_os_error()).to_string()
}

async fn unavailable_message(programs: &[std::path::PathBuf]) -> String {
    let error = crate::process::tree::list_processes_from(programs)
        .await
        .expect_err("no program can start");
    match error {
        InspectError::Unavailable(message) => message,
        other => panic!("a program that cannot start is unavailable inspection: {other:?}"),
    }
}

#[tokio::test]
async fn a_refused_ps_is_reported_as_refused_not_as_missing() {
    let s = Scratch::new();
    // A listing program that exists but may not be executed, tried before a
    // missing one, as a sandbox that refuses `/bin/ps` presents it. Both live
    // in this test's private directory, and the second is never created.
    let refused = s.path("refused-ps");
    unexecutable(&refused);
    let missing = s.path("never-created-ps");
    let message = unavailable_message(&[refused.clone(), missing.clone()]).await;
    let refused_text = format!(
        "{}: {}",
        refused.display(),
        os_error(rustix::io::Errno::ACCESS)
    );
    let missing_text = format!(
        "{}: {}",
        missing.display(),
        os_error(rustix::io::Errno::NOENT)
    );
    assert_eq!(
        message,
        format!("ps could not be started ({refused_text}, {missing_text})")
    );
}

#[test]
fn an_overlong_ps_start_error_is_cut_to_the_limit_with_the_first_attempt_first() {
    use crate::process::limits::MAX_ERROR_CHARS;
    // Two attempts whose message is far longer than the limit. The first path
    // holds multi-byte characters, so a cut by bytes would split one.
    let first = format!("/{}/ps", "\u{e9}".repeat(600));
    let failures = [
        (
            std::path::PathBuf::from(&first),
            std::io::Error::from_raw_os_error(rustix::io::Errno::ACCESS.raw_os_error()),
        ),
        (
            std::path::PathBuf::from(format!("/{}/ps", "x".repeat(600))),
            std::io::Error::from_raw_os_error(rustix::io::Errno::NOENT.raw_os_error()),
        ),
    ];
    let InspectError::Unavailable(message) = crate::process::tree::not_started(&failures) else {
        panic!("not started is unavailable inspection");
    };
    assert_eq!(message.chars().count(), MAX_ERROR_CHARS, "{message}");
    assert!(message.ends_with("..."), "{message}");
    let start = format!(
        "ps could not be started ({first}: {}, /",
        os_error(rustix::io::Errno::ACCESS)
    );
    assert!(message.starts_with(&start), "{message}");
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
        "record leader $$; sh -c 'trap \"\" TERM; echo $$ > gc; while :; do sleep 1; done' & \
         while [ ! -e gc ]; do sleep 0.02; done; exit 0",
        LONG,
    )
    .with_lister(lister);
    spec.cleanup_deadline = Duration::from_secs(30);
    let handle = ProcessHandle::start(spec);
    let mut reaper = Reaper::default();
    reaper.watch(&s, "gc").await;
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
        "record leader $$; sleep 60 & record gc $!; exit 0",
        LONG,
    );
    spec.track_descendants = false;
    let started = Instant::now();
    let (report, pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
    assert_eq!(report.ending, Ending::Exited { code: 0 });
    assert!(!report.diagnostics.tracking_enabled);
    assert!(report.diagnostics.capture_gap_observed);
    // The sweep found the group and session clean, which is what confirms.
    assert_confirmed(&report);
    assert_stopped_in_time("grandchild", &pids, started).await;
}

both!(
    without_descendant_tracking_a_session_member_outside_the_group_is_swept,
    without_descendant_tracking_a_session_member_outside_the_group_is_swept_kept,
    async {
        require_perl!();
        let s = Scratch::new();
        // Wait until the descendant has left the group, so the group signal cannot
        // be what reaches it: it says so in `gc`.
        let script = format!(
            "record leader $$; {} & {} exit 0",
            own_group_recorded("gc"),
            await_record("gc")
        );
        let mut spec = sh(&s, &script, LONG);
        spec.track_descendants = false;
        let started = Instant::now();
        let (report, pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
        assert_confirmed(&report);
        assert_eq!(
            report.diagnostics.session_members_swept,
            vec![pids[0] as u32]
        );
        assert_stopped_in_time("session member", &pids, started).await;
    }
);

both!(
    the_report_does_not_claim_more_than_the_sweep_found,
    the_report_does_not_claim_more_than_the_sweep_found_kept,
    async {
        let s = Scratch::new();
        let report = run(sh(&s, "exit 0", LONG)).await;
        assert!(report.diagnostics.session_members_swept.is_empty());
        assert_confirmed(&report);
    }
);

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
        "trap '' TERM; record leader $$; sleep 60 & record gc $!; wait",
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
    let mut reaper = Reaper::default();
    let leader = reaper.watch(&s, "leader").await;
    let gc = reaper.watch(&s, "gc").await;
    let pids = vec![leader, gc];
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
                        .guarded_status()
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
