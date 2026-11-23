//! `applied` in the result of a real `delega run`, as a black box against the
//! fake providers, with a temporary HOME and configuration file.
//!
//! Every test runs the built executable with an empty environment plus the
//! rig's, so nothing reads the real configuration, a real home directory or a
//! real provider CLI.

mod support;

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Output;

use serde_json::Value;
use sha2::{Digest, Sha256};
use support::{Guarded, Rig, text};

/// A value no output's `applied` may contain.
const ENV_SENTINEL: &str = "cli-env-sentinel-value-3308";
/// Rule text no output's `applied` may contain.
const RULE_SENTINEL: &str = "Bash(cli-rule-sentinel-3308)";

fn config_path(rig: &Rig) -> PathBuf {
    rig.home.join("delega").join("config.toml")
}

fn write_config(rig: &Rig, body: &str) -> PathBuf {
    let path = config_path(rig);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("create directory");
    std::fs::write(&path, body).expect("write config");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    path
}

fn json_of(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "not JSON: {e}\n{}\nstderr: {}",
            text(&out.stdout),
            text(&out.stderr)
        )
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

const CONFIG: &str = r#"
schema_version = 1

[defaults]
model = "file-model"
effort = "high"

[env]
pass = ["CONFIG_NAMED_VAR"]
"#;

#[test]
fn a_real_run_reports_the_file_and_the_layer_that_set_each_field() {
    let rig = Rig::new();
    let path = write_config(&rig, CONFIG);
    // The profile overrides the file's effort and sets the time limit; a flag
    // overrides the profile's time limit.
    let profile = rig.file(
        "profile.json",
        r#"{"schema_version": 1, "effort": "low", "timeout_secs": 900,
            "permissions": {"mode": "write"}}"#,
    );
    let profile = profile.to_string_lossy().into_owned();
    let out = rig
        .command()
        .env("CONFIG_NAMED_VAR", "present")
        .env("UNLISTED_VAR", "present")
        .args(rig.run_args(
            "claude",
            "success",
            &[
                "--output",
                "json",
                "--request",
                &profile,
                "--timeout",
                "300s",
            ],
        ))
        .guarded_output()
        .expect("run delega");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let result = json_of(&out);
    assert_eq!(result["outcome"], "completed", "{result}");
    let applied = &result["applied"];

    assert_eq!(
        applied["config"]["path"],
        path.to_string_lossy().as_ref(),
        "{applied}"
    );
    assert_eq!(
        applied["config"]["sha256"],
        sha256_hex(CONFIG.as_bytes()),
        "the digest is of the file's bytes"
    );

    let sources = &applied["sources"];
    let expect = [
        ("provider", "flag"),
        ("prompt", "flag"),
        ("cwd", "flag"),
        ("model", "config"),
        ("effort", "profile"),
        ("permissions.mode", "profile"),
        ("timeout_secs", "flag"),
        ("permissions.on_request", "default"),
        ("permissions.strict", "default"),
        ("session.ephemeral", "default"),
    ];
    for (key, layer) in expect {
        assert_eq!(sources[key], layer, "{key}: {sources}");
    }
    assert_eq!(
        sources.as_object().expect("sources").len(),
        expect.len(),
        "no other field was set: {sources}"
    );

    let env = &applied["env"];
    assert_eq!(
        env["named"],
        serde_json::json!(["CONFIG_NAMED_VAR", "FAKE_RECORD_DIR"]),
        "{env}"
    );
    let passed = &env["passed"];
    assert_eq!(passed["named_by_caller"], 2, "{env}");
    let parts: u64 = [
        "base",
        "network",
        "provider",
        "named_by_caller",
        "set_by_caller",
        "set_by_delega",
    ]
    .iter()
    .map(|k| passed[k].as_u64().expect("count"))
    .sum();
    assert_eq!(passed["total"].as_u64(), Some(parts), "{env}");
    let withheld = &env["withheld"];
    assert_eq!(
        withheld["never_passed"], 3,
        "provider binaries and test system root: {env}"
    );
    assert!(withheld["not_allowlisted"].as_u64() >= Some(1), "{env}");
    assert_eq!(
        withheld["total"].as_u64(),
        Some(
            withheld["never_passed"].as_u64().expect("count")
                + withheld["not_allowlisted"].as_u64().expect("count")
        ),
        "{env}"
    );

    assert_eq!(applied["deny_rules"]["applied"], 0, "{applied}");
    assert_eq!(applied["deny_rules"]["files"], serde_json::json!([]));
    assert_eq!(applied["context"]["settings"], "deny_rules", "{applied}");
    assert_eq!(applied["context"]["instructions"], false, "{applied}");
    rig.assert_nothing_left();
}

