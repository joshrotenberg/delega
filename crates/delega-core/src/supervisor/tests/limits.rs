//! Limits: reached, freed, and retention.

use std::time::Duration;

use super::{Rig, finished, read_compact, result_of};
use crate::contract::outcome::Outcome;
use crate::contract::request::Provider;
use crate::supervisor::{Error, Limits, RunState, Supervisor, Target, WorkerState};

#[tokio::test]
async fn defaults_are_the_named_constants() {
    let limits = Limits::default();
    assert_eq!(
        limits.max_live_workers,
        crate::supervisor::limits::MAX_LIVE_WORKERS
    );
    assert_eq!(
        limits.max_running_runs,
        crate::supervisor::limits::MAX_RUNNING_RUNS
    );
    assert_eq!(
        limits.max_queued_per_worker,
        crate::supervisor::limits::MAX_QUEUED_PER_WORKER
    );
    assert_eq!(
        limits.retained_results,
        crate::supervisor::limits::RETAINED_RESULTS
    );
    assert_eq!(limits.retention, crate::supervisor::limits::RETENTION);
    assert_eq!(limits.wait_ceiling, crate::supervisor::limits::WAIT_CEILING);

    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let view = supervisor.list().await.expect("list").limits;
    assert_eq!(view.max_live_workers, limits.max_live_workers);
    assert_eq!(view.max_running_runs, limits.max_running_runs);
    assert_eq!(view.max_queued_per_worker, limits.max_queued_per_worker);
    assert_eq!(view.retention_secs, limits.retention.as_secs());
    assert_eq!(view.wait_ceiling_secs, limits.wait_ceiling.as_secs());
}

