//! `dry_run`: what would run, with no secrets and no prompt.

use std::ffi::OsString;

use super::{Rig, both};
use crate::contract::request::Provider;
use crate::provider::dry_run;

const PROMPT: &str = "TOPSECRET-PROMPT-TEXT";
const INSTRUCTIONS: &str = "TOPSECRET-INSTRUCTION-TEXT";
const CANARY: &str = "canary-value-must-not-appear";

async fn reports_what_would_run(provider: Provider) {
    let rig = Rig::new();
    let mut request = rig.request(provider, PROMPT);
    request.instructions = Some(INSTRUCTIONS.into());
    request.timeout_secs = 90;
    let mut options = rig.options(provider);
    let mut env = options.parent_env.take().unwrap();
    env.push((
        OsString::from("MY_SERVICE_API_TOKEN"),
        OsString::from(CANARY),
    ));
    env.push((OsString::from("MY_PLAIN_SETTING"), OsString::from(CANARY)));
    options.parent_env = Some(env);
    options
        .env_set
        .push(("MY_EXPLICIT_SECRET".into(), CANARY.into()));

    let plan = dry_run(&request, &options).expect("dry run");

    assert_eq!(plan.provider, provider);
    assert_eq!(plan.program, Rig::fake(provider).to_string_lossy());
    assert_eq!(plan.cwd, rig.work.to_string_lossy());
    assert_eq!(plan.time_limit_secs, 90);
    assert!(plan.stdin_bytes >= PROMPT.len());
    assert!(plan.env_names.contains(&"MY_PLAIN_SETTING".to_string()));
    assert!(plan.env_names.contains(&"MY_EXPLICIT_SECRET".to_string()));
    assert!(!plan.env_names.contains(&"MY_SERVICE_API_TOKEN".to_string()));
    assert!(plan.before_start.iter().any(|l| l.contains("--version")));
    assert_eq!(plan.mapping.on_request, "deny");

    // Neither the prompt, the instructions nor any environment value is in
    // the report, in either its JSON or its debug form.
    let json = serde_json::to_string(&plan).unwrap();
    for text in [json, format!("{plan:?}")] {
        assert!(!text.contains(PROMPT), "{text}");
        assert!(!text.contains(INSTRUCTIONS), "{text}");
        assert!(!text.contains(CANARY), "{text}");
    }
    assert!(plan.args.iter().any(|a| a.contains("bytes elided")));

    // Nothing was started.
    assert!(rig.pid("pid").is_none());
    assert!(rig.argv().is_empty());
}
both!(
    reports_the_program_arguments_names_and_mapping_only,
    reports_what_would_run
);

async fn refuses_like_a_run(provider: Provider) {
    let rig = Rig::new();
    let mut request = rig.request(provider, "p");
    request.native_args = vec!["--sandbox".into(), "x".into()];
    let error = dry_run(&request, &rig.options(provider)).unwrap_err();
    assert_eq!(error.kind, "native_arg_rejected");

    let mut request = rig.request(provider, "");
    request.cwd = None;
    let error = dry_run(&request, &rig.options(provider)).unwrap_err();
    assert_eq!(error.kind, "invalid_request");
}
both!(returns_the_same_refusal_a_run_would, refuses_like_a_run);

#[tokio::test]
async fn codex_dry_run_lists_the_discovery_step_and_claude_the_version_gate() {
    let rig = Rig::new();
    let codex = dry_run(
        &rig.request(Provider::Codex, "p"),
        &rig.options(Provider::Codex),
    )
    .unwrap();
    assert!(
        codex
            .before_start
            .iter()
            .any(|l| l.contains("mcp list --json"))
    );
    assert!(codex.args.iter().any(|a| a.starts_with("mcp_servers=<")));

    let mut request = rig.request(Provider::Claude, "p");
    request.permissions.commands = vec!["make".into()];
    let mut options = rig.options(Provider::Claude);
    options.hook = Some(crate::provider::HookCommand::new(
        "/opt/hook",
        ["permission-hook"],
    ));
    let claude = dry_run(&request, &options).unwrap();
    assert!(claude.before_start.iter().any(|l| l.contains("2.1.325")));
}
