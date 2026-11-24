//! `wait`: with and without a cursor, on timeout, with several runs, with
//! unknown ids.

use std::time::{Duration, Instant};

use super::{Rig, finished, result_of};
use delega_core::contract::outcome::Outcome;
use delega_core::contract::request::Provider;
use delega_core::supervisor::{
    Error, Limits, RunState, Supervisor, Target, WaitLimit, WaitTargets, WorkerState,
};

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

    let mut waits = supervisor.observe_waits();
    let waiting = {
        let supervisor = supervisor.clone();
        let targets = runs(&[&a.run, &b.run]);
        tokio::spawn(async move { supervisor.wait(targets, Some(30), None).await })
    };
    waits.registered().await;
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
    rig.assert_nothing_left().await;
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
        begun.elapsed() < Duration::from_secs(30),
        "{:?}",
        begun.elapsed()
    );
    gate.open();
    result_of(&supervisor, &started.run).await;
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn a_wait_with_no_timeout_uses_the_configured_default_and_the_ceiling_still_caps_it() {
    let rig = Rig::new();
    let limits = Limits {
        wait_default: Duration::from_secs(1),
        wait_ceiling: Duration::from_secs(2),
        ..Limits::default()
    };
    let supervisor = Supervisor::new(rig.config_with(Provider::Claude, limits));
    let gate = rig.gate("default-wait");
    let started = supervisor
        .start(rig.request(Provider::Claude, &gate.prompt()))
        .await
        .expect("start");
    gate.started().await;
    let response = supervisor
        .wait(runs(&[&started.run]), None, None)
        .await
        .expect("wait");
    assert!(response.timed_out);
    assert_eq!(response.timeout_secs, 1);
    let listed = supervisor.list().await.expect("list");
    assert_eq!(listed.limits.default_wait_secs, 1);
    assert_eq!(listed.limits.wait_ceiling_secs, 2);
    // A default above the ceiling is capped like any other timeout.
    let capped = Limits {
        wait_default: Duration::from_secs(50),
        wait_ceiling: Duration::from_secs(1),
        ..Limits::default()
    };
    let second = Supervisor::new(rig.config_with(Provider::Claude, capped));
    let gate2 = rig.gate("default-capped");
    let started2 = second
        .start(rig.request(Provider::Claude, &gate2.prompt()))
        .await
        .expect("start");
    gate2.started().await;
    let response = second
        .wait(runs(&[&started2.run]), None, None)
        .await
        .expect("wait");
    assert_eq!(response.timeout_secs, 1);
    gate.open();
    gate2.open();
    result_of(&supervisor, &started.run).await;
    result_of(&second, &started2.run).await;
    supervisor.shutdown().await;
    second.shutdown().await;
    rig.assert_nothing_left().await;
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
        begun.elapsed() < Duration::from_secs(10),
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

    let mut waits = supervisor.observe_waits();
    let waiting = {
        let supervisor = supervisor.clone();
        let targets = runs(&[&started.run]);
        tokio::spawn(async move { supervisor.wait(targets, Some(30), Some(cursor)).await })
    };
    waits.registered().await;
    assert!(!waiting.is_finished());
    gate.open();
    let response = waiting.await.expect("task").expect("wait");
    assert!(!response.timed_out);
    assert_eq!(response.runs[0].changed, Some(true));
    assert!(response.cursor > cursor);
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
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
        begun.elapsed() < Duration::from_secs(10),
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
    rig.assert_nothing_left().await;
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

    let mut waits = supervisor.observe_waits();
    let waiting = {
        let supervisor = supervisor.clone();
        let targets = workers(&[&worker]);
        tokio::spawn(async move { supervisor.wait(targets, Some(30), None).await })
    };
    waits.registered().await;
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

#[tokio::test]
async fn shutdown_releases_a_wait_whose_cursor_no_run_will_pass() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let gate = rig.gate("shutdown-wait");
    let started = supervisor
        .start(rig.request(Provider::Claude, &gate.prompt()))
        .await
        .expect("start");
    gate.started().await;
    let mut waits = supervisor.observe_waits();
    let waiting = {
        let supervisor = supervisor.clone();
        let targets = runs(&[&started.run]);
        tokio::spawn(async move {
            supervisor
                .wait(targets, Some(300), Some(u64::MAX / 2))
                .await
        })
    };
    let registration = waits.registered().await;
    assert!(!waiting.is_finished());
    let report = supervisor.shutdown().await;
    assert_eq!(report.runs_stopped, 1);
    registration.removed().await;
    let released = tokio::time::timeout(Duration::from_secs(120), waiting)
        .await
        .expect("shutdown released the wait")
        .expect("task")
        .expect("wait answers");
    assert!(!released.timed_out, "{released:#?}");
    // A wait with a cursor that arrives after shutdown began answers at once.
    let late = supervisor
        .wait(runs(&[&started.run]), Some(300), Some(u64::MAX / 2))
        .await
        .expect("wait answers");
    assert!(!late.timed_out, "{late:#?}");
    rig.assert_nothing_left().await;
}

