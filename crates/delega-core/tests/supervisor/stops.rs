//! Interrupt, close, shutdown, drop, and a run's task that panics.

use std::time::{Duration, Instant};

use super::{Rig, read_compact, read_full, result_of};
use delega_core::contract::outcome::Outcome;
use delega_core::contract::request::Provider;
use delega_core::supervisor::{
    Error, Limits, ResultView, RunState, Supervisor, Target, WorkerState,
};

#[tokio::test]
async fn the_rig_sees_a_process_that_is_running_and_not_one_that_ended() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let gate = rig.gate("watched");
    supervisor
        .start(rig.request(Provider::Claude, &gate.prompt()))
        .await
        .expect("start");
    gate.started().await;
    assert!(!rig.survivors().is_empty(), "a running fake must be seen");
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn interrupting_a_running_run_waits_for_cleanup_and_returns_interrupted() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    // `hang` ignores the polite stop, so the stop must escalate.
    let started = supervisor
        .start(rig.request(Provider::Claude, "hang"))
        .await
        .expect("start");
    for _ in 0..1000 {
        if rig.record.join("started").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let response = supervisor
        .interrupt(Target::Run(started.run.clone()))
        .await
        .expect("interrupt");
    assert!(response.settled && !response.was_queued);
    assert_eq!(response.state, Some(RunState::Finished));
    let Some(ResultView::Compact(result)) = &response.result else {
        panic!("{response:#?}");
    };
    assert_eq!(result.outcome, Outcome::Interrupted);
    assert_eq!(
        result.cleanup,
        delega_core::contract::result::Cleanup::Confirmed
    );
    assert_eq!(result.error.as_ref().expect("error").kind, "interrupted");
    rig.assert_nothing_left().await;

    let list = supervisor.list().await.expect("list");
    assert_eq!(list.workers[0].brief.state, WorkerState::Idle);
    assert_eq!(list.limits.running_runs, 0);
    supervisor.shutdown().await;
}

