//! Start, read, follow up, and refusal: the life of a worker and its runs.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use super::{BOTH, Rig, finished, read_compact, read_full, result_of};
use crate::contract::outcome::Outcome;
use crate::contract::request::{Provider, Request};
use crate::supervisor::{Detail, Error, ResultView, RunState, Target, WorkerState};

async fn a_started_run_ends_in_one_valid_completed_result(provider: Provider) {
    let rig = Rig::new();
    let supervisor = rig.supervisor(provider);
    let started = supervisor
        .start(rig.request(provider, "success"))
        .await
        .expect("start");
    assert_eq!(started.state, RunState::Running);
    assert!(started.worker.is_some() && started.outcome.is_none());

    let result = result_of(&supervisor, &started.run).await;
    assert_eq!(result.outcome, Outcome::Completed);
    assert_eq!(result.text, "fake final answer");
    assert_eq!(result.run, started.run);
    assert_eq!(
        result.worker.as_ref().map(|w| &w.id),
        started.worker.as_ref()
    );
    assert_eq!(result.cleanup, crate::contract::result::Cleanup::Confirmed);
    assert!(result.timing.started_at.is_some());

    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn a_started_run_ends_in_one_valid_completed_result_for_each_provider() {
    for provider in BOTH {
        a_started_run_ends_in_one_valid_completed_result(provider).await;
    }
}

#[tokio::test]
async fn a_worker_is_idle_after_its_run_and_list_shows_its_session() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let started = supervisor
        .start(rig.request(Provider::Claude, "success"))
        .await
        .expect("start");
    result_of(&supervisor, &started.run).await;

    let list = supervisor.list().await.expect("list");
    assert_eq!(list.workers.len(), 1);
    let worker = &list.workers[0];
    assert_eq!(worker.brief.state, WorkerState::Idle);
    assert_eq!(worker.brief.queue_length, 0);
    assert_eq!(worker.brief.current_run, None);
    assert_eq!(worker.brief.last_run.as_deref(), Some(started.run.as_str()));
    assert_eq!(worker.brief.last_outcome, Some(Outcome::Completed));
    assert_eq!(worker.session_id.as_deref(), Some(rig.session()));
    assert_eq!(list.limits.live_workers, 1);
    assert_eq!(list.limits.running_runs, 0);
    assert_eq!(list.limits.retained_now, 1);

    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn a_refused_start_is_a_finished_run_without_a_worker_that_read_returns() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let mut request = rig.request(Provider::Claude, "   ");
    request.timeout_secs = 0;
    let started = supervisor.start(request).await.expect("start answers");
    assert_eq!(started.state, RunState::Finished);
    assert_eq!(started.outcome, Some(Outcome::Refused));
    assert_eq!(started.worker, None);
    let error = started.error.expect("a refusal says why");
    assert_eq!(error.kind, "invalid_request");
    assert!(error.message.contains("prompt") && error.message.contains("timeout_secs"));

    let result = result_of(&supervisor, &started.run).await;
    assert_eq!(result.outcome, Outcome::Refused);
    assert_eq!(result.worker, None);
    assert_eq!(result.timing.started_at, None);
    assert_eq!(result.timing.duration_ms, 0);
    assert!(supervisor.list().await.expect("list").workers.is_empty());

    let compact = read_compact(&supervisor, &started.run).await;
    assert_eq!(compact.state, Some(RunState::Finished));
    assert!(compact.progress.is_none());
    supervisor.shutdown().await;
}

#[tokio::test]
async fn a_start_the_provider_layer_refuses_is_refused_at_once_with_its_reason() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let mut request = rig.request(Provider::Claude, "success");
    request.cwd = Some(rig.work.join("missing"));
    let started = supervisor.start(request).await.expect("start answers");
    assert_eq!(started.state, RunState::Finished);
    assert_eq!(started.worker, None);
    assert_eq!(started.error.expect("reason").kind, "cwd_unusable");
    assert!(supervisor.list().await.expect("list").workers.is_empty());
}

