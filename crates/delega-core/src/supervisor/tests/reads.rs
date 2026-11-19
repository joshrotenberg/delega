//! `read` while a run goes on: partial text, activity, compact against full,
//! and the bounds.

use std::sync::atomic::AtomicU64;

use super::{BOTH, Rig, read_compact, read_full, read_until_text, result_of};
use crate::contract::outcome::Outcome;
use crate::contract::request::Provider;
use crate::provider::ProgressEvent;
use crate::supervisor::activity::Activity;
use crate::supervisor::limits::{
    COMPACT_ACTIVITY, COMPACT_PARTIAL_TEXT_CHARS, MAX_ACTIVITY, MAX_PARTIAL_TEXT_CHARS,
};
use crate::supervisor::{Detail, RunState, Target, WorkerState};

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
        .read(Target::Run(started.run.clone()), Detail::Full, Some(last))
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
    let compact = read_compact(&supervisor, &started.run)
        .await
        .progress
        .expect("progress");
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
}

#[tokio::test]
async fn reading_an_unknown_run_or_worker_is_an_error_naming_the_id() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let error = supervisor
        .read(Target::Run("run-x".into()), Detail::Compact, None)
        .await
        .expect_err("unknown");
    assert!(error.to_string().contains("run-x"));
    let error = supervisor
        .read(Target::Worker("worker-x".into()), Detail::Compact, None)
        .await
        .expect_err("unknown");
    assert!(error.to_string().contains("worker-x"));
}

#[test]
fn partial_text_is_bounded_and_keeps_its_end() {
    let activity = Activity::default();
    let cursor = AtomicU64::new(0);
    let block = "x".repeat(MAX_PARTIAL_TEXT_CHARS / 2);
    for _ in 0..3 {
        activity.record(
            ProgressEvent::Message {
                text: block.clone(),
            },
            &cursor,
        );
    }
    activity.record(
        ProgressEvent::Message {
            text: "the end".to_string(),
        },
        &cursor,
    );
    let snapshot = activity.snapshot();
    assert!(snapshot.text.chars().count() <= MAX_PARTIAL_TEXT_CHARS);
    assert!(snapshot.text.ends_with("the end"));
    assert!(snapshot.text_truncated);
    assert!(
        snapshot.entries.is_empty(),
        "a message is text, not an entry"
    );
}

#[test]
fn partial_text_cuts_on_a_character_boundary() {
    let activity = Activity::default();
    let cursor = AtomicU64::new(0);
    let text = "é".repeat(MAX_PARTIAL_TEXT_CHARS + 10);
    activity.record(ProgressEvent::Message { text }, &cursor);
    let snapshot = activity.snapshot();
    assert_eq!(snapshot.text.chars().count(), MAX_PARTIAL_TEXT_CHARS);
}

#[test]
fn the_activity_ring_drops_the_oldest_and_counts_them() {
    let activity = Activity::default();
    let cursor = AtomicU64::new(0);
    let total = MAX_ACTIVITY + COMPACT_ACTIVITY;
    for n in 0..total {
        activity.record(
            ProgressEvent::Provider {
                kind: "assistant".to_string(),
                summary: format!("n={n}"),
            },
            &cursor,
        );
    }
    let snapshot = activity.snapshot();
    assert_eq!(snapshot.entries.len(), MAX_ACTIVITY);
    assert_eq!(snapshot.total, total as u64);
    assert_eq!(snapshot.omitted, COMPACT_ACTIVITY as u64);
    assert_eq!(snapshot.entries[0].summary, format!("n={COMPACT_ACTIVITY}"));
    let cursors: Vec<u64> = snapshot.entries.iter().map(|e| e.cursor).collect();
    assert!(cursors.windows(2).all(|w| w[0] < w[1]), "cursors increase");
}
