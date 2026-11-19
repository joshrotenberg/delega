//! `wait`: with and without a cursor, on timeout, with several runs, with
//! unknown ids.

use std::time::{Duration, Instant};

use super::{Rig, finished, result_of};
use delega_core::contract::outcome::Outcome;
use delega_core::contract::request::Provider;
use delega_core::supervisor::{Error, Limits, RunState, Supervisor, WaitTargets, WorkerState};

fn runs(ids: &[&str]) -> WaitTargets {
    WaitTargets {
        runs: ids.iter().map(ToString::to_string).collect(),
        workers: Vec::new(),
    }
}

fn workers(ids: &[&str]) -> WaitTargets {
    WaitTargets {
        runs: Vec::new(),
        workers: ids.iter().map(ToString::to_string).collect(),
    }
}

#[tokio::test]
async fn a_wait_returns_when_any_named_run_finishes_and_says_which() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let (ga, gb) = (rig.gate("a"), rig.gate("b"));
    let a = supervisor
        .start(rig.request(Provider::Claude, &ga.prompt()))
        .await
        .expect("start");
    let b = supervisor
        .start(rig.request(Provider::Claude, &gb.prompt()))
        .await
        .expect("start");
    ga.started().await;
    gb.started().await;

    let waiting = {
        let supervisor = supervisor.clone();
        let targets = runs(&[&a.run, &b.run]);
        tokio::spawn(async move { supervisor.wait(targets, Some(30), None).await })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!waiting.is_finished(), "nothing has finished yet");
    gb.open();
    let response = waiting.await.expect("task").expect("wait");
    assert!(!response.timed_out);
    let by_run = |id: &str| response.runs.iter().find(|r| r.run == id).expect("brief");
    assert_eq!(by_run(&a.run).state, RunState::Running);
    assert_eq!(by_run(&b.run).state, RunState::Finished);
    assert_eq!(by_run(&b.run).outcome, Some(Outcome::Completed));
    assert!(response.runs.iter().all(|r| r.changed.is_none()));
    assert!(response.unknown.is_empty());

    ga.open();
    result_of(&supervisor, &a.run).await;
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn a_wait_that_times_out_is_a_response_with_current_states_not_an_error() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let gate = rig.gate("slow");
    let started = supervisor
        .start(rig.request(Provider::Claude, &gate.prompt()))
        .await
        .expect("start");
    gate.started().await;

    let begun = Instant::now();
    let response = supervisor
        .wait(runs(&[&started.run]), Some(1), None)
        .await
        .expect("a timeout is not an error");
    assert!(
        begun.elapsed() >= Duration::from_millis(900),
        "{:?}",
        begun.elapsed()
    );
    assert!(response.timed_out);
    assert_eq!(response.timeout_secs, 1);
    assert_eq!(response.runs[0].state, RunState::Running);
    assert!(response.cursor > 0);

    let polled = supervisor
        .wait(runs(&[&started.run]), Some(0), None)
        .await
        .expect("a zero timeout polls");
    assert!(polled.timed_out);
    gate.open();
    result_of(&supervisor, &started.run).await;
    supervisor.shutdown().await;
}

#[tokio::test]
async fn the_wait_ceiling_caps_the_timeout_asked_for() {
    let rig = Rig::new();
    let limits = Limits {
        wait_ceiling: Duration::from_secs(1),
        ..Limits::default()
    };
    let supervisor = Supervisor::new(rig.config_with(Provider::Claude, limits));
    let gate = rig.gate("ceiling");
    let started = supervisor
        .start(rig.request(Provider::Claude, &gate.prompt()))
        .await
        .expect("start");
    gate.started().await;
    let begun = Instant::now();
    let response = supervisor
        .wait(runs(&[&started.run]), Some(3600), None)
        .await
        .expect("wait");
    assert!(response.timed_out);
    assert_eq!(response.timeout_secs, 1);
    assert!(
        begun.elapsed() < Duration::from_secs(5),
        "{:?}",
        begun.elapsed()
    );
    gate.open();
    result_of(&supervisor, &started.run).await;
    supervisor.shutdown().await;
}

#[tokio::test]
async fn a_cursor_returns_at_once_for_a_finish_after_it_and_not_for_one_before_it() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let (ga, gb) = (rig.gate("a"), rig.gate("b"));
    let a = supervisor
        .start(rig.request(Provider::Claude, &ga.prompt()))
        .await
        .expect("start");
    let b = supervisor
        .start(rig.request(Provider::Claude, &gb.prompt()))
        .await
        .expect("start");
    ga.started().await;
    gb.started().await;

    ga.open();
    let first = supervisor
        .wait(runs(&[&a.run, &b.run]), Some(30), None)
        .await
        .expect("wait");
    assert!(!first.timed_out);
    let cursor = first.cursor;

    // `a` finished before the cursor and `b` has not finished: nothing is new.
    let quiet = supervisor
        .wait(runs(&[&a.run, &b.run]), Some(1), Some(cursor))
        .await
        .expect("wait");
    assert!(quiet.timed_out, "{quiet:#?}");
    assert!(
        quiet.runs.iter().all(|r| r.changed == Some(false)),
        "{quiet:#?}"
    );
    assert!(quiet.cursor >= cursor);

    gb.open();
    finished(&supervisor, &[&b.run]).await;
    let begun = Instant::now();
    let later = supervisor
        .wait(runs(&[&a.run, &b.run]), Some(30), Some(cursor))
        .await
        .expect("wait");
    assert!(
        begun.elapsed() < Duration::from_secs(2),
        "it must return at once"
    );
    assert!(!later.timed_out);
    let changed = |id: &str| {
        later
            .runs
            .iter()
            .find(|r| r.run == id)
            .expect("brief")
            .changed
    };
    assert_eq!(changed(&a.run), Some(false));
    assert_eq!(changed(&b.run), Some(true));

    // A cursor of zero sees every finish.
    let all = supervisor
        .wait(runs(&[&a.run]), Some(30), Some(0))
        .await
        .expect("wait");
    assert!(!all.timed_out);
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn a_cursor_wait_that_is_pending_wakes_when_a_run_finishes_after_it() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let gate = rig.gate("pending");
    let started = supervisor
        .start(rig.request(Provider::Claude, &gate.prompt()))
        .await
        .expect("start");
    gate.started().await;
    let cursor = supervisor.list().await.expect("list").cursor;

    let waiting = {
        let supervisor = supervisor.clone();
        let targets = runs(&[&started.run]);
        tokio::spawn(async move { supervisor.wait(targets, Some(30), Some(cursor)).await })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!waiting.is_finished());
    gate.open();
    let response = waiting.await.expect("task").expect("wait");
    assert!(!response.timed_out);
    assert_eq!(response.runs[0].changed, Some(true));
    assert!(response.cursor > cursor);
    supervisor.shutdown().await;
}

