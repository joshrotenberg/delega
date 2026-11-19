//! `applied` through the tools: what `start` records of the configuration file
//! and the layer of each field, and where `read`, `interrupt`, `close` and
//! `follow_up` return it.

use delega_core::contract::applied::AppliedConfig;
use delega_core::contract::partial::PartialRequest;
use delega_core::contract::request::Provider;
use serde_json::{Value, json};

use super::{Rig, assert_honest_cleanup};

const CODEX: &str = "codex";
const CONFIG_SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
/// A value no `applied` may contain.
const ENV_SENTINEL: &str = "mcp-env-sentinel-value-5521";
/// Rule text no `applied` may contain.
const RULE_SENTINEL: &str = "Bash(mcp-rule-sentinel-5521)";

fn defaults() -> PartialRequest {
    serde_json::from_value(json!({"model": "file-model", "timeout_secs": 900}))
        .expect("request defaults")
}

fn config() -> AppliedConfig {
    AppliedConfig {
        path: "/home/u/.config/delega/config.toml".to_string(),
        sha256: CONFIG_SHA.to_string(),
    }
}

async fn configured(provider: Provider) -> Rig {
    Rig::with_config(provider, defaults(), config(), Vec::new()).await
}

/// `start` arguments that set the effort, so one field comes from an argument
/// next to the two that come from the file.
fn args(rig: &Rig, provider: &str, prompt: &str) -> Value {
    let mut args = rig.start_args(provider, prompt);
    args["effort"] = json!("high");
    args
}

fn assert_start_sources(applied: &Value) {
    assert_eq!(
        applied["config"],
        json!({"path": config().path, "sha256": CONFIG_SHA}),
        "{applied}"
    );
    let sources = &applied["sources"];
    for key in ["provider", "prompt", "cwd", "effort"] {
        assert_eq!(sources[key], "argument", "{key}: {applied}");
    }
    for key in ["model", "timeout_secs"] {
        assert_eq!(sources[key], "config", "{key}: {applied}");
    }
    for key in [
        "permissions.mode",
        "permissions.on_request",
        "permissions.strict",
        "session.ephemeral",
    ] {
        assert_eq!(sources[key], "default", "{key}: {applied}");
    }
}

#[tokio::test]
async fn a_compact_and_a_full_read_both_carry_applied_with_the_layer_of_each_field() {
    let mut rig = configured(Provider::Codex).await;
    let started = rig.call("start", args(&rig, CODEX, "success")).await;
    let run = started["run"].as_str().expect("run").to_string();
    rig.finished(&run).await;

    let compact = rig.call("read", json!({"run": run})).await;
    assert_start_sources(&compact["result"]["applied"]);
    assert!(compact["result"]["applied"]["env"].is_object(), "{compact}");
    let full = rig.read_full(&run).await;
    assert_start_sources(&full["result"]["applied"]);
    assert_eq!(
        compact["result"]["applied"], full["result"]["applied"],
        "the two detail levels report the same object"
    );
    rig.finish().await;
}

#[tokio::test]
async fn a_refused_start_reads_back_config_and_sources_without_the_prepared_parts() {
    let mut rig = configured(Provider::Codex).await;
    let mut bad = args(&rig, CODEX, "success");
    bad["permissions"] = json!({"strict": true});
    let started = rig.call("start", bad).await;
    let run = started["run"].as_str().expect("run").to_string();
    let read = rig.call("read", json!({"run": run})).await;
    assert_eq!(read["result"]["outcome"], "refused", "{read}");
    let applied = &read["result"]["applied"];
    assert_eq!(applied["config"]["sha256"], CONFIG_SHA, "{read}");
    assert_eq!(
        applied["sources"]["permissions.strict"], "argument",
        "{read}"
    );
    assert_eq!(
        read["result"]["error"]["kind"],
        "permissions_not_enforceable"
    );
    for part in ["env", "deny_rules", "context"] {
        assert!(applied.get(part).is_none(), "{part}: {read}");
    }
    rig.finish().await;
}