#[tokio::test]
async fn the_live_worker_limit_refuses_the_next_start_and_close_frees_the_slot() {
    let rig = Rig::new();
    let limits = Limits {
        max_live_workers: 2,
        ..Limits::default()
    };
    let supervisor = Supervisor::new(rig.config_with(Provider::Claude, limits));
    let gates = [rig.gate("a"), rig.gate("b"), rig.gate("c")];
    let a = supervisor
        .start(rig.request(Provider::Claude, &gates[0].prompt()))
        .await
        .expect("start");
    let b = supervisor
        .start(rig.request(Provider::Claude, &gates[1].prompt()))
        .await
        .expect("start");
    assert_eq!(a.state, RunState::Running);
    assert_eq!(
        supervisor.list().await.expect("list").limits.live_workers,
        2
    );

    let refused = supervisor
        .start(rig.request(Provider::Claude, &gates[2].prompt()))
        .await
        .expect("start answers");
    assert_eq!(refused.outcome, Some(Outcome::Refused));
    assert_eq!(refused.worker, None);
    let error = refused.error.expect("reason");
    assert_eq!(error.kind, "limit_reached");
    assert!(error.message.contains("2 live workers"), "{error:?}");
    let result = result_of(&supervisor, &refused.run).await;
    assert_eq!(result.outcome, Outcome::Refused);
    assert!(supervisor.list().await.expect("list").workers.len() == 2);

    supervisor
        .close(a.worker.as_deref().expect("worker"))
        .await
        .expect("close");
    let list = supervisor.list().await.expect("list");
    assert_eq!(list.limits.live_workers, 1, "close frees the slot at once");
    let c = supervisor
        .start(rig.request(Provider::Claude, &gates[2].prompt()))
        .await
        .expect("start");
    assert!(c.worker.is_some() && c.outcome.is_none(), "{c:?}");

    for gate in &gates {
        gate.open();
    }
    result_of(&supervisor, &b.run).await;
    result_of(&supervisor, &c.run).await;
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn a_run_beyond_the_running_limit_waits_queued_and_starts_when_a_slot_frees() {
    let rig = Rig::new();
    let limits = Limits {
        max_running_runs: 2,
        ..Limits::default()
    };
    let supervisor = Supervisor::new(rig.config_with(Provider::Claude, limits));
    let gates = [rig.gate("a"), rig.gate("b"), rig.gate("c"), rig.gate("d")];
    let mut started = Vec::new();
    for gate in &gates {
        started.push(
            supervisor
                .start(rig.request(Provider::Claude, &gate.prompt()))
                .await
                .expect("start"),
        );
    }
    assert_eq!(started[0].state, RunState::Running);
    assert_eq!(started[1].state, RunState::Running);
    assert_eq!(started[2].state, RunState::Queued);
    assert_eq!(started[3].state, RunState::Queued);
    gates[0].started().await;
    gates[1].started().await;

    let list = supervisor.list().await.expect("list");
    assert_eq!(list.limits.running_runs, 2);
    assert_eq!(list.workers[2].brief.state, WorkerState::Queued);
    assert_eq!(list.workers[0].brief.state, WorkerState::Running);
    gates[2].assert_not_started().await;
    assert!(!gates[3].has_started());

    // The run that became eligible first starts first.
    gates[0].open();
    gates[2].started().await;
    assert!(!gates[3].has_started());
    gates[1].open();
    gates[3].started().await;
    gates[2].open();
    gates[3].open();
    for run in &started {
        let result = result_of(&supervisor, &run.run).await;
        assert_eq!(result.outcome, Outcome::Completed);
    }
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn the_queue_limit_refuses_a_follow_up_and_a_finish_makes_room() {
    let rig = Rig::new();
    let limits = Limits {
        max_queued_per_worker: 1,
        ..Limits::default()
    };
    let supervisor = Supervisor::new(rig.config_with(Provider::Claude, limits));
    let (g1, g2) = (rig.gate("one"), rig.gate("two"));
    let first = supervisor
        .start(rig.request(Provider::Claude, &g1.prompt()))
        .await
        .expect("start");
    let worker = first.worker.clone().expect("worker");
    g1.started().await;
    let queued = supervisor
        .follow_up(&worker, g2.prompt(), None)
        .await
        .expect("follow_up");
    assert_eq!(queued.state, RunState::Queued);

    let refused = supervisor
        .follow_up(&worker, "echo:too-many", None)
        .await
        .expect("follow_up answers");
    assert_eq!(refused.outcome, Some(Outcome::Refused));
    let error = refused.error.expect("reason");
    assert_eq!(error.kind, "queue_full");
    assert!(error.message.contains("1 runs queued"), "{error:?}");
    let list = supervisor.list().await.expect("list");
    assert_eq!(
        list.workers[0].brief.queue_length, 1,
        "a refusal is not queued"
    );

    g1.open();
    g2.started().await;
    let room = supervisor
        .follow_up(&worker, "echo:room", None)
        .await
        .expect("follow_up");
    assert_eq!(room.state, RunState::Queued);
    g2.open();
    result_of(&supervisor, &room.run).await;
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn results_stay_readable_after_close_until_the_retention_time_passes() {
    let rig = Rig::new();
    let limits = Limits {
        retention: Duration::from_millis(1500),
        ..Limits::default()
    };
    let supervisor = Supervisor::new(rig.config_with(Provider::Claude, limits));
    let started = supervisor
        .start(rig.request(Provider::Claude, "echo:kept"))
        .await
        .expect("start");
    let worker = started.worker.clone().expect("worker");
    let before = result_of(&supervisor, &started.run).await;
    supervisor.close(&worker).await.expect("close");

    let list = supervisor.list().await.expect("list");
    assert_eq!(list.workers[0].brief.state, WorkerState::Closed);
    assert_eq!(list.limits.live_workers, 0);
    let after = read_compact(&supervisor, &started.run).await;
    assert_eq!(after.outcome, Some(Outcome::Completed));
    assert_eq!(
        before.text,
        "answered echo:kept sid=".to_string() + rig.session()
    );

    tokio::time::sleep(Duration::from_millis(1700)).await;
    let gone = supervisor
        .read(Target::Run(started.run.clone()), Default::default(), None)
        .await
        .expect_err("the result has aged out");
    assert_eq!(gone, Error::UnknownRun(started.run.clone()));
    assert_eq!(
        supervisor
            .read(Target::Worker(worker.clone()), Default::default(), None)
            .await
            .expect_err("the closed worker has gone with its results"),
        Error::UnknownWorker(worker)
    );
    let list = supervisor.list().await.expect("list");
    assert!(list.workers.is_empty(), "{list:#?}");
    assert_eq!(list.limits.retained_now, 0);
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn only_the_newest_results_up_to_the_count_are_kept() {
    let rig = Rig::new();
    let limits = Limits {
        retained_results: 3,
        ..Limits::default()
    };
    let supervisor = Supervisor::new(rig.config_with(Provider::Claude, limits));
    let mut runs = Vec::new();
    for n in 0..5 {
        let started = supervisor
            .start(rig.request(Provider::Claude, &format!("echo:{n}")))
            .await
            .expect("start");
        finished(&supervisor, &[&started.run]).await;
        runs.push(started);
    }
    let list = supervisor.list().await.expect("list");
    assert_eq!(list.limits.retained_now, 3);
    for gone in &runs[..2] {
        assert_eq!(
            supervisor
                .read(Target::Run(gone.run.clone()), Default::default(), None)
                .await
                .expect_err("evicted"),
            Error::UnknownRun(gone.run.clone())
        );
    }
    for kept in &runs[2..] {
        assert_eq!(
            read_compact(&supervisor, &kept.run).await.outcome,
            Some(Outcome::Completed)
        );
    }
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}
