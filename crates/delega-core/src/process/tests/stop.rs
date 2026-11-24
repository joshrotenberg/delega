//! Stopping: stopped processes, sessions, the pre-stop race and a panic.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::fixtures::{
    Held, OWN_SESSION, Reaper, Scratch, assert_confirmed, assert_gone, await_event, sh,
};
use crate::process::sys::{self, Signal};
use crate::process::tree::Lister;
use crate::process::{Ending, ProcessHandle, ProcessSpec, run};
use crate::test_spawn::Guarded;

const LONG: Duration = Duration::from_secs(60);
/// A bound for a stop that must not wait out a grace or a limit of a minute or
/// more: only a stop that does wait reaches it, and no load adds this much.
const NOT_THE_GRACE: Duration = Duration::from_secs(30);

#[tokio::test]
async fn a_leader_that_stops_itself_is_still_running_and_ends_as_timed_out() {
    let s = Scratch::new();
    // The limit is long against the two commands the child runs before it stops
    // itself. Whether or not it has stopped by then, a child that has not
    // exited is stopped by the limit and reported as timed out.
    let spec = sh(
        &s,
        "echo $$ > leader; kill -STOP $$; sleep 1",
        Duration::from_secs(2),
    );
    // A run that waited for the stopped child to exit would never return.
    let report = tokio::time::timeout(Duration::from_secs(120), run(spec))
        .await
        .expect("the run did not end with a stopped leader");
    assert!(
        matches!(report.ending, Ending::TimedOut { .. }),
        "a stopped child has not ended: {report:#?}"
    );
    assert_confirmed(&report);
    let leader = i32::try_from(report.pid.expect("it started")).expect("a pid");
    assert_gone("stopped leader", &[leader]).await;
}

#[tokio::test]
async fn a_stopped_descendant_receives_terminate_when_the_group_does() {
    let s = Scratch::new();
    // With a long grace, only SIGTERM followed by SIGCONT stops it quickly.
    // On Linux a stopped process holds a TERM pending until it is continued,
    // so removing the SIGCONT makes this fail there. On macOS TERM ends a
    // stopped process by itself, so this cannot fail there; the Linux run in
    // the checklist in the module docs is what checks it.
    let mut spec = sh(
        &s,
        "echo $$ > leader; sleep 60 & echo $! > gc; kill -STOP $!; echo stopped > stopped; wait",
        LONG,
    );
    // The grace is a minute, so a stop that reached the descendant only with
    // the KILL after the grace would take a minute; the bound is half of it.
    spec.stop_grace = Duration::from_secs(60);
    spec.drain_bound = Duration::from_secs(60);
    spec.cleanup_deadline = Duration::from_secs(180);
    let handle = ProcessHandle::start(spec);
    let gc = s.pid("gc").await;
    let _reaper = Reaper::new([gc]);
    await_event("the descendant being stopped", || {
        s.path("stopped").exists()
    })
    .await;
    let began = Instant::now();
    handle.interrupt();
    let report = handle.wait().await;
    assert_eq!(report.ending, Ending::Interrupted);
    assert!(
        began.elapsed() < NOT_THE_GRACE,
        "took {:?}, which is the grace, not TERM and CONT",
        began.elapsed()
    );
    assert_confirmed(&report);
    assert_gone("stopped descendant", &[gc]).await;
}

both!(
    the_child_leads_a_new_session_and_has_no_controlling_terminal,
    the_child_leads_a_new_session_and_has_no_controlling_terminal_kept,
    async {
        let s = Scratch::new();
        let handle = ProcessHandle::start(sh(&s, "echo $$ > leader; sleep 60", LONG));
        let leader = s.pid("leader").await;
        let _reaper = Reaper::new([leader]);
        assert_eq!(
            sys::session_of(leader),
            Some(leader),
            "it leads its own session"
        );
        assert_ne!(sys::session_of(std::process::id() as i32), Some(leader));
        let tty = std::process::Command::new("/bin/ps")
            .args(["-o", "tty=", "-p", &leader.to_string()])
            .guarded_output()
            .expect("run ps");
        let tty = String::from_utf8_lossy(&tty.stdout);
        assert!(tty.trim().starts_with('?'), "controlling terminal: {tty:?}");
        handle.interrupt();
        let report = handle.wait().await;
        assert_confirmed(&report);
    }
);

