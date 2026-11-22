//! A run's own usage and the session's totals, against the fakes.
//!
//! The fakes report usage the way the real CLIs were seen to (see
//! `docs/providers.md`, Verified against the real CLI): Claude Code gives the
//! run's own tokens and per-model cost next to session totals, and Codex gives
//! only the thread's totals. A resumed fake run is the session's second turn.

use super::Rig;
use crate::contract::outcome::Outcome;
use crate::contract::request::Provider;
use crate::contract::result::{RunResult, Usage};
use crate::provider::ProviderOptions;

fn usage(input: u64, output: u64, cached: u64, reasoning: Option<u64>, cost: Option<f64>) -> Usage {
    Usage {
        input_tokens: Some(input),
        output_tokens: Some(output),
        cached_tokens: Some(cached),
        reasoning_tokens: reasoning,
        cost_usd: cost,
    }
}

async fn run(rig: &Rig, provider: Provider, resume: bool, options: ProviderOptions) -> RunResult {
    let mut request = rig.request(provider, "success");
    if resume {
        request.session.resume = Some("saved-session-9".into());
    }
    let result = super::run_checked(request, options).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    rig.assert_nothing_left().await;
    result
}

/// Claude options whose fake writes `model_usage_since_last_result` in the
/// given shape (`none`, `empty` or `partial`, see the fake).
fn run_usage_shape(rig: &Rig, shape: &str) -> ProviderOptions {
    let mut options = rig.options(Provider::Claude);
    options
        .env_set
        .push(("FAKE_RUN_USAGE".into(), shape.into()));
    options
}

#[tokio::test]
async fn claude_start_reports_the_run_as_its_own_session() {
    let rig = Rig::new();
    let result = run(&rig, Provider::Claude, false, rig.options(Provider::Claude)).await;
    let expected = usage(12, 7, 4, None, Some(0.001));
    assert_eq!(result.usage, Some(expected.clone()));
    assert_eq!(result.session_usage, Some(expected));
}

#[tokio::test]
async fn claude_resume_reports_its_own_cost_not_the_session_total() {
    let rig = Rig::new();
    let result = run(&rig, Provider::Claude, true, rig.options(Provider::Claude)).await;
    assert_eq!(result.usage, Some(usage(12, 7, 4, None, Some(0.001))));
    assert_eq!(
        result.session_usage,
        Some(usage(24, 14, 8, None, Some(0.002)))
    );
}

#[tokio::test]
async fn claude_resume_without_per_run_figures_leaves_the_run_cost_out() {
    let rig = Rig::new();
    let result = run(&rig, Provider::Claude, true, run_usage_shape(&rig, "none")).await;
    assert_eq!(result.usage, Some(usage(12, 7, 4, None, None)));
    assert_eq!(result.session_usage.and_then(|u| u.cost_usd), Some(0.002));
}

#[tokio::test]
async fn claude_start_without_per_run_figures_takes_the_session_cost() {
    let rig = Rig::new();
    let result = run(&rig, Provider::Claude, false, run_usage_shape(&rig, "none")).await;
    assert_eq!(result.usage, Some(usage(12, 7, 4, None, Some(0.001))));
    assert_eq!(result.session_usage.and_then(|u| u.cost_usd), Some(0.001));
}

/// Per-run figures delega cannot use whole: an empty object, or an entry
/// without a cost. A partial sum would understate the run's cost.
async fn unusable_run_figures(shape: &str) {
    let rig = Rig::new();
    let fresh = run(&rig, Provider::Claude, false, run_usage_shape(&rig, shape)).await;
    assert_eq!(
        fresh.usage.and_then(|u| u.cost_usd),
        Some(0.001),
        "{shape}: a start takes the session's total cost"
    );
    let resumed = run(&rig, Provider::Claude, true, run_usage_shape(&rig, shape)).await;
    let usage = resumed.usage.expect("the run's own tokens");
    assert_eq!(usage.output_tokens, Some(7), "{shape}");
    assert_eq!(usage.cost_usd, None, "{shape}: a resumed run has no cost");
    assert_eq!(
        resumed.session_usage.and_then(|u| u.cost_usd),
        Some(0.002),
        "{shape}"
    );
}

#[tokio::test]
async fn claude_empty_per_run_figures_give_no_run_cost_on_resume_and_the_total_on_start() {
    unusable_run_figures("empty").await;
}

#[tokio::test]
async fn claude_per_run_figures_with_an_entry_missing_its_cost_are_not_summed() {
    unusable_run_figures("partial").await;
}

#[tokio::test]
async fn codex_start_reports_the_thread_totals_as_the_run_usage() {
    let rig = Rig::new();
    let result = run(&rig, Provider::Codex, false, rig.options(Provider::Codex)).await;
    let expected = usage(100, 7, 10, Some(3), None);
    assert_eq!(result.usage, Some(expected.clone()));
    assert_eq!(result.session_usage, Some(expected));
}

#[tokio::test]
async fn codex_resume_does_not_report_thread_totals_as_the_run_usage() {
    let rig = Rig::new();
    let result = run(&rig, Provider::Codex, true, rig.options(Provider::Codex)).await;
    assert_eq!(result.usage, None);
    assert_eq!(
        result.session_usage,
        Some(usage(200, 14, 20, Some(6), None))
    );
}