#[tokio::test]
async fn a_live_worker_name_cannot_be_used_twice_until_it_is_closed() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let gate = rig.gate("name");
    let mut first = rig.request(Provider::Claude, &gate.prompt());
    first.name = Some("scribe".to_string());
    let first = supervisor.start(first).await.expect("start");
    gate.started().await;

    let mut second = rig.request(Provider::Claude, "success");
    second.name = Some("scribe".to_string());
    let refused = supervisor
        .start(second.clone())
        .await
        .expect("start answers");
    assert_eq!(refused.error.expect("reason").kind, "name_in_use");
    assert_eq!(refused.worker, None);

    supervisor
        .close(first.worker.as_ref().expect("worker"))
        .await
        .expect("close");
    let again = supervisor.start(second).await.expect("start");
    assert_eq!(again.state, RunState::Running);
    result_of(&supervisor, &again.run).await;
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

async fn a_follow_up_continues_the_session_the_previous_run_reported(provider: Provider) {
    let rig = Rig::new();
    let supervisor = rig.supervisor(provider);
    let first = supervisor
        .start(rig.request(provider, "echo:one"))
        .await
        .expect("start");
    let worker = first.worker.clone().expect("worker");
    let one = result_of(&supervisor, &first.run).await;
    assert_eq!(one.session_id.as_deref(), Some(rig.session()));
    assert!(
        !rig.argv().iter().any(|a| a == "--resume" || a == "resume"),
        "the first run must not resume: {:?}",
        rig.argv()
    );

    let second = supervisor
        .follow_up(&worker, "echo:two", None)
        .await
        .expect("follow_up");
    assert_eq!(second.worker.as_deref(), Some(worker.as_str()));
    assert_ne!(second.run, first.run);
    let two = result_of(&supervisor, &second.run).await;
    assert_eq!(two.outcome, Outcome::Completed);
    assert_eq!(two.text, format!("answered echo:two sid={}", rig.session()));
    let argv = rig.argv();
    let at = argv
        .iter()
        .position(|a| a == "--resume" || a == "resume")
        .unwrap_or_else(|| panic!("the follow-up did not resume: {argv:?}"));
    assert_eq!(argv[at + 1], rig.session(), "{argv:?}");
    assert_eq!(two.worker, one.worker);

    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn a_follow_up_continues_the_session_the_previous_run_reported_for_each_provider() {
    for provider in BOTH {
        a_follow_up_continues_the_session_the_previous_run_reported(provider).await;
    }
}

#[tokio::test]
async fn a_follow_up_keeps_the_workers_permissions_and_settings() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let mut request = rig.request(Provider::Claude, "echo:one");
    request.permissions.mode = crate::contract::permissions::Mode::Write;
    request.timeout_secs = 321;
    let first = supervisor.start(request).await.expect("start");
    let one = result_of(&supervisor, &first.run).await;
    assert_eq!(
        one.permissions.mode,
        crate::contract::permissions::Mode::Write
    );

    let worker = first.worker.expect("worker");
    let second = supervisor
        .follow_up(&worker, "echo:two", None)
        .await
        .expect("follow_up");
    let two = result_of(&supervisor, &second.run).await;
    assert_eq!(
        two.permissions.mode,
        crate::contract::permissions::Mode::Write
    );
    assert_eq!(two.timing.timeout_secs, 321);

    let third = supervisor
        .follow_up(&worker, "echo:three", Some(45))
        .await
        .expect("follow_up");
    let three = result_of(&supervisor, &third.run).await;
    assert_eq!(three.timing.timeout_secs, 45);
    supervisor.shutdown().await;
}

#[tokio::test]
async fn a_follow_up_on_an_ephemeral_worker_is_refused_because_it_has_no_session() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let mut request = rig.request(Provider::Claude, "success");
    request.session.ephemeral = true;
    let first = supervisor.start(request).await.expect("start");
    let worker = first.worker.expect("worker");
    let one = result_of(&supervisor, &first.run).await;
    assert_eq!(one.session_id, None);

    let refused = supervisor
        .follow_up(&worker, "more", None)
        .await
        .expect("follow_up answers");
    assert_eq!(refused.state, RunState::Finished);
    assert_eq!(refused.outcome, Some(Outcome::Refused));
    assert_eq!(refused.error.expect("reason").kind, "no_session");
    let result = result_of(&supervisor, &refused.run).await;
    assert_eq!(result.worker.expect("worker ref").id, worker);
    assert_eq!(result.outcome, Outcome::Refused);
    supervisor.shutdown().await;
}