both!(
    a_child_that_exits_while_the_stop_waits_for_its_scan_is_reported_as_exited,
    a_child_that_exits_while_the_stop_waits_for_its_scan_is_reported_as_exited_kept,
    async {
        let s = Scratch::new();
        let held = Held::new(false);
        // Exits 0 shortly after SIGUSR1, a signal the test sends itself.
        let script = "trap 'exit 0' USR1; echo $$ > leader; echo ready > ready; \
                  while :; do sleep 0.02; done";
        let spec = sh(&s, script, LONG).with_lister(held.lister());
        let handle = ProcessHandle::start(spec);
        let leader = s.pid("leader").await;
        let mut reaper = Reaper::default();
        let child = reaper.register(leader).expect("identify the child");
        await_event("the child being ready", || s.path("ready").exists()).await;
        held.armed.store(true, Ordering::SeqCst);
        // The stop begins and waits for a listing that is held; the child exits
        // during that wait, before any stop signal is sent. The listing is released
        // only once the child is gone, so the order does not depend on speed.
        handle.interrupt();
        await_event("the stop asking for its listing", || {
            held.waiting.load(Ordering::SeqCst) >= 1
        })
        .await;
        assert!(child.signal(Signal::USR1), "signal the child");
        assert_gone("the child after its exit signal", &[leader]).await;
        held.release();
        let report = handle.wait().await;
        assert_eq!(report.ending, Ending::Exited { code: 0 }, "{report:#?}");
        assert_confirmed(&report);
    }
);

both!(
    a_panic_in_the_owner_loop_stops_the_child_and_still_reports,
    a_panic_in_the_owner_loop_stops_the_child_and_still_reports_kept,
    async {
        let s = Scratch::new();
        let mut spec = sh(&s, "echo $$ > leader; sleep 60", LONG);
        spec.panic_in_loop = true;
        let report = run(spec).await;
        assert!(
            matches!(&report.ending, Ending::WaitFailed { message } if message.contains("panicked")),
            "{report:#?}"
        );
        assert_confirmed(&report);
        let pid = report.pid.expect("it started") as i32;
        let _reaper = Reaper::new([pid]);
        assert_gone("child after a loop panic", &[pid]).await;
    }
);

both!(
    a_failing_inspector_factory_is_a_warning_not_a_crash,
    a_failing_inspector_factory_is_a_warning_not_a_crash_kept,
    async {
        let s = Scratch::new();
        let lister: Lister = Arc::new(|| panic!("factory"));
        let report = run(sh(&s, "exit 0", Duration::from_secs(30)).with_lister(lister)).await;
        assert_eq!(report.ending, Ending::Exited { code: 0 }, "{report:#?}");
        assert!(
            report
                .diagnostics
                .inspection
                .last_error
                .as_deref()
                .is_some_and(|e| e.contains("factory panicked")),
            "{report:#?}"
        );
    }
);

#[test]
fn dropping_the_runtime_kills_a_tracked_descendant_outside_the_group_and_session() {
    require_perl!();
    let s = Scratch::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build runtime");
    let script = format!("echo $$ > leader; {OWN_SESSION} & echo $! > gc; wait");
    let (handle, gc) = runtime.block_on(async {
        let handle = ProcessHandle::start(sh(&s, &script, LONG));
        let gc = s.pid("gc").await;
        // The owner's drop can only reach it if a scan has recorded it.
        super::fixtures::wait_tracked(&handle, gc).await;
        (handle, gc)
    });
    let _reaper = Reaper::new([gc]);
    drop(runtime);
    drop(handle);
    let check = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build checking runtime");
    check.block_on(assert_gone(
        "tracked descendant in its own session after runtime shutdown",
        &[gc],
    ));
}

#[tokio::test]
async fn a_finished_child_is_confirmed_at_once_when_inspection_is_unavailable() {
    let missing = crate::process::tree::lister_for(vec!["/nonexistent/delega-ps".into()]);
    let mut spec = ProcessSpec::new("/usr/bin/true", LONG)
        .env(crate::process::EnvPolicy::empty())
        .with_lister(missing);
    // A grace that would show if cleanup slept it out: a minute, against a
    // bound of half of it.
    spec.stop_grace = Duration::from_secs(60);
    spec.drain_bound = Duration::from_secs(60);
    spec.cleanup_deadline = Duration::from_secs(180);
    let started = Instant::now();
    let report = run(spec).await;
    assert_eq!(report.ending, Ending::Exited { code: 0 });
    assert!(
        started.elapsed() < NOT_THE_GRACE,
        "took {:?}, which is the grace",
        started.elapsed()
    );
    match &report.cleanup {
        crate::contract::result::Cleanup::Unconfirmed {
            process_groups,
            reason,
            ..
        } => {
            assert!(process_groups.is_empty(), "{process_groups:?}");
            assert!(
                reason.contains("no process has the child's group id"),
                "{reason}"
            );
        }
        other => panic!("without inspection the verdict is Unconfirmed: {other:?}"),
    }
}
