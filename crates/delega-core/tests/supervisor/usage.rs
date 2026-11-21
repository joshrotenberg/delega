//! A follow-up's own usage, worked out from the session totals a worker's runs
//! report. Codex reports only the thread's totals on a resumed turn; the fake
//! does the same, 100 in, 10 cached, 7 out and 3 reasoning per turn, counting
//! each thread's turns in `FAKE_USAGE_DIR`.

use super::{Rig, result_of};
use delega_core::contract::outcome::Outcome;
use delega_core::contract::request::Provider;
use delega_core::contract::result::{RunResult, Usage};
use delega_core::supervisor::Supervisor;

/// The thread's totals after `turns` turns, or that many turns' own usage.
fn turns(turns: u64) -> Usage {
    Usage {
        input_tokens: Some(100 * turns),
        output_tokens: Some(7 * turns),
        cached_tokens: Some(10 * turns),
        reasoning_tokens: Some(3 * turns),
        cost_usd: None,
    }
}

fn codex_supervisor(rig: &Rig) -> Supervisor {
    let mut config = rig.config(Provider::Codex);
    config.provider.env_set.push((
        "FAKE_USAGE_DIR".into(),
        rig.root().to_string_lossy().into_owned(),
    ));
    Supervisor::new(config)
}

async fn follow_up(supervisor: &Supervisor, worker: &str, prompt: &str) -> RunResult {
    let run = supervisor
        .follow_up(worker, prompt, None)
        .await
        .expect("follow_up")
        .run;
    result_of(supervisor, &run).await
}

#[tokio::test]
async fn codex_follow_ups_report_the_difference_of_the_thread_totals() {
    let rig = Rig::new();
    let supervisor = codex_supervisor(&rig);
    let first = supervisor
        .start(rig.request(Provider::Codex, "echo:one"))
        .await
        .expect("start");
    let worker = first.worker.clone().expect("worker");
    let one = result_of(&supervisor, &first.run).await;
    assert_eq!(one.usage, Some(turns(1)));
    assert_eq!(one.session_usage, Some(turns(1)));

    let two = follow_up(&supervisor, &worker, "echo:two").await;
    assert_eq!(two.outcome, Outcome::Completed, "{two:#?}");
    assert_eq!(two.session_usage, Some(turns(2)));
    assert_eq!(two.usage, Some(turns(1)));

    let three = follow_up(&supervisor, &worker, "echo:three").await;
    assert_eq!(three.session_usage, Some(turns(3)));
    assert_eq!(three.usage, Some(turns(1)));

    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn codex_totals_that_went_down_give_a_follow_up_no_usage() {
    let rig = Rig::new();
    let supervisor = codex_supervisor(&rig);
    let first = supervisor
        .start(rig.request(Provider::Codex, "echo:one"))
        .await
        .expect("start");
    let worker = first.worker.clone().expect("worker");
    result_of(&supervisor, &first.run).await;
    let two = follow_up(&supervisor, &worker, "echo:two").await;
    assert_eq!(two.session_usage, Some(turns(2)));

    // The next turn reports the totals of turn 1, below the previous ones.
    std::fs::write(rig.root().join(format!("turns-{}", rig.session())), "0\n")
        .expect("reset the turn counter");
    let three = follow_up(&supervisor, &worker, "echo:three").await;
    assert_eq!(three.session_usage, Some(turns(1)));
    assert_eq!(three.usage, None);

    // The lower totals are the new baseline.
    let four = follow_up(&supervisor, &worker, "echo:four").await;
    assert_eq!(four.usage, Some(turns(1)));

    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn codex_follow_up_after_a_failed_run_without_totals_covers_both_turns() {
    let rig = Rig::new();
    let supervisor = codex_supervisor(&rig);
    let first = supervisor
        .start(rig.request(Provider::Codex, "echo:one"))
        .await
        .expect("start");
    let worker = first.worker.clone().expect("worker");
    result_of(&supervisor, &first.run).await;

    let failed = follow_up(&supervisor, &worker, "provider-error").await;
    assert_eq!(failed.outcome, Outcome::Failed, "{failed:#?}");
    assert_eq!(failed.usage, None);
    assert_eq!(failed.session_usage, None);

    let three = follow_up(&supervisor, &worker, "echo:three").await;
    assert_eq!(three.session_usage, Some(turns(3)));
    assert_eq!(three.usage, Some(turns(2)));

    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn claude_follow_up_keeps_its_own_usage_instead_of_a_difference() {
    let rig = Rig::new();
    let mut config = rig.config(Provider::Claude);
    config
        .provider
        .env_set
        .push(("FAKE_NO_RUN_USAGE".into(), "1".into()));
    let supervisor = Supervisor::new(config);
    let first = supervisor
        .start(rig.request(Provider::Claude, "echo:one"))
        .await
        .expect("start");
    let worker = first.worker.clone().expect("worker");
    result_of(&supervisor, &first.run).await;

    // Without per-run figures the resumed run has its own tokens and no cost;
    // its tokens are not replaced and no cost is worked out.
    let two = follow_up(&supervisor, &worker, "echo:two").await;
    let usage = two.usage.expect("the run's own tokens");
    assert_eq!(
        (usage.input_tokens, usage.output_tokens, usage.cached_tokens),
        (Some(12), Some(7), Some(4))
    );
    assert_eq!(usage.cost_usd, None);
    assert_eq!(two.session_usage.and_then(|u| u.cost_usd), Some(0.002));

    supervisor.shutdown().await;
    rig.assert_nothing_left().await;
}