#[tokio::test]
async fn a_queued_follow_up_whose_worker_reported_no_session_is_refused_when_its_turn_comes() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    // The first run ends before the provider reports a session.
    let first = supervisor
        .start(rig.request(Provider::Claude, "malformed"))
        .await
        .expect("start");
    let worker = first.worker.expect("worker");
    let queued = supervisor
        .follow_up(&worker, "echo:later", None)
        .await
        .expect("follow_up");
    let one = result_of(&supervisor, &first.run).await;
    assert_eq!(one.outcome, Outcome::Failed);
    assert_eq!(one.session_id, None);

    let two = result_of(&supervisor, &queued.run).await;
    assert_eq!(two.outcome, Outcome::Refused);
    assert_eq!(two.error.expect("reason").kind, "no_session");
    assert_eq!(two.timing.started_at, None);
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn a_follow_up_on_a_worker_nobody_knows_is_an_error_not_a_run() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let error = supervisor
        .follow_up("worker-nope", "more", None)
        .await
        .expect_err("unknown worker");
    assert_eq!(error, Error::UnknownWorker("worker-nope".to_string()));
}

#[tokio::test]
async fn a_follow_up_with_an_invalid_prompt_is_refused_as_a_run() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let first = supervisor
        .start(rig.request(Provider::Claude, "success"))
        .await
        .expect("start");
    result_of(&supervisor, &first.run).await;
    let refused = supervisor
        .follow_up(first.worker.as_deref().expect("worker"), "  ", None)
        .await
        .expect("follow_up answers");
    assert_eq!(refused.error.expect("reason").kind, "invalid_request");
    supervisor.shutdown().await;
}

#[tokio::test]
async fn follow_ups_run_in_the_order_given_one_at_a_time() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let (g1, g2, g3) = (rig.gate("one"), rig.gate("two"), rig.gate("three"));
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
    let third = supervisor
        .follow_up(&worker, g3.prompt(), None)
        .await
        .expect("follow_up");
    assert_eq!(second.state, RunState::Queued);
    assert_eq!(second.ahead, Some(1));
    assert_eq!(third.ahead, Some(2));

    let list = supervisor.list().await.expect("list");
    assert_eq!(list.workers[0].brief.state, WorkerState::Running);
    assert_eq!(list.workers[0].brief.queue_length, 2);
    assert_eq!(
        list.workers[0].brief.current_run.as_deref(),
        Some(first.run.as_str())
    );

    g2.assert_not_started().await;
    g1.open();
    g2.started().await;
    g3.assert_not_started().await;
    let read = read_compact(&supervisor, &first.run).await;
    assert_eq!(read.outcome, Some(Outcome::Completed));
    g2.open();
    g3.started().await;
    g3.open();
    for run in [&first.run, &second.run, &third.run] {
        let result = result_of(&supervisor, run).await;
        assert_eq!(result.outcome, Outcome::Completed);
    }
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn timing_includes_the_time_a_run_waited_in_the_queue() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let gate = rig.gate("slow");
    let first = supervisor
        .start(rig.request(Provider::Claude, &gate.prompt()))
        .await
        .expect("start");
    gate.started().await;
    let second = supervisor
        .follow_up(first.worker.as_deref().expect("worker"), "echo:next", None)
        .await
        .expect("follow_up");
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let waiting = read_compact(&supervisor, &second.run).await;
    assert_eq!(waiting.state, Some(RunState::Queued));
    let progress = waiting.progress.expect("progress while queued");
    assert!(progress.queued_ms >= 1100, "{progress:?}");
    assert_eq!(progress.running_ms, None);

    gate.open();
    let result = result_of(&supervisor, &second.run).await;
    let started = result.timing.started_at.clone().expect("started");
    assert!(result.timing.queued_at < started, "{:?}", result.timing);
    supervisor.shutdown().await;
}