/// How long a call that must not block may take. It does no waiting, so this
/// is generous for a loaded machine and far below any wait a test parks.
const AT_ONCE: Duration = Duration::from_secs(10);
/// The timeout of a wait a test parks: longer than the test, so it only
/// returns because something woke it.
const PARKED_SECS: u64 = 120;

fn limited(rig: &Rig, max_waits_in_flight: usize) -> Supervisor {
    let limits = Limits {
        max_waits_in_flight,
        ..Limits::default()
    };
    Supervisor::new(rig.config_with(Provider::Claude, limits))
}

fn park(
    supervisor: &Supervisor,
    targets: WaitTargets,
) -> tokio::task::JoinHandle<Result<delega_core::supervisor::WaitResponse, Error>> {
    let supervisor = supervisor.clone();
    tokio::spawn(async move { supervisor.wait(targets, Some(PARKED_SECS), None).await })
}

async fn assert_waits_in_flight(supervisor: &Supervisor, count: usize) {
    let listed = supervisor.list().await.expect("list");
    assert_eq!(listed.limits.waits_in_flight, count, "{listed:#?}");
}

#[tokio::test]
async fn a_wait_past_the_limit_returns_at_once_and_list_and_interrupt_still_answer() {
    let rig = Rig::new();
    let supervisor = limited(&rig, 2);
    let (held, quick) = (rig.gate("held"), rig.gate("quick"));
    let a = supervisor
        .start(rig.request(Provider::Claude, &held.prompt()))
        .await
        .expect("start");
    let b = supervisor
        .start(rig.request(Provider::Claude, &quick.prompt()))
        .await
        .expect("start");
    held.started().await;
    quick.open();
    result_of(&supervisor, &b.run).await;

    let mut waits = supervisor.observe_waits();
    let parked = [
        park(&supervisor, runs(&[&a.run])),
        park(&supervisor, runs(&[&a.run])),
    ];
    let registrations = [waits.registered().await, waits.registered().await];
    assert_waits_in_flight(&supervisor, 2).await;

    // The next wait that would block is answered instead of queued.
    let begun = Instant::now();
    let refused = supervisor
        .wait(runs(&[&a.run]), Some(PARKED_SECS), None)
        .await
        .expect("a wait past the limit is a response, not an error");
    assert!(begun.elapsed() < AT_ONCE, "{:?}", begun.elapsed());
    assert_eq!(refused.limit_reached, Some(WaitLimit::MaxWaitsInFlight));
    assert_eq!(refused.timeout_secs, 0);
    assert!(refused.timed_out);
    assert!(!refused.released_by_shutdown);
    assert_eq!(refused.runs[0].state, RunState::Running);
    assert!(refused.cursor > 0);
    // The same with a cursor: the cursor it gets back is good for the next call.
    let polled = supervisor
        .wait(runs(&[&a.run]), Some(PARKED_SECS), Some(refused.cursor))
        .await
        .expect("wait");
    assert_eq!(polled.limit_reached, Some(WaitLimit::MaxWaitsInFlight));
    assert_eq!(polled.runs[0].changed, Some(false));

    // A wait given no time never blocks, so the limit does not apply to it.
    let poll = supervisor
        .wait(runs(&[&a.run]), Some(0), None)
        .await
        .expect("wait");
    assert_eq!(poll.limit_reached, None, "{poll:#?}");
    assert!(poll.timed_out);
    assert_eq!(poll.timeout_secs, 0);
    assert_eq!(poll.runs[0].state, RunState::Running);

    // A wait with something to report takes no slot and is served at the limit.
    let begun = Instant::now();
    let ready = supervisor
        .wait(runs(&[&a.run, &b.run]), Some(PARKED_SECS), None)
        .await
        .expect("wait");
    assert!(begun.elapsed() < AT_ONCE, "{:?}", begun.elapsed());
    assert_eq!(ready.limit_reached, None);
    assert!(!ready.timed_out);
    let unknown = supervisor
        .wait(runs(&["no-such-run"]), Some(PARKED_SECS), None)
        .await
        .expect("wait");
    assert_eq!(unknown.limit_reached, None);
    assert_eq!(unknown.unknown, ["no-such-run"]);

    // The parked waits hold nothing else back.
    let begun = Instant::now();
    let listed = supervisor.list().await.expect("list");
    assert!(begun.elapsed() < AT_ONCE, "{:?}", begun.elapsed());
    assert_eq!(listed.limits.max_waits_in_flight, 2);
    assert_eq!(listed.limits.waits_in_flight, 2);
    let stopped = supervisor
        .interrupt(Target::Run(a.run.clone()))
        .await
        .expect("interrupt answers while the waits are parked");
    assert!(stopped.settled, "{stopped:#?}");

    // The interrupt finished the run, which wakes the parked waits.
    for registration in registrations {
        registration.removed().await;
    }
    for wait in parked {
        let response = wait.await.expect("task").expect("wait");
        assert!(!response.timed_out, "{response:#?}");
        assert_eq!(response.limit_reached, None);
        assert_eq!(response.timeout_secs, PARKED_SECS);
    }
    assert_waits_in_flight(&supervisor, 0).await;
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn a_wait_that_times_out_frees_its_slot() {
    let rig = Rig::new();
    let supervisor = limited(&rig, 1);
    let gate = rig.gate("timeout");
    let started = supervisor
        .start(rig.request(Provider::Claude, &gate.prompt()))
        .await
        .expect("start");
    gate.started().await;

    let mut waits = supervisor.observe_waits();
    let timed_out = supervisor
        .wait(runs(&[&started.run]), Some(1), None)
        .await
        .expect("wait");
    waits.registered().await.removed().await;
    assert!(timed_out.timed_out);
    assert_eq!(timed_out.limit_reached, None);
    assert_eq!(timed_out.timeout_secs, 1);
    let listed = supervisor.list().await.expect("list");
    assert_eq!(listed.limits.waits_in_flight, 0, "{listed:#?}");

    // The slot is free: the next wait parks instead of being turned away.
    let parked = park(&supervisor, runs(&[&started.run]));
    let registration = waits.registered().await;
    assert_waits_in_flight(&supervisor, 1).await;
    gate.open();
    registration.removed().await;
    let response = parked.await.expect("task").expect("wait");
    assert_eq!(response.limit_reached, None);
    assert!(!response.timed_out);
    assert_waits_in_flight(&supervisor, 0).await;
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn a_wait_whose_default_timeout_is_zero_is_a_poll_and_is_never_turned_away_for_the_limit() {
    let rig = Rig::new();
    let limits = Limits {
        max_waits_in_flight: 1,
        wait_default: Duration::ZERO,
        ..Limits::default()
    };
    let supervisor = Supervisor::new(rig.config_with(Provider::Claude, limits));
    let gate = rig.gate("poll");
    let started = supervisor
        .start(rig.request(Provider::Claude, &gate.prompt()))
        .await
        .expect("start");
    gate.started().await;
    let mut waits = supervisor.observe_waits();
    let parked = park(&supervisor, runs(&[&started.run]));
    let registration = waits.registered().await;
    assert_waits_in_flight(&supervisor, 1).await;

    for timeout in [None, Some(0)] {
        let poll = supervisor
            .wait(runs(&[&started.run]), timeout, None)
            .await
            .expect("wait");
        assert_eq!(poll.limit_reached, None, "{timeout:?}: {poll:#?}");
        assert!(poll.timed_out, "{timeout:?}");
        assert_eq!(poll.timeout_secs, 0, "{timeout:?}");
    }
    // The polls took no slot and left the parked wait alone.
    let listed = supervisor.list().await.expect("list");
    assert_eq!(listed.limits.waits_in_flight, 1, "{listed:#?}");
    // A poll still reports a finish.
    gate.open();
    registration.removed().await;
    let woken = parked.await.expect("task").expect("wait");
    assert!(!woken.timed_out, "{woken:#?}");
    let poll = supervisor
        .wait(runs(&[&started.run]), None, None)
        .await
        .expect("wait");
    assert!(!poll.timed_out, "{poll:#?}");
    assert_eq!(poll.runs[0].state, RunState::Finished);
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn a_wait_whose_caller_went_away_frees_its_slot() {
    let rig = Rig::new();
    let supervisor = limited(&rig, 1);
    let gate = rig.gate("dropped");
    let started = supervisor
        .start(rig.request(Provider::Claude, &gate.prompt()))
        .await
        .expect("start");
    gate.started().await;

    let mut waits = supervisor.observe_waits();
    let abandoned = park(&supervisor, runs(&[&started.run]));
    let registration = waits.registered().await;
    assert_waits_in_flight(&supervisor, 1).await;
    let refused = supervisor
        .wait(runs(&[&started.run]), Some(PARKED_SECS), None)
        .await
        .expect("wait");
    assert_eq!(refused.limit_reached, Some(WaitLimit::MaxWaitsInFlight));

    // Dropping the future mid-wait is what a cancelled MCP request does.
    abandoned.abort();
    assert!(abandoned.await.expect_err("aborted").is_cancelled());
    // Cancellation drops the receiver. A command makes the actor prune it.
    let listed = supervisor.list().await.expect("list");
    registration.removed().await;
    assert_eq!(listed.limits.waits_in_flight, 0, "{listed:#?}");

    let parked = park(&supervisor, runs(&[&started.run]));
    let registration = waits.registered().await;
    assert_waits_in_flight(&supervisor, 1).await;
    gate.open();
    registration.removed().await;
    let response = parked.await.expect("task").expect("wait");
    assert_eq!(response.limit_reached, None);
    assert!(!response.timed_out);
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn shutdown_frees_the_slots_of_the_waits_it_releases() {
    let rig = Rig::new();
    let supervisor = limited(&rig, 1);
    let gate = rig.gate("released");
    let started = supervisor
        .start(rig.request(Provider::Claude, &gate.prompt()))
        .await
        .expect("start");
    gate.started().await;
    let mut waits = supervisor.observe_waits();
    let parked = {
        let supervisor = supervisor.clone();
        let targets = runs(&[&started.run]);
        tokio::spawn(async move {
            supervisor
                .wait(targets, Some(PARKED_SECS), Some(u64::MAX / 2))
                .await
        })
    };
    let registration = waits.registered().await;
    assert_waits_in_flight(&supervisor, 1).await;
    supervisor.shutdown().await;
    registration.removed().await;
    let response = parked.await.expect("task").expect("wait");
    assert!(response.released_by_shutdown, "{response:#?}");
    assert_eq!(response.limit_reached, None);
    assert_eq!(
        supervisor
            .list()
            .await
            .expect("list")
            .limits
            .waits_in_flight,
        0
    );
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn a_limit_of_no_waits_is_raised_to_one_so_a_wait_can_still_block() {
    let rig = Rig::new();
    let supervisor = limited(&rig, 0);
    let gate = rig.gate("one");
    let started = supervisor
        .start(rig.request(Provider::Claude, &gate.prompt()))
        .await
        .expect("start");
    gate.started().await;
    assert_eq!(
        supervisor
            .list()
            .await
            .expect("list")
            .limits
            .max_waits_in_flight,
        1
    );
    let mut waits = supervisor.observe_waits();
    let parked = park(&supervisor, runs(&[&started.run]));
    let registration = waits.registered().await;
    assert_waits_in_flight(&supervisor, 1).await;
    gate.open();
    registration.removed().await;
    let response = parked.await.expect("task").expect("wait");
    assert_eq!(response.limit_reached, None);
    assert!(!response.timed_out);
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[test]
fn the_default_limit_on_waits_is_the_named_constant() {
    assert_eq!(
        Limits::default().max_waits_in_flight,
        delega_core::supervisor::limits::MAX_WAITS_IN_FLIGHT
    );
}
