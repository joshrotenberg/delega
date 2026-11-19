//! Stopping: stopped processes, sessions, the pre-stop race and a panic.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::fixtures::{OWN_SESSION, Reaper, Scratch, assert_confirmed, assert_gone, sh};
use crate::process::sys::{self, Signal};
use crate::process::tree::{Lister, list_processes};
use crate::process::{Ending, ProcessHandle, ProcessSpec, run};

const LONG: Duration = Duration::from_secs(60);
/// Longer than the pre-stop scan budget, short enough to keep the suite quick.
const STALL: Duration = Duration::from_millis(300);

#[tokio::test]
async fn a_leader_that_stops_itself_is_still_running_and_ends_as_timed_out() {
    let s = Scratch::new();
    let spec = sh(
        &s,
        "echo $$ > leader; kill -STOP $$; sleep 1",
        Duration::from_millis(600),
    );
    let started = Instant::now();
    let report = run(spec).await;
    assert!(
        matches!(report.ending, Ending::TimedOut { .. }),
        "a stopped child has not ended: {report:#?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(6),
        "took {:?}",
        started.elapsed()
    );
    assert_confirmed(&report);
    assert_gone("stopped leader", &[s.pid_now("leader")]).await;
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
    spec.stop_grace = Duration::from_secs(5);
    spec.drain_bound = Duration::from_secs(5);
    spec.cleanup_deadline = Duration::from_secs(30);
    let handle = ProcessHandle::start(spec);
    let gc = s.pid("gc").await;
    let _reaper = Reaper(vec![gc]);
    for _ in 0..500 {
        if s.path("stopped").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let began = Instant::now();
    handle.interrupt();
    let report = handle.wait().await;
    assert_eq!(report.ending, Ending::Interrupted);
    assert!(
        began.elapsed() < Duration::from_secs(3),
        "took {:?}",
        began.elapsed()
    );
    assert_confirmed(&report);
    assert_gone("stopped descendant", &[gc]).await;
}

#[tokio::test]
async fn the_child_leads_a_new_session_and_has_no_controlling_terminal() {
    let s = Scratch::new();
    let handle = ProcessHandle::start(sh(&s, "echo $$ > leader; sleep 60", LONG));
    let leader = s.pid("leader").await;
    let _reaper = Reaper(vec![leader]);
    assert_eq!(
        sys::session_of(leader),
        Some(leader),
        "it leads its own session"
    );
    assert_ne!(sys::session_of(std::process::id() as i32), Some(leader));
    let tty = std::process::Command::new("/bin/ps")
        .args(["-o", "tty=", "-p", &leader.to_string()])
        .output()
        .expect("run ps");
    let tty = String::from_utf8_lossy(&tty.stdout);
    assert!(tty.trim().starts_with('?'), "controlling terminal: {tty:?}");
    handle.interrupt();
    let report = handle.wait().await;
    assert_confirmed(&report);
}

/// A lister that, once armed, stalls once before answering, so a stop's pre-stop
/// scan cannot finish within its budget.
fn stalling_lister(armed: Arc<AtomicBool>) -> Lister {
    // Only the first call after arming stalls: the stop's own scan.
    let stalls = Arc::new(AtomicBool::new(true));
    Arc::new(move || {
        let armed = armed.clone();
        let stalls = stalls.clone();
        Box::pin(async move {
            if armed.load(Ordering::SeqCst) && stalls.swap(false, Ordering::SeqCst) {
                tokio::time::sleep(STALL).await;
            }
            list_processes().await
        })
    })
}

#[tokio::test]
async fn a_child_that_exits_while_the_stop_waits_for_its_scan_is_reported_as_exited() {
    let s = Scratch::new();
    let armed = Arc::new(AtomicBool::new(false));
    // Exits 0 shortly after SIGUSR1, a signal the test sends itself.
    let script = "trap 'exit 0' USR1; echo $$ > leader; echo ready > ready; \
                  while :; do sleep 0.02; done";
    let spec = sh(&s, script, LONG).with_lister(stalling_lister(armed.clone()));
    let handle = ProcessHandle::start(spec);
    let leader = s.pid("leader").await;
    for _ in 0..500 {
        if s.path("ready").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    armed.store(true, Ordering::SeqCst);
    // The stop begins and waits up to its budget for a scan that stalls; the
    // child exits during that wait, before any stop signal is sent.
    handle.interrupt();
    sys::signal_process(leader, Signal::USR1).expect("signal the child");
    let report = handle.wait().await;
    assert_eq!(report.ending, Ending::Exited { code: 0 }, "{report:#?}");
    assert_confirmed(&report);
}

#[tokio::test]
async fn a_panic_in_the_owner_loop_stops_the_child_and_still_reports() {
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
    let _reaper = Reaper(vec![pid]);
    assert_gone("child after a loop panic", &[pid]).await;
}

#[tokio::test]
async fn a_failing_inspector_factory_is_a_warning_not_a_crash() {
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
        for _ in 0..500 {
            if handle.tracked_pids().contains(&(gc as u32)) {
                return (handle, gc);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the scan never tracked the descendant");
    });
    let _reaper = Reaper(vec![gc]);
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
    // A grace that would show if cleanup slept it out.
    spec.stop_grace = Duration::from_secs(5);
    spec.drain_bound = Duration::from_secs(5);
    spec.cleanup_deadline = Duration::from_secs(30);
    let started = Instant::now();
    let report = run(spec).await;
    assert_eq!(report.ending, Ending::Exited { code: 0 });
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "took {:?}",
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
