//! The `applied` object of a result and of a dry run, against the fake
//! providers: what each outcome carries, what a refusal knows, and that no
//! environment value or rule text ever reaches it.

use std::ffi::OsString;

use super::{Rig, run_checked};
use crate::contract::applied::{Applied, AppliedConfig, Layer};
use crate::contract::outcome::Outcome;
use crate::contract::request::Provider;
use crate::provider::{dry_run, start};

const ENV_VALUE: &str = "planted-env-value-8841";
const RULE_TEXT: &str = "planted-rule-text-5520";

fn origin() -> Applied {
    Applied {
        config: Some(AppliedConfig {
            path: "/home/u/.config/delega/config.toml".to_string(),
            sha256: "ab".repeat(32),
        }),
        sources: [
            ("provider".to_string(), Layer::Flag),
            ("timeout_secs".to_string(), Layer::Default),
        ]
        .into(),
        ..Applied::default()
    }
}

/// Options with a planted environment (a value to pass by name, one to set, one
/// that is withheld) and a settings file with a planted rule.
fn planted(rig: &Rig, provider: Provider) -> crate::provider::ProviderOptions {
    let mut options = rig.options(provider);
    options.origin = origin();
    let parent = options.parent_env.as_mut().expect("parent env");
    parent.push((
        OsString::from("PLANTED_PASS"),
        OsString::from(format!("{ENV_VALUE}-pass")),
    ));
    parent.push((
        OsString::from("PLANTED_WITHHELD"),
        OsString::from(format!("{ENV_VALUE}-withheld")),
    ));
    parent.push((
        OsString::from("DELEGA_PLANTED"),
        OsString::from(format!("{ENV_VALUE}-delega")),
    ));
    options.env_pass.push("PLANTED_PASS".to_string());
    options
        .env_set
        .push(("PLANTED_SET".to_string(), format!("{ENV_VALUE}-set")));
    let settings = rig.config.join("settings.json");
    std::fs::write(
        &settings,
        format!(r#"{{"permissions": {{"deny": ["Bash({RULE_TEXT})", "Read(/{RULE_TEXT}/**)"]}}}}"#),
    )
    .expect("write settings");
    options
}

#[tokio::test]
async fn a_completed_claude_run_reports_every_part() {
    let rig = Rig::new();
    let options = planted(&rig, Provider::Claude);
    let dry = dry_run(&rig.request(Provider::Claude, "p"), &options).expect("dry run");
    let result = run_checked(rig.request(Provider::Claude, "success"), options).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    let applied = result.applied.expect("applied");
    assert_eq!(applied.config, origin().config);
    assert_eq!(applied.sources, origin().sources);

    let env = applied.env.expect("env");
    assert_eq!(env.passed.total, dry.env_names.len());
    assert_eq!(env.passed.named_by_caller, 1, "PLANTED_PASS");
    // The rig's own variables, the planted one and the one delega sets.
    assert_eq!(env.passed.set_by_caller, 5);
    assert_eq!(env.passed.set_by_delega, 1);
    assert_eq!(env.passed.base, 2, "PATH and HOME");
    assert_eq!(
        env.passed.total,
        env.passed.base
            + env.passed.network
            + env.passed.provider
            + env.passed.named_by_caller
            + env.passed.set_by_caller
            + env.passed.set_by_delega
    );
    assert_eq!(env.withheld.total, 2, "PLANTED_WITHHELD and DELEGA_PLANTED");
    assert_eq!(env.withheld.never_passed, 1, "DELEGA_PLANTED");
    assert_eq!(env.withheld.not_allowlisted, 1, "PLANTED_WITHHELD");
    assert_eq!(
        env.named,
        [
            "CLAUDE_CONFIG_DIR",
            "CODEX_HOME",
            "FAKE_RECORD_DIR",
            "PLANTED_PASS",
            "PLANTED_SET",
            "XDG_CONFIG_HOME"
        ]
    );

    let deny = applied.deny_rules.expect("deny rules");
    assert_eq!(deny.files.len(), 1);
    assert!(deny.files[0].path.ends_with("settings.json"));
    assert_eq!(deny.files[0].rules, 2);
    assert!(deny.skipped.is_empty());
    assert_eq!(deny.applied, dry.deny_rules.applied);
    assert!(deny.applied >= 2);

    let context = applied.context.expect("context");
    assert_eq!(context.settings, "deny_rules");
    assert!(!context.instructions);
}

#[tokio::test]
async fn a_dry_run_shows_the_same_object_a_run_carries() {
    let rig = Rig::new();
    let options = planted(&rig, Provider::Claude);
    let dry = dry_run(&rig.request(Provider::Claude, "success"), &options).expect("dry run");
    let result = run_checked(rig.request(Provider::Claude, "success"), options).await;
    assert_eq!(result.applied.as_ref(), Some(&dry.applied));
}

#[tokio::test]
async fn a_codex_run_has_env_but_no_deny_rules_or_context() {
    let rig = Rig::new();
    let options = planted(&rig, Provider::Codex);
    let result = run_checked(rig.request(Provider::Codex, "success"), options).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    let applied = result.applied.expect("applied");
    assert!(applied.env.is_some());
    assert!(applied.deny_rules.is_none());
    assert!(applied.context.is_none());
    assert_eq!(applied.config, origin().config);
}

#[tokio::test]
async fn failed_timed_out_and_interrupted_runs_carry_applied() {
    let rig = Rig::new();
    let failed = run_checked(
        rig.request(Provider::Claude, "exit-nonzero"),
        planted(&rig, Provider::Claude),
    )
    .await;
    assert_eq!(failed.outcome, Outcome::Failed, "{failed:#?}");

    let mut request = rig.request(Provider::Claude, "hang");
    request.timeout_secs = 1;
    let timed_out = run_checked(request, planted(&rig, Provider::Claude)).await;
    assert_eq!(timed_out.outcome, Outcome::TimedOut, "{timed_out:#?}");

    let handle = start(
        rig.request(Provider::Claude, "hang"),
        planted(&rig, Provider::Claude),
    );
    rig.wait_started().await;
    handle.interrupt();
    let interrupted = handle.wait().await;
    assert_eq!(
        interrupted.outcome,
        Outcome::Interrupted,
        "{interrupted:#?}"
    );

    for result in [failed, timed_out, interrupted] {
        let applied = result.applied.expect("applied");
        assert_eq!(applied.sources, origin().sources);
        assert!(applied.env.is_some() && applied.deny_rules.is_some());
        assert!(applied.context.is_some());
    }
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn a_refusal_before_start_carries_only_what_the_caller_said() {
    let rig = Rig::new();
    let mut options = planted(&rig, Provider::Claude);
    options.executable = Some("/nonexistent/delega/claude".into());
    let result = run_checked(rig.request(Provider::Claude, "success"), options).await;
    assert_eq!(result.outcome, Outcome::Refused, "{result:#?}");
    let applied = result.applied.expect("applied");
    assert_eq!(applied.config, origin().config);
    assert_eq!(applied.sources, origin().sources);
    assert!(applied.env.is_none(), "{applied:#?}");
    assert!(applied.deny_rules.is_none());
    assert!(applied.context.is_none());

    // An invalid request is refused before the environment too.
    let mut request = rig.request(Provider::Claude, "success");
    request.timeout_secs = 0;
    let result = run_checked(request, planted(&rig, Provider::Claude)).await;
    let applied = result.applied.expect("applied");
    assert!(applied.env.is_none() && applied.sources == origin().sources);

    // So is a forbidden environment name.
    let mut options = planted(&rig, Provider::Claude);
    options.env_pass.push("DELEGA_CONFIG".to_string());
    let result = run_checked(rig.request(Provider::Claude, "success"), options).await;
    assert_eq!(
        result.error.as_ref().map(|e| e.kind.as_str()),
        Some("forbidden_env_name")
    );
    assert!(result.applied.expect("applied").env.is_none());
}

#[tokio::test]
async fn a_run_started_without_an_origin_reports_no_sources_and_a_bare_refusal_none() {
    let rig = Rig::new();
    let result = run_checked(
        rig.request(Provider::Claude, "success"),
        rig.options(Provider::Claude),
    )
    .await;
    let applied = result.applied.expect("applied");
    assert!(applied.config.is_none() && applied.sources.is_empty());
    assert!(applied.env.is_some());

    let mut request = rig.request(Provider::Claude, "success");
    request.timeout_secs = 0;
    let refused = run_checked(request, rig.options(Provider::Claude)).await;
    assert_eq!(refused.applied, None);
    assert!(!serde_json::to_string(&refused).unwrap().contains("applied"));
}

#[tokio::test]
async fn no_environment_value_or_rule_text_reaches_applied() {
    let rig = Rig::new();
    let options = planted(&rig, Provider::Claude);
    let dry = dry_run(&rig.request(Provider::Claude, "p"), &options).expect("dry run");
    let result = run_checked(rig.request(Provider::Claude, "success"), options).await;
    let mut texts = vec![
        serde_json::to_string(&result.applied).expect("json"),
        serde_json::to_string(&dry.applied).expect("json"),
        format!("{:?}", result.applied),
    ];
    // The refusal path too.
    let mut refused_options = planted(&rig, Provider::Claude);
    refused_options.executable = Some("/nonexistent/delega/claude".into());
    let refused = run_checked(rig.request(Provider::Claude, "success"), refused_options).await;
    texts.push(serde_json::to_string(&refused.applied).expect("json"));
    for (at, text) in texts.iter().enumerate() {
        // The refusal, last, never reached the environment, so it has no names.
        assert!(
            at == texts.len() - 1 || text.contains("PLANTED_PASS"),
            "names are reported: {text}"
        );
        assert!(!text.contains(ENV_VALUE), "an environment value: {text}");
        assert!(!text.contains(RULE_TEXT), "rule text: {text}");
        assert!(
            !text.contains("PLANTED_WITHHELD") && !text.contains("DELEGA_PLANTED"),
            "the name of a withheld variable: {text}"
        );
    }
}
