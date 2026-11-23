//! Live permission decisions across the real supervisor and fake Claude Code.

use super::{Rig, read_full, result_of};
use delega_core::contract::permissions::OnRequest;
use delega_core::contract::request::Provider;
use delega_core::contract::result::PermissionDecision;
use delega_core::supervisor::{WaitTargets, WorkerState};

#[tokio::test]
async fn an_ask_waits_for_one_answer_and_the_provider_exits_on_eof() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let mut request = rig.request(Provider::Claude, "ask");
    request.permissions.on_request = OnRequest::Ask;
    let start = supervisor.start(request).await.unwrap();
    let targets = WaitTargets {
        workers: vec![start.worker.unwrap()],
        runs: vec![start.run.clone()],
    };
    let pending = supervisor
        .wait(targets.clone(), Some(5), None)
        .await
        .unwrap();
    assert!(!pending.timed_out);
    assert_eq!(pending.workers[0].state, WorkerState::WaitingForAnswer);
    let interaction = &pending.runs[0].pending_interactions[0];
    assert_eq!(interaction.summary["file_path"], "example.txt");
    assert_eq!(interaction.summary["content_bytes"], 23);
    assert!(
        !interaction
            .summary
            .to_string()
            .contains("fixture-private-content")
    );
    let id = interaction.id.clone();
    let ack = supervisor
        .respond(id.clone(), PermissionDecision::Allow)
        .await
        .unwrap();
    assert_eq!(ack.status, "accepted");
    let ack = supervisor
        .respond(id, PermissionDecision::Deny)
        .await
        .unwrap();
    assert_eq!(ack.status, "already_answered");
    let mut cursor = pending.cursor;
    loop {
        let next = supervisor
            .wait(targets.clone(), Some(5), Some(cursor))
            .await
            .unwrap();
        assert!(!next.timed_out);
        if next.runs[0].outcome.is_some() {
            break;
        }
        cursor = next.cursor;
    }
    let read = read_full(&supervisor, &start.run).await;
    let result = result_of(&supervisor, &start.run).await;
    assert_eq!(result.text, "allowed", "{result:#?}");
    assert_eq!(result.permissions.asked.len(), 1);
    assert!(result.permissions.asked[0].written);
    assert!(read.pending_interactions.is_empty());
    supervisor.shutdown().await;
}

#[tokio::test]
async fn an_expired_ask_is_denied_and_cannot_be_answered_later() {
    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let mut request = rig.request(Provider::Claude, "ask");
    request.permissions.on_request = OnRequest::Ask;
    request.permissions.ask_timeout = Some(1);
    let start = supervisor.start(request).await.unwrap();
    let targets = WaitTargets {
        workers: vec![],
        runs: vec![start.run.clone()],
    };
    let pending = supervisor
        .wait(targets.clone(), Some(5), None)
        .await
        .unwrap();
    let id = pending.runs[0].pending_interactions[0].id.clone();
    let mut cursor = pending.cursor;
    loop {
        let next = supervisor
            .wait(targets.clone(), Some(5), Some(cursor))
            .await
            .unwrap();
        assert!(!next.timed_out);
        if next.runs[0].outcome.is_some() {
            break;
        }
        cursor = next.cursor;
    }
    let ack = supervisor
        .respond(id, PermissionDecision::Allow)
        .await
        .unwrap();
    assert_eq!(ack.status, "timed_out");
    let result = result_of(&supervisor, &start.run).await;
    assert_eq!(result.text, "denied", "{result:#?}");
    assert_eq!(result.permissions.asked[0].decided_by, "timeout");
    assert!(result.permissions.asked[0].written);
    supervisor.shutdown().await;
}

#[tokio::test]
async fn transport_shutdown_denies_pending_permissions_and_stops_the_run() {
    use delega_core::contract::outcome::Outcome;

    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let mut request = rig.request(Provider::Claude, "ask");
    request.permissions.on_request = OnRequest::Ask;
    let start = supervisor.start(request).await.unwrap();
    let targets = WaitTargets {
        workers: vec![],
        runs: vec![start.run.clone()],
    };
    let pending = supervisor.wait(targets, Some(5), None).await.unwrap();
    let id = pending.runs[0].pending_interactions[0].id.clone();
    supervisor.connection_closed().await;
    let result = result_of(&supervisor, &start.run).await;
    assert_eq!(result.outcome, Outcome::Interrupted);
    assert_eq!(
        result.permissions.asked[0].decision,
        PermissionDecision::Deny
    );
    assert_eq!(result.permissions.asked[0].decided_by, "connection_closed");
    let ack = supervisor
        .respond(id, PermissionDecision::Allow)
        .await
        .unwrap();
    assert_eq!(ack.status, "closed");
}