#[tokio::test]
async fn interrupt_and_close_return_applied_in_their_compact_result() {
    let mut rig = configured(Provider::Codex).await;
    let gate = rig.gate("applied-stop");
    let started = rig.call("start", args(&rig, CODEX, &gate.prompt())).await;
    gate.started().await;
    let stopped = rig.call("interrupt", json!({"run": started["run"]})).await;
    assert_eq!(stopped["result"]["outcome"], "interrupted", "{stopped}");
    assert_honest_cleanup(&stopped["result"]["cleanup"]);
    assert_start_sources(&stopped["result"]["applied"]);

    let second_gate = rig.gate("applied-close");
    let second = rig
        .call("start", args(&rig, CODEX, &second_gate.prompt()))
        .await;
    second_gate.started().await;
    let closed = rig.call("close", json!({"worker": second["worker"]})).await;
    assert_eq!(closed["result"]["outcome"], "interrupted", "{closed}");
    assert_honest_cleanup(&closed["result"]["cleanup"]);
    assert_start_sources(&closed["result"]["applied"]);
    rig.finish().await;
}

#[tokio::test]
async fn a_follow_up_reports_the_starts_sources_with_prompt_and_a_given_timeout_as_argument() {
    let mut rig = configured(Provider::Codex).await;
    let started = rig.call("start", args(&rig, CODEX, "echo:one")).await;
    let worker = started["worker"].as_str().expect("worker").to_string();
    rig.finished(started["run"].as_str().expect("run")).await;

    // Without a time limit of its own the follow-up keeps the start's.
    let plain = rig
        .call("follow_up", json!({"worker": worker, "prompt": "echo:two"}))
        .await;
    let plain_run = plain["run"].as_str().expect("run").to_string();
    rig.finished(&plain_run).await;
    let read = rig.call("read", json!({"run": plain_run})).await;
    let applied = &read["result"]["applied"];
    assert_start_sources(applied);
    assert_eq!(applied["sources"]["timeout_secs"], "config", "{read}");

    // With one, the time limit is an argument too; the rest is the start's.
    let timed = rig
        .call(
            "follow_up",
            json!({"worker": worker, "prompt": "echo:three", "timeout_secs": 120}),
        )
        .await;
    let timed_run = timed["run"].as_str().expect("run").to_string();
    rig.finished(&timed_run).await;
    let read = rig.call("read", json!({"run": timed_run})).await;
    let applied = &read["result"]["applied"];
    assert_eq!(applied["sources"]["timeout_secs"], "argument", "{read}");
    assert_eq!(applied["sources"]["prompt"], "argument", "{read}");
    assert_eq!(applied["sources"]["model"], "config", "{read}");
    assert_eq!(applied["config"]["sha256"], CONFIG_SHA, "{read}");
    rig.finish().await;
}

#[tokio::test]
async fn an_mcp_read_never_holds_an_environment_value_or_a_deny_rule() {
    let mut rig = Rig::with_config(
        Provider::Claude,
        defaults(),
        config(),
        vec![("MCP_SENTINEL_VAR".to_string(), ENV_SENTINEL.to_string())],
    )
    .await;
    std::fs::write(
        rig.config_dir.join("settings.json"),
        json!({"permissions": {"deny": [RULE_SENTINEL]}}).to_string(),
    )
    .expect("write settings");
    let started = rig.call("start", args(&rig, "claude", "success")).await;
    let run = started["run"].as_str().expect("run").to_string();
    rig.finished(&run).await;

    let compact = rig.call("read", json!({"run": run})).await;
    let full = rig.read_full(&run).await;
    for read in [compact, full] {
        let applied = &read["result"]["applied"];
        // The rule is counted and the file named, so the section is not empty.
        assert_eq!(applied["deny_rules"]["applied"], 1, "{read}");
        assert_eq!(applied["context"]["settings"], "deny_rules", "{read}");
        assert!(
            applied["env"]["passed"]["total"].as_u64() > Some(0),
            "{read}"
        );
        let shown = applied.to_string();
        assert!(
            !shown.contains(ENV_SENTINEL),
            "an environment value is shown"
        );
        assert!(!shown.contains(RULE_SENTINEL), "a deny rule is shown");
    }
    rig.finish().await;
}