#[tokio::test]
async fn several_runs_finishing_together_are_all_reported_by_one_wait_or_the_next() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let gates = [rig.gate("1"), rig.gate("2"), rig.gate("3")];
    let mut ids = Vec::new();
    for gate in &gates {
        let started = supervisor
            .start(rig.request(Provider::Claude, &gate.prompt()))
            .await
            .expect("start");
        ids.push(started.run);
    }
    for gate in &gates {
        gate.started().await;
        gate.open();
    }
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let mut done = std::collections::HashSet::new();
    let mut cursor = None;
    for _ in 0..10 {
        let response = supervisor
            .wait(runs(&refs), Some(30), cursor)
            .await
            .expect("wait");
        assert!(!response.timed_out);
        for run in &response.runs {
            if run.changed.unwrap_or(true) && run.outcome.is_some() {
                done.insert(run.run.clone());
            }
        }
        cursor = Some(response.cursor);
        if done.len() == ids.len() {
            break;
        }
    }
    assert_eq!(
        done.len(),
        3,
        "every finish is seen exactly through the cursor"
    );
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn unknown_ids_are_reported_per_id_and_the_known_ones_still_count() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let gate = rig.gate("known");
    let started = supervisor
        .start(rig.request(Provider::Claude, &gate.prompt()))
        .await
        .expect("start");
    gate.started().await;
    let worker = started.worker.clone().expect("worker");

    let mixed = supervisor
        .wait(
            WaitTargets {
                runs: vec![started.run.clone(), "run-nope".to_string()],
                workers: vec!["worker-nope".to_string()],
            },
            Some(1),
            None,
        )
        .await
        .expect("an unknown id is not a failure");
    assert!(mixed.timed_out, "the known run is still running");
    assert_eq!(mixed.unknown, ["run-nope", "worker-nope"]);
    assert_eq!(mixed.runs.len(), 1);

    let begun = Instant::now();
    let none_known = supervisor
        .wait(runs(&["run-a", "run-b"]), Some(30), None)
        .await
        .expect("wait");
    assert!(
        begun.elapsed() < Duration::from_secs(2),
        "nothing to wait for"
    );
    assert!(!none_known.timed_out);
    assert_eq!(none_known.unknown, ["run-a", "run-b"]);

    let empty = supervisor
        .wait(WaitTargets::default(), Some(1), None)
        .await
        .expect_err("nothing named");
    assert!(matches!(empty, Error::InvalidArguments(_)), "{empty:?}");
    gate.open();
    result_of(&supervisor, &started.run).await;
    supervisor.close(&worker).await.expect("close");
    supervisor.shutdown().await;
}

#[tokio::test]
async fn a_worker_wait_ends_when_the_current_run_finishes_even_if_more_are_queued() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let (g1, g2) = (rig.gate("1"), rig.gate("2"));
    let first = supervisor
        .start(rig.request(Provider::Claude, &g1.prompt()))
        .await
        .expect("start");
    let worker = first.worker.clone().expect("worker");
    g1.started().await;
    let second = supervisor
        .follow_up(&worker, g2.prompt(), None)
        .await
        .expect("follow_up");

    let waiting = {
        let supervisor = supervisor.clone();
        let targets = workers(&[&worker]);
        tokio::spawn(async move { supervisor.wait(targets, Some(30), None).await })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!waiting.is_finished());
    g1.open();
    let response = waiting.await.expect("task").expect("wait");
    assert!(!response.timed_out);
    let brief = &response.workers[0];
    assert_eq!(brief.worker, worker);
    assert_eq!(brief.last_run.as_deref(), Some(first.run.as_str()));
    assert_eq!(brief.last_outcome, Some(Outcome::Completed));
    // The queued run has started in its place, so the worker is still busy.
    assert!(matches!(
        brief.state,
        WorkerState::Running | WorkerState::Queued
    ));

    g2.started().await;
    g2.open();
    result_of(&supervisor, &second.run).await;
    let idle = supervisor
        .wait(workers(&[&worker]), Some(30), None)
        .await
        .expect("wait");
    assert_eq!(idle.workers[0].state, WorkerState::Idle);
    assert!(!idle.timed_out, "an idle worker has nothing to wait for");
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}