#[tokio::test]
async fn interrupting_a_worker_stops_its_current_run_and_the_queue_behind_it_goes_on() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let (g1, g2) = (rig.gate("front"), rig.gate("next"));
    let first = supervisor
        .start(rig.request(Provider::Claude, &g1.prompt()))
        .await
        .expect("start");
    let worker = first.worker.clone().expect("worker");
    g1.started().await;
    let next = supervisor
        .follow_up(&worker, g2.prompt(), None)
        .await
        .expect("follow_up");
    // The first run reports its session before it is stopped, so the
    // follow-up can resume it.
    super::read_until_text(&supervisor, &first.run).await;

    let response = supervisor
        .interrupt(Target::Worker(worker.clone()))
        .await
        .expect("interrupt");
    assert_eq!(response.run.as_deref(), Some(first.run.as_str()));
    assert!(response.settled);
    let stopped = result_of(&supervisor, &first.run).await;
    assert_eq!(stopped.outcome, Outcome::Interrupted);
    assert_eq!(stopped.session_id.as_deref(), Some(rig.session()));
    assert_eq!(stopped.text, "working", "the partial answer is kept");

    g2.started().await;
    g2.open();
    let ran = result_of(&supervisor, &next.run).await;
    assert_eq!(ran.outcome, Outcome::Completed, "{ran:#?}");
    assert!(ran.text.contains(&format!("sid={}", rig.session())));
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn interrupting_a_queued_run_removes_it_with_an_interrupted_result_that_never_started() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let (g1, g2, g3) = (rig.gate("front"), rig.gate("middle"), rig.gate("last"));
    let first = supervisor
        .start(rig.request(Provider::Claude, &g1.prompt()))
        .await
        .expect("start");
    let worker = first.worker.clone().expect("worker");
    g1.started().await;
    let middle = supervisor
        .follow_up(&worker, g2.prompt(), None)
        .await
        .expect("follow_up");
    let last = supervisor
        .follow_up(&worker, g3.prompt(), None)
        .await
        .expect("follow_up");

    let response = supervisor
        .interrupt(Target::Run(middle.run.clone()))
        .await
        .expect("interrupt");
    assert!(response.was_queued && response.settled);
    let result = result_of(&supervisor, &middle.run).await;
    assert_eq!(result.outcome, Outcome::Interrupted);
    assert_eq!(result.timing.started_at, None);
    assert_eq!(result.timing.duration_ms, 0);
    assert_eq!(
        result.cleanup,
        delega_core::contract::result::Cleanup::Confirmed
    );
    assert!(!g2.has_started());
    let list = supervisor.list().await.expect("list");
    assert_eq!(list.workers[0].brief.queue_length, 1);

    // The run behind it still goes, after the first.
    g1.open();
    g3.started().await;
    assert!(!g2.has_started(), "the removed run never starts");
    g3.open();
    assert_eq!(
        result_of(&supervisor, &last.run).await.outcome,
        Outcome::Completed
    );
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn interrupting_a_finished_run_returns_its_result_unchanged() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let started = supervisor
        .start(rig.request(Provider::Claude, "echo:done"))
        .await
        .expect("start");
    let before = result_of(&supervisor, &started.run).await;

    let response = supervisor
        .interrupt(Target::Run(started.run.clone()))
        .await
        .expect("interrupt");
    assert!(response.settled && !response.was_queued);
    let Some(ResultView::Compact(result)) = &response.result else {
        panic!("{response:#?}");
    };
    assert_eq!(result.outcome, Outcome::Completed);
    assert_eq!(result.text, before.text);
    assert_eq!(result.timing, before.timing);
    let after = result_of(&supervisor, &started.run).await;
    assert_eq!(after, before, "the stored result did not change");

    let by_worker = supervisor
        .interrupt(Target::Worker(started.worker.clone().expect("worker")))
        .await
        .expect("interrupt");
    assert_eq!(by_worker.run.as_deref(), Some(started.run.as_str()));
    assert_eq!(
        supervisor
            .interrupt(Target::Run("run-nope".into()))
            .await
            .expect_err("unknown"),
        Error::UnknownRun("run-nope".into())
    );
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn closing_a_worker_stops_its_run_discards_its_queue_and_keeps_the_results() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let (g1, g2, g3) = (rig.gate("front"), rig.gate("a"), rig.gate("b"));
    let first = supervisor
        .start(rig.request(Provider::Claude, &g1.prompt()))
        .await
        .expect("start");
    let worker = first.worker.clone().expect("worker");
    g1.started().await;
    let a = supervisor
        .follow_up(&worker, g2.prompt(), None)
        .await
        .expect("follow_up");
    let b = supervisor
        .follow_up(&worker, g3.prompt(), None)
        .await
        .expect("follow_up");

    let closed = supervisor.close(&worker).await.expect("close");
    assert_eq!(closed.state, WorkerState::Closed);
    assert!(!closed.already_closed && closed.settled);
    assert_eq!(closed.stopped_run.as_deref(), Some(first.run.as_str()));
    assert_eq!(closed.discarded, [a.run.clone(), b.run.clone()]);
    let Some(ResultView::Compact(stopped)) = &closed.result else {
        panic!("{closed:#?}");
    };
    assert_eq!(stopped.outcome, Outcome::Interrupted);

    for run in [&a.run, &b.run] {
        let result = result_of(&supervisor, run).await;
        assert_eq!(result.outcome, Outcome::Interrupted);
        let error = result.error.expect("error");
        assert_eq!(error.kind, "interrupted");
        assert!(error.message.contains("discarded by close"), "{error:?}");
        assert_eq!(result.timing.started_at, None);
    }
    assert!(
        !g2.has_started() && !g3.has_started(),
        "discarded runs never start"
    );
    assert_eq!(
        read_compact(&supervisor, &first.run).await.outcome,
        Some(Outcome::Interrupted)
    );
    let list = supervisor.list().await.expect("list");
    assert_eq!(list.workers[0].brief.state, WorkerState::Closed);
    assert_eq!(list.limits.live_workers, 0);
    assert_eq!(list.limits.running_runs, 0);
    rig.assert_nothing_left().await;

    // Closing again changes nothing.
    let again = supervisor.close(&worker).await.expect("close");
    assert!(again.already_closed && again.settled && again.discarded.is_empty());
    assert_eq!(again.state, WorkerState::Closed);

    // A closed worker takes no more work.
    let refused = supervisor
        .follow_up(&worker, "echo:late", None)
        .await
        .expect("follow_up answers");
    assert_eq!(refused.outcome, Some(Outcome::Refused));
    assert_eq!(refused.error.expect("reason").kind, "worker_closed");
    assert_eq!(
        supervisor.close("worker-nope").await.expect_err("unknown"),
        Error::UnknownWorker("worker-nope".into())
    );
    supervisor.shutdown().await;
}

