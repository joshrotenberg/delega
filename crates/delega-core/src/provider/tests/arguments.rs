//! Generated launch arguments over the per-argument limit refuse the run
//! before the provider starts; an argument exactly at the limit runs.

use serde_json::json;

use super::{Rig, run_checked};
use crate::contract::outcome::Outcome;
use crate::contract::permissions::Mode;
use crate::contract::request::{Provider, Request};
use crate::contract::result::Cleanup;
use crate::provider::limits::ARGUMENT_LIMIT;
use crate::provider::{ProviderOptions, dry_run};

/// The length of each full command, within the per-command limit.
const COMMAND_BYTES: usize = 4000;

/// Long extra directories under the working directory, within the count limit.
fn long_directories(rig: &Rig) -> Vec<std::path::PathBuf> {
    (0..8)
        .map(|i| {
            let dir = rig.work.join(format!("d{i}-{}", "a".repeat(200)));
            std::fs::create_dir_all(&dir).expect("create directory");
            dir
        })
        .collect()
}

fn claude_request(rig: &Rig, commands: Vec<String>) -> (Request, ProviderOptions) {
    let mut request = rig.request(Provider::Claude, "success");
    request.permissions.mode = Mode::Read;
    request.permissions.directories = long_directories(rig);
    request.permissions.commands = commands;
    let mut options = rig.options(Provider::Claude);
    options.hook = Some(Rig::guard_hook());
    (request, options)
}

fn allowed_tools(args: &[String]) -> &str {
    let at = args
        .iter()
        .position(|a| a == "--allowedTools")
        .expect("--allowedTools in the launch");
    &args[at + 1]
}

/// Commands that make `--allowedTools` exactly `ARGUMENT_LIMIT + extra` bytes
/// for the request [`claude_request`] builds under `rig`.
fn commands_for(rig: &Rig, extra: usize) -> Vec<String> {
    let (request, options) = claude_request(rig, vec!["z".to_string()]);
    let plan = dry_run(&request, &options).expect("a short request is planned");
    let base = allowed_tools(&plan.args).len();
    let step = COMMAND_BYTES + ",Bash()".len();
    let full = (ARGUMENT_LIMIT - base) / step;
    let rest = ARGUMENT_LIMIT - base - full * step + extra;
    let mut commands: Vec<String> = (0..full)
        .map(|i| format!("c{i:03}{}", "c".repeat(COMMAND_BYTES - 4)))
        .collect();
    commands.push("z".repeat(1 + rest));
    commands
}

#[tokio::test]
async fn claude_allowed_tools_over_the_argument_limit_is_refused_before_any_process() {
    let rig = Rig::new();
    let (request, options) = claude_request(&rig, commands_for(&rig, 0));
    let plan = dry_run(&request, &options).expect("exactly at the limit is planned");
    assert_eq!(allowed_tools(&plan.args).len(), ARGUMENT_LIMIT);
    let result = run_checked(request, options).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    assert_eq!(allowed_tools(&rig.argv()).len(), ARGUMENT_LIMIT);
    assert!(rig.version_probed(), "the version probe leaves its record");
    rig.assert_nothing_left().await;

    let rig = Rig::new();
    let (request, options) = claude_request(&rig, commands_for(&rig, 1));
    assert!(
        request.validate().is_ok(),
        "the request is within its limits"
    );
    let refused = dry_run(&request, &options).expect_err("a dry run refuses too");
    assert_eq!(refused.kind, "argument_too_long");
    let result = run_checked(request, options).await;
    assert_eq!(result.outcome, Outcome::Refused, "{result:#?}");
    let error = result.error.expect("error");
    assert_eq!(error.kind, "argument_too_long");
    assert!(
        error.message.contains("--allowedTools"),
        "{}",
        error.message
    );
    assert!(
        error
            .message
            .contains(&format!("{} bytes", ARGUMENT_LIMIT + 1)),
        "{}",
        error.message
    );
    assert!(!error.message.contains("cccc"), "{}", error.message);
    assert!(!rig.version_probed(), "no version probe ran");
    assert!(rig.pid("pid").is_none(), "no provider process ran");
    assert!(rig.argv().is_empty(), "no provider process ran");
}

#[tokio::test]
async fn claude_command_grants_entry_over_the_argument_limit_is_refused_before_any_process() {
    // A quote is one byte in a rule and two in the JSON grant list, so the
    // grant entry goes over the limit while `--allowedTools` stays under it.
    let rig = Rig::new();
    let commands: Vec<String> = (0..20)
        .map(|i| format!("c{i:03}{}", "\"".repeat(COMMAND_BYTES - 4)))
        .collect();
    let grants: usize = crate::provider::hook::GRANTS_ENV.len()
        + 1
        + serde_json::Value::from(commands.clone()).to_string().len();
    assert!(grants > ARGUMENT_LIMIT, "{grants}");
    let (request, options) = claude_request(&rig, commands);
    assert!(
        request.validate().is_ok(),
        "the request is within its limits"
    );
    let refused = dry_run(&request, &options).expect_err("a dry run refuses too");
    assert_eq!(refused.kind, "argument_too_long");
    let result = run_checked(request, options).await;
    assert_eq!(result.outcome, Outcome::Refused, "{result:#?}");
    let error = result.error.expect("error");
    assert_eq!(error.kind, "argument_too_long");
    assert!(
        error.message.contains(crate::provider::hook::GRANTS_ENV),
        "{}",
        error.message
    );
    assert!(
        error.message.contains(&format!("{grants} bytes")),
        "{}",
        error.message
    );
    assert!(!error.message.contains("\"\"\"\""), "{}", error.message);
    assert!(!rig.version_probed(), "no version probe ran");
    assert!(rig.pid("pid").is_none(), "no provider process ran");
}

