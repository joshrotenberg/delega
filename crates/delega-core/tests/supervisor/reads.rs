//! `read` while a run goes on: partial text, activity, compact against full,
//! and the bounds.

use super::{BOTH, Rig, read_compact, read_full, read_until_text, result_of};
use delega_core::contract::outcome::Outcome;
use delega_core::contract::request::Provider;
use delega_core::supervisor::limits::{COMPACT_ACTIVITY, COMPACT_PARTIAL_TEXT_CHARS, MAX_ACTIVITY};
use delega_core::supervisor::{Detail, RunState, Target, WorkerState};

async fn a_running_run_shows_partial_text_and_activity(provider: Provider) {
    let rig = Rig::new();
    let supervisor = rig.supervisor(provider);
    let gate = rig.gate("partial");
    let started = supervisor
        .start(rig.request(provider, &gate.prompt()))
        .await
        .expect("start");
    gate.started().await;

    let read = read_until_text(&supervisor, &started.run).await;
    assert_eq!(read.state, Some(RunState::Running));
    assert!(read.result.is_none(), "an unfinished run has no result");
    assert_eq!(read.outcome, None);
    let progress = read.progress.expect("progress");
    assert_eq!(progress.partial_text, "working");
    assert!(!progress.partial_text_truncated);
    assert_eq!(progress.timeout_secs, 600);
    assert!(progress.running_ms.is_some());
    assert!(progress.diagnostics.is_none(), "diagnostics are full only");
    let kinds: Vec<&str> = progress.activity.iter().map(|a| a.kind.as_str()).collect();
    assert_eq!(kinds.first(), Some(&"started"), "{kinds:?}");
    assert!(
        progress
            .activity
            .iter()
            .any(|a| a.summary == "message: 7 chars"),
        "a message is shown by length: {:#?}",
        progress.activity
    );
    assert!(
        progress
            .activity
            .iter()
            .all(|a| !a.summary.contains("working")),
        "the activity never carries message text"
    );
    assert_eq!(progress.activity_total as usize, progress.activity.len());

    // A worker read shows its current run.
    let by_worker = supervisor
        .read(
            Target::Worker(started.worker.clone().expect("worker")),
            Detail::Compact,
            None,
            None,
        )
        .await
        .expect("read");
    assert_eq!(by_worker.run.as_deref(), Some(started.run.as_str()));
    assert_eq!(by_worker.worker_state, Some(WorkerState::Running));

    gate.open();
    result_of(&supervisor, &started.run).await;
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn a_running_run_shows_partial_text_and_activity_for_each_provider() {
    for provider in BOTH {
        a_running_run_shows_partial_text_and_activity(provider).await;
    }
}

#[tokio::test]
async fn a_cursor_returns_only_activity_after_it() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let gate = rig.gate("cursor");
    let started = supervisor
        .start(rig.request(Provider::Claude, &gate.prompt()))
        .await
        .expect("start");
    gate.started().await;
    let read = read_until_text(&supervisor, &started.run).await;
    let progress = read.progress.expect("progress");
    let last = progress.activity.last().expect("some activity").cursor;

    let again = supervisor
        .read(
            Target::Run(started.run.clone()),
            Detail::Compact,
            Some(last),
            None,
        )
        .await
        .expect("read");
    let progress = again.progress.expect("progress");
    assert!(progress.activity.is_empty(), "{progress:#?}");
    assert!(
        progress.activity_omitted > 0,
        "the earlier entries are counted"
    );

    gate.open();
    result_of(&supervisor, &started.run).await;
    let full = supervisor
        .read(
            Target::Run(started.run.clone()),
            Detail::Full,
            Some(last),
            None,
        )
        .await
        .expect("read");
    let kinds: Vec<String> = full
        .progress
        .expect("full keeps the history of a finished run")
        .activity
        .into_iter()
        .map(|a| a.kind)
        .collect();
    assert!(kinds.contains(&"finished".to_string()), "{kinds:?}");
    assert!(!kinds.contains(&"started".to_string()), "{kinds:?}");
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn full_detail_adds_diagnostics_and_the_whole_history_to_an_unfinished_read() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let gate = rig.gate("full");
    let started = supervisor
        .start(rig.request(Provider::Claude, &gate.prompt()))
        .await
        .expect("start");
    gate.started().await;
    read_until_text(&supervisor, &started.run).await;
    let compact = read_compact(&supervisor, &started.run)
        .await
        .progress
        .expect("progress");
    let full = read_full(&supervisor, &started.run)
        .await
        .progress
        .expect("progress");
    assert!(compact.diagnostics.is_none());
    let diagnostics = full.diagnostics.expect("full adds diagnostics");
    assert_eq!(diagnostics.provider, Provider::Claude);
    assert!(diagnostics.started_at.is_some());
    assert_eq!(diagnostics.stop_requested, None);
    assert!(full.activity.len() >= compact.activity.len());
    gate.open();
    result_of(&supervisor, &started.run).await;
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn a_chatty_run_keeps_only_a_bounded_history() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let started = supervisor
        .start(rig.request(Provider::Claude, "chatty"))
        .await
        .expect("start");
    let result = result_of(&supervisor, &started.run).await;
    assert_eq!(result.outcome, Outcome::Completed);

    let full = read_full(&supervisor, &started.run)
        .await
        .progress
        .expect("progress");
    assert!(
        full.activity.len() <= MAX_ACTIVITY,
        "{}",
        full.activity.len()
    );
    assert!(
        full.activity_total > MAX_ACTIVITY as u64,
        "{}",
        full.activity_total
    );
    assert_eq!(
        full.activity_omitted,
        full.activity_total - full.activity.len() as u64
    );
    assert!(full.diagnostics.expect("diagnostics").activity_dropped > 0);
    assert_eq!(
        full.activity.last().map(|a| a.kind.as_str()),
        Some("finished")
    );
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn a_compact_read_of_a_chatty_unfinished_run_shows_only_the_last_few_entries() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let started = supervisor
        .start(rig.request(Provider::Claude, "chatty-hang"))
        .await
        .expect("start");
    for _ in 0..3000 {
        if rig.record.join("started").exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        rig.record.join("started").exists(),
        "the fake never finished its chatter"
    );
    // The marker says the fake has written its chatter, not that the supervisor
    // has consumed it. Poll the read until it shows the whole chatter.
    let mut compact = None;
    for _ in 0..6000 {
        let progress = read_compact(&supervisor, &started.run)
            .await
            .progress
            .expect("progress");
        if progress.partial_text_truncated && progress.activity.len() == COMPACT_ACTIVITY {
            compact = Some(progress);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let compact = compact.expect("the supervisor never showed the chatty run's output");
    assert_eq!(compact.activity.len(), COMPACT_ACTIVITY);
    assert!(compact.activity_omitted > 0);
    assert!(compact.partial_text.chars().count() <= COMPACT_PARTIAL_TEXT_CHARS);
    assert!(compact.partial_text_truncated);
    let full = read_full(&supervisor, &started.run)
        .await
        .progress
        .expect("progress");
    assert!(full.activity.len() > compact.activity.len());
    assert!(full.partial_text.chars().count() > compact.partial_text.chars().count());
    supervisor
        .interrupt(Target::Run(started.run.clone()))
        .await
        .expect("interrupt");
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn a_queued_run_reads_as_queued_without_activity() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let gate = rig.gate("front");
    let first = supervisor
        .start(rig.request(Provider::Claude, &gate.prompt()))
        .await
        .expect("start");
    gate.started().await;
    let queued = supervisor
        .follow_up(first.worker.as_deref().expect("worker"), "echo:next", None)
        .await
        .expect("follow_up");
    let read = read_compact(&supervisor, &queued.run).await;
    assert_eq!(read.state, Some(RunState::Queued));
    let progress = read.progress.expect("progress");
    assert!(progress.activity.is_empty() && progress.partial_text.is_empty());
    assert_eq!(progress.running_ms, None);
    gate.open();
    result_of(&supervisor, &queued.run).await;
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn reading_an_unknown_run_or_worker_is_an_error_naming_the_id() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let error = supervisor
        .read(Target::Run("run-x".into()), Detail::Compact, None, None)
        .await
        .expect_err("unknown");
    assert!(error.to_string().contains("run-x"));
    let error = supervisor
        .read(
            Target::Worker("worker-x".into()),
            Detail::Compact,
            None,
            None,
        )
        .await
        .expect_err("unknown");
    assert!(error.to_string().contains("worker-x"));
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}