#[tokio::test]
async fn ids_are_unique_opaque_and_never_reused_after_close() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let mut ids = HashSet::new();
    for round in 0..4 {
        let started = supervisor
            .start(rig.request(Provider::Claude, &format!("echo:{round}")))
            .await
            .expect("start");
        let worker = started.worker.clone().expect("worker");
        assert!(ids.insert(worker.clone()), "worker id reused: {worker}");
        assert!(ids.insert(started.run.clone()), "run id reused");
        result_of(&supervisor, &started.run).await;
        let follow = supervisor
            .follow_up(&worker, "echo:more", None)
            .await
            .expect("follow_up");
        assert!(ids.insert(follow.run.clone()), "run id reused");
        result_of(&supervisor, &follow.run).await;
        supervisor.close(&worker).await.expect("close");
    }
    let refused = supervisor
        .start(rig.request(Provider::Claude, ""))
        .await
        .expect("start answers");
    assert!(ids.insert(refused.run), "a refused run reused an id");
    assert_eq!(ids.len(), 4 * 3 + 1);
    supervisor.shutdown().await;
}

#[tokio::test]
async fn start_follow_up_read_and_list_return_while_a_provider_hangs() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let gate = rig.gate("held");
    let held = supervisor
        .start(rig.request(Provider::Claude, &gate.prompt()))
        .await
        .expect("start");
    gate.started().await;
    let worker = held.worker.expect("worker");

    let begun = Instant::now();
    let second = supervisor
        .start(rig.request(Provider::Claude, "echo:other"))
        .await
        .expect("start");
    supervisor
        .follow_up(&worker, "echo:queued", None)
        .await
        .expect("follow_up");
    read_compact(&supervisor, &held.run).await;
    supervisor
        .read(Target::Worker(worker.clone()), Detail::Full, None)
        .await
        .expect("read");
    supervisor.list().await.expect("list");
    assert!(
        begun.elapsed() < Duration::from_millis(500),
        "an operation waited on a provider: {:?}",
        begun.elapsed()
    );
    finished(&supervisor, &[&second.run]).await;
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn every_result_is_one_of_the_five_outcomes_and_validates() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let mut outcomes = HashSet::new();
    for prompt in ["success", "provider-error", "denied-refused", "malformed"] {
        let started = supervisor
            .start(rig.request(Provider::Claude, prompt))
            .await
            .expect("start");
        outcomes.insert(result_of(&supervisor, &started.run).await.outcome);
    }
    let mut short = rig.request(Provider::Claude, "hang");
    short.timeout_secs = 1;
    let started = supervisor.start(short).await.expect("start");
    outcomes.insert(result_of(&supervisor, &started.run).await.outcome);
    let refused = supervisor
        .start(Request::new(Provider::Claude, ""))
        .await
        .expect("start");
    outcomes.insert(result_of(&supervisor, &refused.run).await.outcome);
    for expected in [
        Outcome::Completed,
        Outcome::Failed,
        Outcome::Refused,
        Outcome::TimedOut,
    ] {
        assert!(outcomes.contains(&expected), "{outcomes:?}");
    }
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn compact_and_full_finished_reads_differ_in_what_they_hold() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let started = supervisor
        .start(rig.request(Provider::Claude, "success"))
        .await
        .expect("start");
    result_of(&supervisor, &started.run).await;

    let compact = read_compact(&supervisor, &started.run).await;
    let Some(ResultView::Compact(_)) = &compact.result else {
        panic!("{compact:#?}");
    };
    let json = serde_json::to_value(&compact).expect("serialize");
    let result = json["result"].as_object().expect("result object");
    for key in ["run", "outcome", "text", "cleanup", "timing", "session_id"] {
        assert!(result.contains_key(key), "compact lacks {key}: {json}");
    }
    for key in ["usage", "provenance", "schema_version"] {
        assert!(!result.contains_key(key), "compact holds {key}: {json}");
    }
    assert!(json.get("progress").is_none(), "{json}");

    let full = read_full(&supervisor, &started.run).await;
    let json = serde_json::to_value(&full).expect("serialize");
    let result = json["result"].as_object().expect("result object");
    for key in ["usage", "provenance", "schema_version", "permissions"] {
        assert!(result.contains_key(key), "full lacks {key}: {json}");
    }
    let progress = &json["progress"];
    assert!(
        progress["activity"]
            .as_array()
            .is_some_and(|a| !a.is_empty()),
        "{json}"
    );
    assert!(progress["diagnostics"].is_object(), "{json}");
    supervisor.shutdown().await;
}
