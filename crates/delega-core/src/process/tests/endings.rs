//! Each way a run can end, and that nothing is left running afterwards.

use std::time::{Duration, Instant};

use super::fixtures::{Reaper, Scratch, assert_confirmed, assert_gone, sh};
use crate::contract::outcome::Outcome;
use crate::process::{Ending, ProcessHandle, ProcessSpec, run};

const LONG: Duration = Duration::from_secs(60);
const BOUND: Duration = Duration::from_secs(6);

#[tokio::test]
async fn a_normal_exit_reports_code_zero_its_output_and_confirmed_cleanup() {
    let s = Scratch::new();
    let spec = sh(
        &s,
        "echo $$ > leader; printf out; printf err >&2; exit 0",
        LONG,
    );
    let report = run(spec).await;
    assert_eq!(report.ending, Ending::Exited { code: 0 });
    assert_eq!(report.outcome(), Outcome::Completed);
    assert!(report.error_detail().is_none());
    assert_eq!(report.stdout.text(), "out");
    assert_eq!(report.stderr.text(), "err");
    assert_eq!(report.pid, Some(s.pid_now("leader") as u32));
    assert_confirmed(&report);
    assert_gone("leader", &[s.pid_now("leader")]).await;
}

#[tokio::test]
async fn a_nonzero_exit_is_a_failure_that_carries_the_code() {
    let s = Scratch::new();
    let report = run(sh(&s, "echo $$ > leader; echo boom >&2; exit 7", LONG)).await;
    assert_eq!(report.ending, Ending::Exited { code: 7 });
    assert_eq!(report.outcome(), Outcome::Failed);
    let error = report.error_detail().expect("a failure has an error");
    assert_eq!(error.provider_exit_code, Some(7));
    assert_eq!(report.stderr.text(), "boom\n");
    assert_confirmed(&report);
    assert_gone("leader", &[s.pid_now("leader")]).await;
}

#[tokio::test]
async fn a_child_killed_by_a_signal_is_reported_as_signaled() {
    let s = Scratch::new();
    let report = run(sh(&s, "echo $$ > leader; kill -KILL $$; sleep 5", LONG)).await;
    assert_eq!(
        report.ending,
        Ending::Signaled {
            signal: 9,
            name: "SIGKILL".into()
        }
    );
    assert_eq!(report.outcome(), Outcome::Failed);
    assert_eq!(report.exit.as_ref().and_then(|e| e.signal), Some(9));
    assert_confirmed(&report);
    assert_gone("leader", &[s.pid_now("leader")]).await;
}

#[tokio::test]
async fn a_time_limit_ends_the_run_as_timed_out_and_stops_the_child() {
    let s = Scratch::new();
    let started = Instant::now();
    let spec = sh(&s, "echo $$ > leader; sleep 60", Duration::from_millis(400));
    let report = run(spec).await;
    assert_eq!(
        report.ending,
        Ending::TimedOut {
            limit: Duration::from_millis(400)
        }
    );
    assert_eq!(report.outcome(), Outcome::TimedOut);
    assert_eq!(report.error_detail().unwrap().kind, "timed_out");
    assert!(report.exit.as_ref().is_some_and(|e| e.signal.is_some()));
    assert!(started.elapsed() < BOUND, "took {:?}", started.elapsed());
    assert_confirmed(&report);
    assert_gone("leader", &[s.pid_now("leader")]).await;
}

#[tokio::test]
async fn a_child_that_ignores_terminate_is_killed_after_the_grace_period() {
    let s = Scratch::new();
    let spec = sh(
        &s,
        "trap '' TERM; echo $$ > leader; sleep 60 & echo $! > gc; wait",
        Duration::from_millis(400),
    );
    let started = Instant::now();
    let report = run(spec).await;
    assert_eq!(report.outcome(), Outcome::TimedOut);
    assert_eq!(
        report.exit.as_ref().and_then(|e| e.signal),
        Some(9),
        "only SIGKILL stops a child that ignores SIGTERM: {report:#?}"
    );
    assert!(started.elapsed() < BOUND, "took {:?}", started.elapsed());
    assert_confirmed(&report);
    assert_gone(
        "child and grandchild",
        &[s.pid_now("leader"), s.pid_now("gc")],
    )
    .await;
}

#[tokio::test]
async fn interrupting_through_the_handle_ends_the_run_as_interrupted() {
    let s = Scratch::new();
    let handle = ProcessHandle::start(sh(&s, "echo $$ > leader; sleep 60", LONG));
    let leader = s.pid("leader").await;
    assert_eq!(handle.pid(), Some(leader as u32));
    handle.interrupt();
    let started = Instant::now();
    let report = handle.wait().await;
    assert_eq!(report.ending, Ending::Interrupted);
    assert_eq!(report.outcome(), Outcome::Interrupted);
    assert!(started.elapsed() < BOUND);
    assert_confirmed(&report);
    assert_gone("leader", &[leader]).await;
}

