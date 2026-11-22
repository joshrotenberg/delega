//! The supervisor passes a run's usage through as the provider layer reported
//! it. It does not work out a follow-up's own usage from session totals: it
//! cannot know what else used the provider session between two runs.

use super::{Rig, result_of};
use delega_core::contract::outcome::Outcome;
use delega_core::contract::request::Provider;
use delega_core::contract::result::{RunResult, Usage};
use delega_core::supervisor::Supervisor;

async fn start_and_follow_up(rig: &Rig, provider: Provider) -> (RunResult, RunResult) {
    let supervisor = rig.supervisor(provider);
    let first = supervisor
        .start(rig.request(provider, "echo:one"))
        .await
        .expect("start");
    let worker = first.worker.clone().expect("worker");
    let one = result_of(&supervisor, &first.run).await;
    let run = follow_up_run(&supervisor, &worker).await;
    let two = result_of(&supervisor, &run).await;
    assert_eq!(two.outcome, Outcome::Completed, "{two:#?}");
    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
    (one, two)
}

async fn follow_up_run(supervisor: &Supervisor, worker: &str) -> String {
    supervisor
        .follow_up(worker, "echo:two", None)
        .await
        .expect("follow_up")
        .run
}

#[tokio::test]
async fn a_codex_follow_up_reports_thread_totals_and_no_run_usage() {
    let rig = Rig::new();
    let (one, two) = start_and_follow_up(&rig, Provider::Codex).await;
    assert!(one.usage.is_some());
    // The fake's thread totals after two turns of 100 in, 10 cached, 7 out and
    // 3 reasoning each.
    assert_eq!(
        two.session_usage,
        Some(Usage {
            input_tokens: Some(200),
            output_tokens: Some(14),
            cached_tokens: Some(20),
            reasoning_tokens: Some(6),
            cost_usd: None,
        })
    );
    assert_eq!(two.usage, None);
}

#[tokio::test]
async fn a_claude_follow_up_keeps_its_own_run_usage() {
    let rig = Rig::new();
    let (_, two) = start_and_follow_up(&rig, Provider::Claude).await;
    let usage = two.usage.expect("the run's own usage");
    assert_eq!(
        (
            usage.input_tokens,
            usage.output_tokens,
            usage.cached_tokens,
            usage.cost_usd
        ),
        (Some(12), Some(7), Some(4), Some(0.001))
    );
    assert_eq!(two.session_usage.and_then(|u| u.cost_usd), Some(0.002));
}