/// Options for a kept Codex run that inherits a run-marker list sized so the
/// entry the run passes is exactly `ARGUMENT_LIMIT + extra` bytes. The keeper
/// is never started: the plan refuses first, or the test only plans.
fn kept_options(rig: &Rig, extra: usize) -> ProviderOptions {
    use crate::process::MARKER_ENV;
    // `NAME=`, the inherited list, `:` and this run's 32 hex characters.
    let overhead = MARKER_ENV.len() + 1 + 1 + 32;
    let mut options = rig.options(Provider::Codex);
    options.keeper = Some(crate::process::KeeperCommand::new(
        "/nonexistent/delega-keeper",
        ["keeper"],
    ));
    options.inherited_marker = Some("a".repeat(ARGUMENT_LIMIT - overhead + extra).into());
    options
}

#[tokio::test]
async fn an_inherited_run_marker_that_would_go_over_the_argument_limit_refuses_a_kept_run() {
    let rig = Rig::new();
    dry_run(
        &rig.request(Provider::Codex, "success"),
        &kept_options(&rig, 0),
    )
    .expect("a marker entry exactly at the limit is planned");

    let request = rig.request(Provider::Codex, "success");
    let options = kept_options(&rig, 1);
    let refused = dry_run(&request, &options).expect_err("a dry run refuses too");
    assert_eq!(refused.kind, "argument_too_long");
    let result = run_checked(request, options).await;
    assert_eq!(result.outcome, Outcome::Refused, "{result:#?}");
    let error = result.error.expect("error");
    assert_eq!(error.kind, "argument_too_long");
    assert!(
        error.message.contains(crate::process::MARKER_ENV),
        "{}",
        error.message
    );
    assert!(
        error
            .message
            .contains(&format!("{} bytes", ARGUMENT_LIMIT + 1)),
        "{}",
        error.message
    );
    assert!(!error.message.contains("aaaa"), "{}", error.message);
    assert!(!rig.version_probed(), "no version probe ran");
    assert!(rig.pid("pid").is_none(), "no provider process ran");

    // Without a keeper no marker is added, so the same inheritance is fine.
    let mut options = kept_options(&rig, 1);
    options.keeper = None;
    dry_run(&rig.request(Provider::Codex, "success"), &options)
        .expect("an unkept run passes no marker");
}

/// Options whose fake Codex lists one stdio MCP server with a name that makes
/// the override exactly `ARGUMENT_LIMIT + extra` bytes.
fn codex_options(rig: &Rig, extra: usize) -> ProviderOptions {
    let listing = |name: &str| json!([{"name": name, "transport": {"type": "stdio"}}]).to_string();
    let overhead = crate::provider::codex::disabled_servers(&listing(""))
        .expect("override")
        .len();
    let name = "n".repeat(ARGUMENT_LIMIT - overhead + extra);
    let file = rig.home.join("mcp.json");
    std::fs::write(&file, listing(&name)).expect("write listing");
    let mut options = rig.options(Provider::Codex);
    options.env_set.extend([
        ("FAKE_MCP".into(), "file".into()),
        ("FAKE_MCP_FILE".into(), file.to_string_lossy().into_owned()),
    ]);
    options
}

#[tokio::test]
async fn codex_mcp_override_over_the_argument_limit_is_refused_before_the_provider_starts() {
    let rig = Rig::new();
    let result = run_checked(
        rig.request(Provider::Codex, "success"),
        codex_options(&rig, 0),
    )
    .await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    let argv = rig.argv();
    assert!(
        argv.iter()
            .any(|a| a.starts_with("mcp_servers=") && a.len() == ARGUMENT_LIMIT),
        "the override at the limit is passed"
    );
    rig.assert_nothing_left().await;

    let rig = Rig::new();
    let result = run_checked(
        rig.request(Provider::Codex, "success"),
        codex_options(&rig, 1),
    )
    .await;
    assert!(!rig.mcp_argv().is_empty(), "discovery ran");
    assert!(rig.pid("pid").is_none(), "the provider never started");
    assert!(rig.argv().is_empty(), "the provider never started");
    assert_eq!(result.timing.started_at, None);
    assert_eq!(result.timing.duration_ms, 0);
    let error = result.error.as_ref().expect("error");
    assert_eq!(error.kind, "argument_too_long");
    assert!(
        error.message.contains("-c mcp_servers"),
        "{}",
        error.message
    );
    assert!(
        error
            .message
            .contains(&format!("{} bytes", ARGUMENT_LIMIT + 1)),
        "{}",
        error.message
    );
    assert!(!error.message.contains("nnnn"), "{}", error.message);
    // The probes left nothing behind, so the run is refused, like other
    // refusals after the probes.
    assert_eq!(result.cleanup, Cleanup::Confirmed, "{result:#?}");
    assert_eq!(result.outcome, Outcome::Refused, "{result:#?}");
}
