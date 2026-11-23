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

async fn pending_id(supervisor: &delega_core::supervisor::Supervisor, run: &str) -> String {
    let waiting = supervisor
        .wait(
            WaitTargets {
                workers: vec![],
                runs: vec![run.to_string()],
            },
            Some(5),
            None,
        )
        .await
        .unwrap();
    assert!(!waiting.timed_out);
    waiting.runs[0]
        .pending_interactions
        .first()
        .unwrap_or_else(|| panic!("no request in {waiting:#?}"))
        .id
        .clone()
}

#[tokio::test]
async fn interrupt_denies_the_pending_request_and_keeps_the_worker_open() {
    use delega_core::contract::outcome::Outcome;
    use delega_core::supervisor::Target;

    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let mut request = rig.request(Provider::Claude, "ask");
    request.permissions.on_request = OnRequest::Ask;
    let start = supervisor.start(request).await.unwrap();
    let id = pending_id(&supervisor, &start.run).await;
    let interrupted = supervisor
        .interrupt(Target::Run(start.run.clone()))
        .await
        .unwrap();
    assert!(interrupted.settled);
    let result = result_of(&supervisor, &start.run).await;
    assert_eq!(result.outcome, Outcome::Interrupted);
    let asked = &result.permissions.asked[0];
    assert_eq!(asked.interaction.id, id);
    assert_eq!(asked.decision, PermissionDecision::Deny);
    assert_eq!(asked.decided_by, "interrupt");
    let list = supervisor.list().await.unwrap();
    assert_eq!(list.workers[0].brief.state, WorkerState::Idle);
    supervisor.shutdown().await;
}

#[tokio::test]
async fn the_run_deadline_denies_a_waiting_request_before_its_permission_timeout() {
    use delega_core::contract::outcome::Outcome;

    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let mut request = rig.request(Provider::Claude, "ask");
    request.permissions.on_request = OnRequest::Ask;
    request.timeout_secs = 4;
    let start = supervisor.start(request).await.unwrap();
    let id = pending_id(&supervisor, &start.run).await;
    let result = tokio::time::timeout(std::time::Duration::from_secs(12), async {
        supervisor
            .wait(
                WaitTargets {
                    workers: vec![],
                    runs: vec![start.run.clone()],
                },
                Some(5),
                Some(read_full(&supervisor, &start.run).await.cursor),
            )
            .await
            .unwrap();
        result_of(&supervisor, &start.run).await
    })
    .await
    .expect("run deadline settles");
    assert_eq!(result.outcome, Outcome::TimedOut);
    let asked = &result.permissions.asked[0];
    assert_eq!(asked.interaction.id, id);
    assert_eq!(asked.decision, PermissionDecision::Deny);
    assert_eq!(asked.decided_by, "run_timeout");
    supervisor.shutdown().await;
}

#[tokio::test]
async fn an_oversized_permission_is_denied_and_a_following_request_can_be_allowed() {
    use delega_core::contract::outcome::Outcome;

    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let mut request = rig.request(Provider::Claude, "ask-oversized");
    request.permissions.on_request = OnRequest::Ask;
    let start = supervisor.start(request).await.unwrap();
    let id = pending_id(&supervisor, &start.run).await;
    assert!(id.ends_with(":2"), "{id}");
    assert_eq!(
        supervisor
            .respond(id, PermissionDecision::Allow)
            .await
            .unwrap()
            .status,
        "accepted"
    );
    let result = result_of(&supervisor, &start.run).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    assert_eq!(result.permissions.asked.len(), 2);
    let first = &result.permissions.asked[0];
    assert_eq!(first.decided_by, "request_too_large");
    assert_eq!(first.decision, PermissionDecision::Deny);
    assert!(first.written);
    supervisor.shutdown().await;
}

#[tokio::test]
async fn a_stream_frame_over_the_hard_limit_fails_the_run_without_publishing_a_request() {
    use delega_core::contract::outcome::Outcome;

    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let mut request = rig.request(Provider::Claude, "ask-stream-too-large");
    request.permissions.on_request = OnRequest::Ask;
    request.timeout_secs = 5;
    let start = supervisor.start(request).await.unwrap();
    let first = supervisor
        .wait(
            WaitTargets {
                workers: vec![],
                runs: vec![start.run.clone()],
            },
            Some(8),
            None,
        )
        .await
        .unwrap();
    assert!(!first.timed_out);
    assert!(first.runs[0].pending_interactions.is_empty());
    assert_eq!(first.runs[0].outcome, Some(Outcome::Failed));

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(12),
        result_of(&supervisor, &start.run),
    )
    .await
    .expect("oversized stream fails within the run bound");
    assert_eq!(result.outcome, Outcome::Failed);
    assert_eq!(result.error.unwrap().kind, "wait_failed");
    assert!(result.permissions.asked.is_empty());
    supervisor.shutdown().await;
}

#[tokio::test]
async fn malformed_stdout_is_counted_while_an_ask_run_completes() {
    use delega_core::contract::outcome::Outcome;

    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let mut request = rig.request(Provider::Claude, "ask-invalid-json");
    request.permissions.on_request = OnRequest::Ask;
    let start = supervisor.start(request).await.unwrap();
    let id = pending_id(&supervisor, &start.run).await;
    assert_eq!(
        supervisor
            .respond(id, PermissionDecision::Allow)
            .await
            .unwrap()
            .status,
        "accepted"
    );
    let result = result_of(&supervisor, &start.run).await;
    assert_eq!(result.outcome, Outcome::Completed);
    assert!(
        result
            .provider_notes
            .iter()
            .any(|note| note.contains("1 provider output lines were not JSON objects"))
    );
    supervisor.shutdown().await;
}

#[tokio::test]
async fn a_provider_that_never_reads_a_large_reply_still_stops_at_the_run_deadline() {
    use delega_core::contract::outcome::Outcome;

    let rig = Rig::new();
    let supervisor = rig.supervisor(Provider::Claude);
    let mut request = rig.request(Provider::Claude, "ask-no-read");
    request.permissions.on_request = OnRequest::Ask;
    request.timeout_secs = 5;
    let start = supervisor.start(request).await.unwrap();
    let id = pending_id(&supervisor, &start.run).await;
    assert_eq!(
        supervisor
            .respond(id, PermissionDecision::Allow)
            .await
            .unwrap()
            .status,
        "accepted"
    );
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(12),
        result_of(&supervisor, &start.run),
    )
    .await
    .expect("blocked stdin cannot stall deadlines or cleanup");
    assert_eq!(result.outcome, Outcome::TimedOut);
    let asked = &result.permissions.asked[0];
    assert_eq!(asked.decision, PermissionDecision::Allow);
    assert!(!asked.written);
    supervisor.shutdown().await;
}