#[test]
fn a_codex_run_reports_env_but_no_deny_rules_or_context() {
    let rig = Rig::new();
    write_config(&rig, CONFIG);
    let out = rig.run("codex", "success", &["--output", "json"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let applied = &json_of(&out)["applied"];
    assert!(applied["env"].is_object(), "{applied}");
    assert!(applied.get("deny_rules").is_none(), "{applied}");
    assert!(applied.get("context").is_none(), "{applied}");
    assert_eq!(applied["sources"]["model"], "config", "{applied}");
    rig.assert_nothing_left();
}

/// `delega run` refusals after the request was merged, each with the file's
/// layer information and nothing that needs a prepared run.
#[test]
fn a_refusal_after_the_merge_carries_config_and_sources_only() {
    let rig = Rig::new();
    write_config(&rig, CONFIG);
    let missing = rig.path("no-such-directory");
    let cases: Vec<(&str, Output, &str)> = vec![
        (
            "a forbidden --env name",
            rig.run(
                "claude",
                "success",
                &["--output", "json", "--env", "DELEGA_X"],
            ),
            "forbidden_env_name",
        ),
        (
            "an unusable working directory",
            rig.run_in(&missing, "claude", "success", &["--output", "json"]),
            "cwd_unusable",
        ),
    ];
    for (what, out, kind) in cases {
        assert_eq!(out.status.code(), Some(2), "{what}: {}", text(&out.stderr));
        let result = json_of(&out);
        assert_eq!(result["outcome"], "refused", "{what}: {result}");
        assert_eq!(result["error"]["kind"], kind, "{what}: {result}");
        let applied = &result["applied"];
        assert_eq!(
            applied["config"]["sha256"],
            sha256_hex(CONFIG.as_bytes()),
            "{what}: {result}"
        );
        assert_eq!(applied["sources"]["provider"], "flag", "{what}: {result}");
        assert_eq!(applied["sources"]["model"], "config", "{what}: {result}");
        for part in ["env", "deny_rules", "context"] {
            assert!(applied.get(part).is_none(), "{what}: {part}: {result}");
        }
    }
    assert!(!rig.record.join("argv").exists(), "a provider was started");
}

#[test]
fn a_refusal_for_an_unusable_config_file_has_no_applied() {
    let rig = Rig::new();
    write_config(&rig, "schema_version = 1\n[defaults\n");
    let out = rig.run("claude", "success", &["--output", "json"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    let result = json_of(&out);
    assert_eq!(result["outcome"], "refused", "{result}");
    assert!(
        result.get("applied").is_none(),
        "no file was read and no request was built: {result}"
    );
    assert!(!rig.record.join("argv").exists(), "a provider was started");
}

#[test]
fn applied_never_holds_an_environment_value_or_a_deny_rule() {
    let rig = Rig::new();
    write_config(
        &rig,
        &format!(
            "schema_version = 1\n[env.set]\nCONFIG_SET_VAR = \"{ENV_SENTINEL}\"\n\
             [env]\npass = [\"CONFIG_NAMED_VAR\"]\n"
        ),
    );
    std::fs::write(
        rig.home.join("settings.json"),
        format!(r#"{{"permissions":{{"deny":["{RULE_SENTINEL}"]}}}}"#),
    )
    .expect("write settings");
    let flag_value = format!("FLAG_SET_VAR={ENV_SENTINEL}");
    let out = rig
        .command()
        .env("CONFIG_NAMED_VAR", ENV_SENTINEL)
        .env("UNLISTED_VAR", ENV_SENTINEL)
        .args(rig.run_args(
            "claude",
            "success",
            &["--output", "json", "--env", &flag_value],
        ))
        .guarded_output()
        .expect("run delega");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let result = json_of(&out);
    let applied = &result["applied"];
    // The rule is counted and the values are counted, so the check is not
    // passing on an empty object.
    assert_eq!(applied["deny_rules"]["applied"], 1, "{applied}");
    assert_eq!(applied["env"]["passed"]["set_by_caller"], 2, "{applied}");
    let shown = applied.to_string();
    assert!(
        !shown.contains(ENV_SENTINEL),
        "an environment value is shown"
    );
    assert!(!shown.contains(RULE_SENTINEL), "a deny rule is shown");
    // A dry run's `applied` holds the same.
    let out = rig
        .command()
        .env("CONFIG_NAMED_VAR", ENV_SENTINEL)
        .args(rig.run_args(
            "claude",
            "success",
            &["--output", "json", "--dry-run", "--env", &flag_value],
        ))
        .guarded_output()
        .expect("run delega");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let shown = json_of(&out)["dry_run"]["applied"].to_string();
    assert!(
        !shown.contains(ENV_SENTINEL),
        "an environment value is shown"
    );
    assert!(!shown.contains(RULE_SENTINEL), "a deny rule is shown");
    rig.assert_nothing_left();
}