#[tokio::test]
async fn interrupting_from_another_task_stops_the_child_and_its_descendants() {
    let s = Scratch::new();
    let handle = ProcessHandle::start(sh(
        &s,
        "echo $$ > leader; sleep 60 & echo $! > gc; wait",
        LONG,
    ));
    let interrupter = handle.interrupter();
    let leader = s.pid("leader").await;
    let gc = s.pid("gc").await;
    let other = tokio::spawn(async move { interrupter.interrupt() });
    other.await.expect("interrupting task");
    let report = handle.wait().await;
    assert_eq!(report.ending, Ending::Interrupted);
    assert_confirmed(&report);
    assert_gone("child and grandchild", &[leader, gc]).await;
}

#[tokio::test]
async fn interrupting_from_another_thread_ends_the_run_as_interrupted() {
    let s = Scratch::new();
    let handle = ProcessHandle::start(sh(&s, "echo $$ > leader; sleep 60", LONG));
    let interrupter = handle.interrupter();
    let leader = s.pid("leader").await;
    std::thread::spawn(move || interrupter.interrupt())
        .join()
        .expect("interrupting thread");
    let report = handle.wait().await;
    assert_eq!(report.ending, Ending::Interrupted);
    assert_confirmed(&report);
    assert_gone("leader", &[leader]).await;
}

#[tokio::test]
async fn interrupting_before_the_child_has_run_still_ends_clean() {
    let s = Scratch::new();
    let handle = ProcessHandle::start(sh(&s, "echo $$ > leader; sleep 60", LONG));
    handle.interrupt();
    let report = handle.wait().await;
    assert!(
        matches!(report.ending, Ending::Interrupted | Ending::Exited { .. }),
        "{report:#?}"
    );
    assert_confirmed(&report);
    if let Ok(text) = std::fs::read_to_string(s.path("leader")) {
        let leader: i32 = text.trim().parse().expect("pid");
        assert_gone("leader", &[leader]).await;
    }
}

#[tokio::test]
async fn dropping_the_handle_stops_the_child_and_its_descendants() {
    let s = Scratch::new();
    let handle = ProcessHandle::start(sh(
        &s,
        "echo $$ > leader; sleep 60 & echo $! > gc; wait",
        LONG,
    ));
    let leader = s.pid("leader").await;
    let gc = s.pid("gc").await;
    let _reaper = Reaper(vec![leader, gc]);
    drop(handle);
    assert_gone(
        "child and grandchild after the handle was dropped",
        &[leader, gc],
    )
    .await;
}

#[tokio::test]
async fn cancelling_the_run_future_stops_the_child() {
    let s = Scratch::new();
    let spec = sh(&s, "echo $$ > leader; sleep 60", LONG);
    let mut future = Box::pin(run(spec));
    // Wait for the child to exist, however slow the machine, then cancel.
    let leader = tokio::select! {
        _ = &mut future => panic!("the run should still be going"),
        pid = s.pid("leader") => pid,
    };
    let _reaper = Reaper(vec![leader]);
    drop(future);
    assert_gone("leader after the run future was dropped", &[leader]).await;
}

#[test]
fn shutting_down_the_runtime_kills_the_group_of_a_running_child() {
    let s = Scratch::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build runtime");
    let (handle, leader, gc) = runtime.block_on(async {
        let handle = ProcessHandle::start(sh(
            &s,
            "echo $$ > leader; sleep 60 & echo $! > gc; wait",
            LONG,
        ));
        let leader = s.pid("leader").await;
        let gc = s.pid("gc").await;
        (handle, leader, gc)
    });
    let _reaper = Reaper(vec![leader, gc]);
    drop(runtime);
    drop(handle);
    let check = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build checking runtime");
    check.block_on(assert_gone(
        "child and grandchild after runtime shutdown",
        &[leader, gc],
    ));
}

#[tokio::test]
async fn a_program_that_does_not_exist_is_a_failure_to_start_with_nothing_left() {
    let spec = ProcessSpec::new("/nonexistent/delega-test-program", LONG);
    let report = run(spec).await;
    assert!(
        matches!(&report.ending, Ending::FailedToStart { message } if message.contains("/nonexistent/delega-test-program")),
        "{report:#?}"
    );
    assert_eq!(report.outcome(), Outcome::Failed);
    assert_eq!(report.error_detail().unwrap().kind, "spawn_failed");
    assert_eq!(report.pid, None);
    assert_eq!(report.timing.started_at, None);
    assert_eq!(report.timing.duration, Duration::ZERO);
    assert_confirmed(&report);
}

#[tokio::test]
async fn a_working_directory_that_does_not_exist_is_a_failure_to_start() {
    let spec = ProcessSpec::new("/bin/sh", LONG)
        .args(["-c", "exit 0"])
        .cwd("/nonexistent/delega-test-dir");
    let report = run(spec).await;
    assert!(
        matches!(report.ending, Ending::FailedToStart { .. }),
        "{report:#?}"
    );
    assert_confirmed(&report);
}

#[tokio::test]
async fn timing_is_ordered_and_names_the_limit() {
    let s = Scratch::new();
    let report = run(sh(&s, "sleep 0.2", LONG)).await;
    let started = report.timing.started_at.expect("it started");
    assert!(started <= report.timing.finished_at);
    assert!(report.timing.duration >= Duration::from_millis(150));
    assert_eq!(report.timing.time_limit, LONG);
    assert_eq!(report.timing.time_limit_secs(), 60);
}
