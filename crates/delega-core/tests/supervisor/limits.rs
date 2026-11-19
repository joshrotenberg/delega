//! Limits: reached, freed, and retention.

use std::time::Duration;

use super::{Rig, assert_state, finished, read_compact, result_of};
use delega_core::contract::outcome::Outcome;
use delega_core::contract::request::Provider;
use delega_core::supervisor::{Error, Limits, RunState, Supervisor, Target, WorkerState};

#[tokio::test]
async fn defaults_are_the_named_constants() {
    let limits = Limits::default();
    assert_eq!(
        limits.max_live_workers,
        delega_core::supervisor::limits::MAX_LIVE_WORKERS
    );
    assert_eq!(
        limits.max_running_runs,
        delega_core::supervisor::limits::MAX_RUNNING_RUNS
    );
    assert_eq!(
        limits.max_queued_per_worker,
        delega_core::supervisor::limits::MAX_QUEUED_PER_WORKER
    );
    assert_eq!(
        limits.retained_results,
        delega_core::supervisor::limits::RETAINED_RESULTS
    );
    assert_eq!(limits.retention, delega_core::supervisor::limits::RETENTION);
    assert_eq!(
        limits.wait_ceiling,
        delega_core::supervisor::limits::WAIT_CEILING
    );
    assert_eq!(
        limits.retained_bytes,
        delega_core::supervisor::limits::RETAINED_BYTES
    );
    assert_eq!(
        delega_core::supervisor::limits::DEFAULT_WAIT,
        Duration::from_secs(30)
    );

    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let view = supervisor.list().await.expect("list").limits;
    assert_eq!(view.max_live_workers, limits.max_live_workers);
    assert_eq!(view.max_running_runs, limits.max_running_runs);
    assert_eq!(view.max_queued_per_worker, limits.max_queued_per_worker);
    assert_eq!(view.retention_secs, limits.retention.as_secs());
    assert_eq!(view.wait_ceiling_secs, limits.wait_ceiling.as_secs());
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
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
    assert_state(&supervisor, &started[2].run, RunState::Queued).await;
    gates[2].assert_not_started();
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
async fn results_stay_readable_after_close() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
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
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn results_are_gone_after_the_retention_time_and_a_closed_worker_goes_with_them() {
    let rig = Rig::new();
    let limits = Limits {
        retention: Duration::from_millis(100),
        ..Limits::default()
    };
    let supervisor = Supervisor::new(rig.config_with(Provider::Claude, limits));
    let started = supervisor
        .start(rig.request(Provider::Claude, "echo:aged"))
        .await
        .expect("start");
    let worker = started.worker.clone().expect("worker");
    // Only the wait: by the time it answers the result may already be gone.
    finished(&supervisor, &[&started.run]).await;

    // Sleeping past the retention time can only make the result older.
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        supervisor
            .read(
                Target::Run(started.run.clone()),
                Default::default(),
                None,
                None
            )
            .await
            .expect_err("the result has aged out"),
        Error::UnknownRun(started.run.clone())
    );
    // A live worker is never dropped for age, even with no result kept.
    let list = supervisor.list().await.expect("list");
    assert_eq!(list.workers.len(), 1, "{list:#?}");
    assert_eq!(list.workers[0].brief.state, WorkerState::Idle);
    assert_eq!(list.limits.retained_now, 0);

    // Closing it then leaves nothing to keep it listed.
    supervisor.close(&worker).await.expect("close");
    let list = supervisor.list().await.expect("list");
    assert!(list.workers.is_empty(), "{list:#?}");
    assert_eq!(
        supervisor
            .read(
                Target::Worker(worker.clone()),
                Default::default(),
                None,
                None
            )
            .await
            .expect_err("the closed worker has gone"),
        Error::UnknownWorker(worker)
    );
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn the_byte_bound_drops_the_oldest_results_and_always_keeps_the_newest() {
    let rig = Rig::new();
    // Each result weighs a little over 2 KiB, so 5 KB keeps two.
    let limits = Limits {
        retained_bytes: 5_000,
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
        runs.push(started.run);
    }
    let list = supervisor.list().await.expect("list");
    assert_eq!(list.limits.retained_bytes, 5_000);
    assert!(list.limits.retained_bytes_now <= 5_000, "{:?}", list.limits);
    assert!(list.limits.retained_now >= 1 && list.limits.retained_now < 5);
    assert_eq!(
        read_compact(&supervisor, &runs[4]).await.outcome,
        Some(Outcome::Completed),
        "the newest is kept"
    );
    assert!(matches!(
        supervisor
            .read(Target::Run(runs[0].clone()), Default::default(), None, None)
            .await,
        Err(Error::UnknownRun(_))
    ));
    supervisor.shutdown().await;

    // A bound smaller than one result still keeps the newest.
    let limits = Limits {
        retained_bytes: 1,
        ..Limits::default()
    };
    let supervisor = Supervisor::new(rig.config_with(Provider::Claude, limits));
    for n in 0..3 {
        let started = supervisor
            .start(rig.request(Provider::Claude, &format!("echo:b{n}")))
            .await
            .expect("start");
        finished(&supervisor, &[&started.run]).await;
    }
    assert_eq!(
        supervisor.list().await.expect("list").limits.retained_now,
        1
    );
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn a_compact_read_cuts_a_long_answer_and_says_how_long_it_was_while_full_returns_all() {
    use delega_core::supervisor::limits::COMPACT_RESULT_TEXT_CHARS;
    use delega_core::supervisor::{Detail, ResultView};
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let started = supervisor
        .start(rig.request(Provider::Claude, "large"))
        .await
        .expect("start");
    let full = result_of(&supervisor, &started.run).await;
    let total = full.text.chars().count();
    assert!(total > COMPACT_RESULT_TEXT_CHARS, "{total}");

    let compact = read_compact(&supervisor, &started.run).await;
    let Some(ResultView::Compact(result)) = &compact.result else {
        panic!("{compact:#?}");
    };
    assert!(result.text_truncated);
    assert_eq!(result.text_chars, Some(total));
    assert_eq!(result.text.chars().count(), COMPACT_RESULT_TEXT_CHARS);
    assert!(
        full.text.starts_with(&result.text),
        "a prefix, not a summary"
    );

    // The same cut shows through interrupt and close, which are compact too.
    let closed = supervisor
        .close(started.worker.as_deref().expect("worker"))
        .await
        .expect("close");
    assert!(closed.result.is_none(), "nothing was running to stop");

    let read = supervisor
        .read(Target::Run(started.run.clone()), Detail::Full, None, None)
        .await
        .expect("read");
    let Some(ResultView::Full(whole)) = read.result else {
        panic!("full read");
    };
    assert_eq!(whole.text.chars().count(), total);

    // A short answer is not marked.
    let short = supervisor
        .start(rig.request(Provider::Claude, "echo:short"))
        .await
        .expect("start");
    result_of(&supervisor, &short.run).await;
    let json = serde_json::to_value(read_compact(&supervisor, &short.run).await).expect("json");
    assert!(json["result"].get("text_truncated").is_none(), "{json}");
    assert!(json["result"].get("text_chars").is_none(), "{json}");
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
                .read(
                    Target::Run(gone.run.clone()),
                    Default::default(),
                    None,
                    None
                )
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

#[tokio::test]
async fn a_queue_limit_of_zero_means_no_waiting_behind_a_running_run_not_no_follow_ups() {
    let rig = Rig::new();
    let limits = Limits {
        max_queued_per_worker: 0,
        ..Limits::default()
    };
    let supervisor = Supervisor::new(rig.config_with(Provider::Claude, limits));
    let gate = rig.gate("zero");
    let first = supervisor
        .start(rig.request(Provider::Claude, &gate.prompt()))
        .await
        .expect("start");
    let worker = first.worker.clone().expect("worker");
    gate.started().await;

    // Behind a running run there is no room to wait.
    let refused = supervisor
        .follow_up(&worker, "echo:waits", None)
        .await
        .expect("follow_up answers");
    assert_eq!(refused.outcome, Some(Outcome::Refused));
    assert_eq!(refused.error.expect("reason").kind, "queue_full");

    // An idle worker takes one, which starts at once and is not queued.
    gate.open();
    result_of(&supervisor, &first.run).await;
    let accepted = supervisor
        .follow_up(&worker, "echo:now", None)
        .await
        .expect("follow_up");
    assert_eq!(accepted.outcome, None, "{accepted:?}");
    assert_eq!(accepted.state, RunState::Running);
    assert_eq!(
        result_of(&supervisor, &accepted.run).await.outcome,
        Outcome::Completed
    );
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn a_compact_read_leaves_out_a_large_structured_answer_and_says_how_big_it_was() {
    use delega_core::supervisor::limits::COMPACT_STRUCTURED_BYTES;
    use delega_core::supervisor::{Detail, ResultView};
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let mut request = rig.request(Provider::Claude, "structured-large");
    request.answer_schema = Some(serde_json::json!({"type": "object"}));
    let started = supervisor.start(request).await.expect("start");
    let full = result_of(&supervisor, &started.run).await;
    let whole = full.structured.clone().expect("a structured answer");
    let size = whole.to_string().len();
    assert!(size > COMPACT_STRUCTURED_BYTES, "{size}");

    let compact = read_compact(&supervisor, &started.run).await;
    let Some(ResultView::Compact(result)) = &compact.result else {
        panic!("{compact:#?}");
    };
    assert!(result.structured.is_none());
    assert!(result.structured_omitted);
    assert_eq!(result.structured_bytes, Some(size));
    assert_eq!(
        result.structured_valid,
        Some(true),
        "validity still reported"
    );
    let read = supervisor
        .read(Target::Run(started.run.clone()), Detail::Full, None, None)
        .await
        .expect("read");
    let Some(ResultView::Full(whole_result)) = read.result else {
        panic!("full read");
    };
    assert_eq!(whole_result.structured, Some(whole));

    // A small one is returned as it is, unmarked.
    let mut small = rig.request(Provider::Claude, "structured");
    small.answer_schema = Some(serde_json::json!({"type": "object"}));
    let small = supervisor.start(small).await.expect("start");
    result_of(&supervisor, &small.run).await;
    let json = serde_json::to_value(read_compact(&supervisor, &small.run).await).expect("json");
    assert_eq!(json["result"]["structured"]["answer"], 42, "{json}");
    assert!(json["result"].get("structured_omitted").is_none(), "{json}");
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn read_pages_a_long_answer_by_character_offset_with_the_total() {
    use delega_core::supervisor::limits::COMPACT_RESULT_TEXT_CHARS;
    use delega_core::supervisor::{Detail, ResultView};
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let started = supervisor
        .start(rig.request(Provider::Claude, "large"))
        .await
        .expect("start");
    let full = result_of(&supervisor, &started.run).await;
    let total = full.text.chars().count();
    let target = || Target::Run(started.run.clone());

    // Page through the whole answer with the offsets each window names.
    let mut assembled = String::new();
    let mut offset = 0;
    let mut windows = 0;
    loop {
        let read = supervisor
            .read(target(), Detail::Compact, None, Some(offset))
            .await
            .expect("read");
        let window = read
            .text_window
            .clone()
            .expect("a paged read says where it is");
        let Some(ResultView::Compact(result)) = &read.result else {
            panic!("{read:#?}");
        };
        assert_eq!(window.offset, offset);
        assert_eq!(window.total, total);
        assert_eq!(window.length, result.text.chars().count());
        assert!(window.length <= COMPACT_RESULT_TEXT_CHARS);
        assert_eq!(result.text_chars, Some(total));
        assembled.push_str(&result.text);
        windows += 1;
        if !window.more {
            break;
        }
        offset = window.offset + window.length;
    }
    assert_eq!(assembled, full.text, "the windows add up to the answer");
    assert_eq!(windows, total.div_ceil(COMPACT_RESULT_TEXT_CHARS));

    // Full detail takes an offset too and returns the same window; without one
    // it returns everything.
    let read = supervisor
        .read(target(), Detail::Full, None, Some(total - 5))
        .await
        .expect("read");
    let Some(ResultView::Full(tail)) = read.result else {
        panic!("full read");
    };
    assert_eq!(tail.text.chars().count(), 5);
    assert!(!read.text_window.expect("window").more);
    let read = supervisor
        .read(target(), Detail::Full, None, None)
        .await
        .expect("read");
    assert!(read.text_window.is_none());
    let Some(ResultView::Full(all)) = read.result else {
        panic!("full read");
    };
    assert_eq!(all.text.chars().count(), total);

    // Past the end is an empty window with the total, not an error.
    let read = supervisor
        .read(target(), Detail::Compact, None, Some(total + 1000))
        .await
        .expect("read");
    let window = read.text_window.expect("window");
    assert_eq!(
        (window.length, window.total, window.more),
        (0, total, false)
    );
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}
