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

/// The answer of the fake's `large-utf8` mode: `é` (two bytes), `€` (three) and
/// `😀` (four), 8192 times over, 24576 characters.
fn utf8_answer() -> String {
    "é€😀".repeat(8192)
}

#[tokio::test]
async fn read_pages_multibyte_text_by_character_and_the_pages_add_up_exactly() {
    use delega_core::supervisor::limits::COMPACT_RESULT_TEXT_CHARS;
    use delega_core::supervisor::{Detail, ResultView};
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let started = supervisor
        .start(rig.request(Provider::Claude, "large-utf8"))
        .await
        .expect("start");
    let full = result_of(&supervisor, &started.run).await;
    let expected = utf8_answer();
    assert_eq!(full.text, expected);
    let total = expected.chars().count();
    assert_eq!(total, 24576);
    assert!(
        expected.len() > total * 2,
        "the answer has characters wider than two bytes"
    );
    let target = || Target::Run(started.run.clone());

    // Every window starts at a character, whichever width the character before
    // it has: 20000 is a multiple of three, so the first page ends after a
    // `€` and the second begins with a `😀`; the neighbours begin at an `é` and
    // at a `€`.
    for offset in [0, 1, 2, 3, 19_999, 20_000, 20_001, 24_575, total] {
        let want: String = expected
            .chars()
            .skip(offset)
            .take(COMPACT_RESULT_TEXT_CHARS)
            .collect();
        for detail in [Detail::Compact, Detail::Full] {
            let read = supervisor
                .read(target(), detail, None, Some(offset))
                .await
                .expect("read");
            let window = read.text_window.clone().expect("window");
            let text = match &read.result {
                Some(ResultView::Compact(result)) => result.text.clone(),
                Some(ResultView::Full(result)) => result.text.clone(),
                None => panic!("{read:#?}"),
            };
            assert_eq!(text, want, "offset {offset}");
            assert_eq!(window.offset, offset);
            assert_eq!(window.total, total);
            assert_eq!(window.length, want.chars().count());
            assert_eq!(window.more, offset + window.length < total);
        }
    }

    // Following the offsets each window names reassembles the answer exactly.
    let mut assembled = String::new();
    let mut offset = 0;
    let mut windows = 0;
    loop {
        let read = supervisor
            .read(target(), Detail::Compact, None, Some(offset))
            .await
            .expect("read");
        let window = read.text_window.clone().expect("window");
        let Some(ResultView::Compact(result)) = &read.result else {
            panic!("{read:#?}");
        };
        assembled.push_str(&result.text);
        windows += 1;
        if !window.more {
            break;
        }
        offset = window.offset + window.length;
    }
    assert_eq!(windows, 2);
    assert_eq!(assembled, expected);
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn paging_by_worker_follows_the_latest_run_so_a_follow_up_changes_the_run_between_pages() {
    use delega_core::supervisor::limits::COMPACT_RESULT_TEXT_CHARS;
    use delega_core::supervisor::{Detail, ResultView};
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let started = supervisor
        .start(rig.request(Provider::Claude, "large-utf8"))
        .await
        .expect("start");
    let worker = started.worker.clone().expect("worker");
    result_of(&supervisor, &started.run).await;
    let expected = utf8_answer();
    let total = expected.chars().count();

    // The first page, read by worker, comes from the first run.
    let page = supervisor
        .read(
            Target::Worker(worker.clone()),
            Detail::Compact,
            None,
            Some(0),
        )
        .await
        .expect("read");
    assert_eq!(page.run.as_deref(), Some(started.run.as_str()));
    let window = page.text_window.clone().expect("window");
    assert!(window.more);
    assert_eq!(window.length, COMPACT_RESULT_TEXT_CHARS);
    let next = window.offset + window.length;

    // A follow-up finishes before the second page is asked for.
    let follow = supervisor
        .follow_up(&worker, "echo:second", None)
        .await
        .expect("follow_up");
    let second = result_of(&supervisor, &follow.run).await;
    assert_ne!(follow.run, started.run);

    // By worker, the second page is the new run's: an empty window that reads
    // as done, but the response names the other run and its own total.
    let by_worker = supervisor
        .read(Target::Worker(worker), Detail::Compact, None, Some(next))
        .await
        .expect("read");
    assert_eq!(by_worker.run.as_deref(), Some(follow.run.as_str()));
    assert_ne!(by_worker.run.as_deref(), Some(started.run.as_str()));
    let window = by_worker.text_window.clone().expect("window");
    assert_eq!(window.total, second.text.chars().count());
    assert_ne!(window.total, total);
    assert_eq!((window.length, window.more), (0, false));

    // By run id, the second page is still the rest of the first answer.
    let by_run = supervisor
        .read(
            Target::Run(started.run.clone()),
            Detail::Compact,
            None,
            Some(next),
        )
        .await
        .expect("read");
    assert_eq!(by_run.run.as_deref(), Some(started.run.as_str()));
    let window = by_run.text_window.clone().expect("window");
    assert_eq!((window.total, window.more), (total, false));
    let Some(ResultView::Compact(rest)) = &by_run.result else {
        panic!("{by_run:#?}");
    };
    let want: String = expected.chars().skip(next).collect();
    assert_eq!(rest.text, want);
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

/// Start `count` requests at once from separate tasks and return the responses.
async fn start_together(
    supervisor: &Supervisor,
    requests: Vec<delega_core::contract::request::Request>,
) -> Vec<delega_core::supervisor::StartResponse> {
    let mut tasks = tokio::task::JoinSet::new();
    for request in requests {
        let supervisor = supervisor.clone();
        tasks.spawn(async move { supervisor.start(request).await.expect("start") });
    }
    let mut responses = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        responses.push(joined.expect("the start task did not panic"));
    }
    responses
}

fn refusal_kind(response: &delega_core::supervisor::StartResponse) -> Option<&str> {
    response.error.as_ref().map(|e| e.kind.as_str())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forty_concurrent_starts_are_all_admitted_when_the_filesystem_answers() {
    let rig = Rig::new();
    let limits = Limits {
        max_live_workers: 64,
        ..Limits::default()
    };
    let supervisor = Supervisor::new(rig.config_with(Provider::Claude, limits));
    let requests = (0..40)
        .map(|n| rig.request(Provider::Claude, &format!("echo:{n}")))
        .collect();
    let responses = start_together(&supervisor, requests).await;
    assert_eq!(responses.len(), 40);
    for response in &responses {
        assert_eq!(refusal_kind(response), None, "{response:#?}");
        assert!(response.worker.is_some(), "{response:#?}");
    }
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn thirty_concurrent_starts_on_a_missing_directory_are_all_refused_for_the_directory() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let requests = (0..30)
        .map(|n| {
            let mut request = rig.request(Provider::Claude, &format!("echo:{n}"));
            request.cwd = Some(rig.root().join("no-such-directory"));
            request
        })
        .collect();
    let responses = start_together(&supervisor, requests).await;
    assert_eq!(responses.len(), 30);
    for response in &responses {
        assert_eq!(
            refusal_kind(response),
            Some("cwd_unusable"),
            "{response:#?}"
        );
    }
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn twenty_concurrent_starts_with_default_limits_refuse_four_for_the_live_worker_limit() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let requests = (0..20)
        .map(|n| rig.request(Provider::Claude, &format!("echo:{n}")))
        .collect();
    let responses = start_together(&supervisor, requests).await;
    let refused: Vec<_> = responses.iter().filter(|r| r.error.is_some()).collect();
    assert_eq!(responses.len() - refused.len(), 16, "{responses:#?}");
    assert_eq!(refused.len(), 4, "{refused:#?}");
    for response in refused {
        assert_eq!(refusal_kind(response), Some("limit_reached"));
        let message = &response.error.as_ref().expect("error").message;
        assert!(message.contains("live workers"), "{message}");
    }
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}