#[tokio::test]
async fn closing_an_idle_worker_frees_its_slot_and_a_run_waiting_for_one_starts() {
    let rig = Rig::new();
    let limits = Limits {
        max_running_runs: 1,
        ..Limits::default()
    };
    let supervisor = Supervisor::new(rig.config_with(Provider::Claude, limits));
    let (g1, g2) = (rig.gate("running"), rig.gate("waiting"));
    let a = supervisor
        .start(rig.request(Provider::Claude, &g1.prompt()))
        .await
        .expect("start");
    let b = supervisor
        .start(rig.request(Provider::Claude, &g2.prompt()))
        .await
        .expect("start");
    assert_eq!(b.state, RunState::Queued);
    g1.started().await;
    supervisor
        .close(a.worker.as_deref().expect("worker"))
        .await
        .expect("close");
    g2.started().await;
    g2.open();
    assert_eq!(
        result_of(&supervisor, &b.run).await.outcome,
        Outcome::Completed
    );
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

fn ten_workers_limits() -> Limits {
    Limits {
        max_live_workers: 10,
        max_running_runs: 10,
        ..Limits::default()
    }
}

async fn start_ten(rig: &Rig, supervisor: &Supervisor) -> Vec<(String, String)> {
    let mut started = Vec::new();
    for n in 0..10 {
        let gate = rig.gate(&format!("w{n}"));
        let response = supervisor
            .start(rig.request(Provider::Claude, &gate.prompt()))
            .await
            .expect("start");
        assert_eq!(response.state, RunState::Running);
        started.push((response.worker.expect("worker"), response.run));
    }
    for n in 0..10 {
        rig.gate(&format!("w{n}")).started().await;
    }
    started
}

#[tokio::test]
async fn shutdown_stops_ten_running_workers_and_leaves_nothing() {
    let rig = Rig::new();
    let supervisor = Supervisor::new(rig.config_with(Provider::Claude, ten_workers_limits()));
    let started = start_ten(&rig, &supervisor).await;
    assert!(!rig.survivors().is_empty());

    let report = supervisor.shutdown().await;
    assert_eq!(report.workers_closed, 10);
    assert_eq!(report.runs_stopped, 10);
    assert_eq!(report.runs_discarded, 0);
    assert!(report.unsettled.is_empty(), "{report:?}");
    for (_, run) in &started {
        let result = result_of(&supervisor, run).await;
        assert_eq!(result.outcome, Outcome::Interrupted);
        assert_eq!(
            result.cleanup,
            delega_core::contract::result::Cleanup::Confirmed
        );
    }
    rig.assert_nothing_left().await;

    assert_eq!(
        supervisor.list().await.expect("list").limits.live_workers,
        0
    );
    let late = supervisor
        .start(rig.request(Provider::Claude, "success"))
        .await
        .expect_err("a shut down supervisor takes no work");
    assert_eq!(late, Error::ShutDown);
    assert_eq!(
        supervisor
            .follow_up(&started[0].0, "more", None)
            .await
            .expect_err("shut down"),
        Error::ShutDown
    );
    let again = supervisor.shutdown().await;
    assert_eq!(again.workers_closed, 0);
}

#[tokio::test]
async fn shutdown_discards_what_is_queued_and_says_so() {
    let rig = Rig::new();
    let limits = Limits {
        max_running_runs: 1,
        ..Limits::default()
    };
    let supervisor = Supervisor::new(rig.config_with(Provider::Claude, limits));
    let gate = rig.gate("only");
    let first = supervisor
        .start(rig.request(Provider::Claude, &gate.prompt()))
        .await
        .expect("start");
    let waiting = supervisor
        .start(rig.request(Provider::Claude, "echo:never"))
        .await
        .expect("start");
    let behind = supervisor
        .follow_up(
            first.worker.as_deref().expect("worker"),
            "echo:behind",
            None,
        )
        .await
        .expect("follow_up");
    gate.started().await;

    let report = supervisor.shutdown().await;
    assert_eq!(report.workers_closed, 2);
    assert_eq!(report.runs_stopped, 1);
    assert_eq!(report.runs_discarded, 2);
    for run in [&waiting.run, &behind.run] {
        let result = result_of(&supervisor, run).await;
        assert_eq!(result.outcome, Outcome::Interrupted);
        assert!(
            result
                .error
                .expect("error")
                .message
                .contains("discarded by shutdown")
        );
    }
    rig.assert_nothing_left().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_the_supervisor_with_ten_workers_running_leaves_nothing_behind() {
    let rig = Rig::new();
    let supervisor = Supervisor::new(rig.config_with(Provider::Claude, ten_workers_limits()));
    start_ten(&rig, &supervisor).await;
    assert!(!rig.survivors().is_empty());

    let begun = Instant::now();
    drop(supervisor);
    assert!(
        begun.elapsed() < Duration::from_secs(10),
        "{:?}",
        begun.elapsed()
    );
    // Drop waited for the stop on a multi-threaded runtime: nothing is left
    // by now, without any further waiting.
    let left = rig.survivors();
    assert!(
        left.is_empty(),
        "drop returned with processes alive: {left:#?}"
    );
}

#[tokio::test]
async fn dropping_the_supervisor_on_a_current_thread_runtime_stops_everything_once_it_runs() {
    let rig = Rig::new();
    let supervisor = Supervisor::new(rig.config_with(Provider::Claude, ten_workers_limits()));
    start_ten(&rig, &supervisor).await;
    let begun = Instant::now();
    drop(supervisor);
    // Nothing blocks, because nothing else could run the stop; every child has
    // been signalled and the runtime stops them as soon as it is polled.
    assert!(
        begun.elapsed() < Duration::from_secs(5),
        "{:?}",
        begun.elapsed()
    );
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn dropping_one_clone_keeps_the_supervisor_and_the_last_drop_stops_it() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let gate = rig.gate("shared");
    let other = supervisor.clone();
    let started = supervisor
        .start(rig.request(Provider::Claude, &gate.prompt()))
        .await
        .expect("start");
    gate.started().await;
    drop(supervisor);
    assert_eq!(
        read_compact(&other, &started.run).await.state,
        Some(RunState::Running),
        "another clone keeps it alive"
    );
    drop(other);
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn a_run_stopped_while_stopping_is_asked_again_changes_nothing() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let gate = rig.gate("twice");
    let started = supervisor
        .start(rig.request(Provider::Claude, &gate.prompt()))
        .await
        .expect("start");
    gate.started().await;
    let (a, b) = tokio::join!(
        supervisor.interrupt(Target::Run(started.run.clone())),
        supervisor.interrupt(Target::Run(started.run.clone())),
    );
    let (a, b) = (a.expect("interrupt"), b.expect("interrupt"));
    assert!(a.settled && b.settled);
    assert_eq!(
        serde_json::to_value(&a.result).expect("serialize"),
        serde_json::to_value(&b.result).expect("serialize")
    );
    let full = read_full(&supervisor, &started.run).await;
    assert_eq!(
        full.progress
            .expect("progress")
            .diagnostics
            .expect("diagnostics")
            .stop_requested,
        Some("interrupt")
    );
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn an_interrupt_that_does_not_settle_within_its_bound_says_so_and_the_run_still_ends() {
    let rig = Rig::new();
    let mut config = rig.config(Provider::Claude);
    config.interrupt_bound = Duration::from_millis(1);
    let supervisor = Supervisor::new(config);
    let gate = rig.gate("bounded");
    let started = supervisor
        .start(rig.request(Provider::Claude, &gate.prompt()))
        .await
        .expect("start");
    gate.started().await;

    let response = supervisor
        .interrupt(Target::Run(started.run.clone()))
        .await
        .expect("interrupt");
    assert!(
        !response.settled,
        "stopping a process takes longer than 1 ms"
    );
    assert!(response.result.is_none());
    let result = result_of(&supervisor, &started.run).await;
    assert_eq!(result.outcome, Outcome::Interrupted);
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn a_shutdown_that_does_not_settle_within_its_bound_names_the_runs_left() {
    let rig = Rig::new();
    let mut config = rig.config(Provider::Claude);
    config.shutdown_bound = Duration::from_millis(1);
    let supervisor = Supervisor::new(config);
    let gate = rig.gate("bounded");
    let started = supervisor
        .start(rig.request(Provider::Claude, &gate.prompt()))
        .await
        .expect("start");
    gate.started().await;

    let report = supervisor.shutdown().await;
    assert_eq!(report.runs_stopped, 1);
    assert_eq!(report.unsettled, std::slice::from_ref(&started.run));
    // The stop was asked for and goes on; the run still ends interrupted.
    assert_eq!(
        result_of(&supervisor, &started.run).await.outcome,
        Outcome::Interrupted
    );
    rig.assert_nothing_left().await;
}

/// Drop the last clone inside a spawned task, which runs on a runtime worker
/// thread, with ten workers running. The drop must not hold that thread for
/// the whole shutdown bound, and must leave nothing running.
async fn drop_inside_a_spawned_task(rig: &Rig) {
    let supervisor = Supervisor::new(rig.config_with(Provider::Claude, ten_workers_limits()));
    start_ten(rig, &supervisor).await;
    assert!(!rig.survivors().is_empty());
    let took = tokio::spawn(async move {
        let begun = Instant::now();
        drop(supervisor);
        begun.elapsed()
    })
    .await
    .expect("the task that drops the supervisor");
    assert!(
        took < Duration::from_secs(10),
        "the drop held a worker thread for {took:?}"
    );
    let left = rig.survivors();
    assert!(
        left.is_empty(),
        "drop returned with processes alive: {left:#?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropping_the_last_clone_inside_a_task_with_several_runtime_threads_leaves_nothing() {
    let rig = Rig::new();
    drop_inside_a_spawned_task(&rig).await;
    rig.assert_nothing_left().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn dropping_the_last_clone_inside_a_task_with_one_runtime_thread_leaves_nothing() {
    let rig = Rig::new();
    drop_inside_a_spawned_task(&rig).await;
    rig.assert_nothing_left().await;
}
