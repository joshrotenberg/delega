//! The permission mapping, as the exact argument lists and settings produced.

use serde_json::{Value, json};

use super::Rig;
use crate::contract::permissions::Mode;
use crate::contract::request::{Effort, Provider, Request};
use crate::provider::launch::Inputs;
use crate::provider::{HookCommand, ProviderOptions, claude, dry_run, hook, permissions};

fn hook_command() -> HookCommand {
    HookCommand::new("/opt/delega/hook", ["permission-hook"])
}

fn options(rig: &Rig, provider: Provider) -> ProviderOptions {
    let mut options = rig.options(provider);
    options.hook = Some(hook_command());
    options
}

fn dry(rig: &Rig, request: &Request) -> crate::provider::DryRun {
    dry_run(request, &options(rig, request.provider)).expect("dry run")
}

/// The resolved working directory as it appears in a Claude Code rule.
fn rule(rig: &Rig, tool: &str) -> String {
    format!(
        "{tool}(//{}/**)",
        rig.work.to_string_lossy().trim_matches('/')
    )
}

fn settings_of(args: &[String]) -> Value {
    let at = args
        .iter()
        .position(|a| a == "--settings")
        .expect("--settings");
    serde_json::from_str(&args[at + 1]).expect("settings are JSON")
}

fn last_is_elided(args: &[String]) {
    let last = args.last().expect("arguments");
    assert!(
        last.starts_with('<') && last.ends_with(" bytes elided>"),
        "{last}"
    );
}

fn claude_base(rig: &Rig, tools: &str, allowed: &str, settings: &Value) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--permission-mode",
        "dontAsk",
        "--permission-prompts",
        "none",
        "--strict-mcp-config",
        "--mcp-config",
        "{\"mcpServers\":{}}",
        "--setting-sources",
        "",
        "--settings",
    ]
    .map(String::from)
    .to_vec();
    args.push(settings.to_string());
    args.extend(["--add-dir".into(), rig.work.to_string_lossy().into_owned()]);
    args.extend(["--disable-slash-commands".into(), "--no-chrome".into()]);
    args.extend([
        "--tools".into(),
        tools.into(),
        "--allowedTools".into(),
        allowed.into(),
    ]);
    args.push("--append-system-prompt".into());
    args
}

#[test]
fn claude_read_mode_grants_reading_the_working_directory_and_nothing_else() {
    let rig = Rig::new();
    let plan = dry(&rig, &rig.request(Provider::Claude, "p"));
    let allowed = format!("{},Glob,Grep", rule(&rig, "Read"));
    let expected = claude_base(
        &rig,
        "Read,Glob,Grep",
        &allowed,
        &json!({"disableAllHooks": true}),
    );
    assert_eq!(plan.args[..plan.args.len() - 1], expected[..]);
    last_is_elided(&plan.args);
    assert_eq!(plan.mapping.mode, Mode::Read);
    assert!(plan.mapping.commands.is_empty());
    assert_eq!(plan.mapping.on_request, "deny");
    assert!(
        plan.notes
            .iter()
            .any(|n| n.contains("Glob and Grep are granted without a path scope"))
    );
    assert!(
        plan.notes
            .iter()
            .any(|n| n.contains("including their deny rules, were not applied"))
    );
}

#[test]
fn claude_write_mode_adds_edit_and_write_scoped_to_the_working_directory() {
    let rig = Rig::new();
    let mut request = rig.request(Provider::Claude, "p");
    request.permissions.mode = Mode::Write;
    let plan = dry(&rig, &request);
    let allowed = format!(
        "{},Glob,Grep,{},{}",
        rule(&rig, "Read"),
        rule(&rig, "Edit"),
        rule(&rig, "Write")
    );
    let expected = claude_base(
        &rig,
        "Read,Glob,Grep,Edit,Write",
        &allowed,
        &json!({"disableAllHooks": true}),
    );
    assert_eq!(plan.args[..plan.args.len() - 1], expected[..]);
    assert!(
        plan.notes
            .iter()
            .any(|n| n.contains("NotebookEdit is not granted"))
    );
}

#[test]
fn claude_full_mode_grants_the_nine_builtin_tools_unscoped_and_no_hook() {
    let rig = Rig::new();
    let mut request = rig.request(Provider::Claude, "p");
    request.permissions.mode = Mode::Full;
    request.permissions.commands = vec!["ignored because full allows every command".into()];
    let plan = dry(&rig, &request);
    let all = "Read,Glob,Grep,Bash,Edit,Write,NotebookEdit,WebFetch,WebSearch";
    let expected = claude_base(&rig, all, all, &json!({"disableAllHooks": true}));
    assert_eq!(plan.args[..plan.args.len() - 1], expected[..]);
    assert!(plan.mapping.commands.is_empty());
    assert!(
        !plan
            .env_names
            .contains(&"DELEGA_EXACT_BASH_GRANTS".to_string())
    );
    assert!(plan.notes.iter().any(|n| n.contains("nine built-in tools")));
}

#[test]
fn claude_commands_become_exact_rules_and_the_guard_hook() {
    let rig = Rig::new();
    let mut request = rig.request(Provider::Claude, "p");
    request.permissions.commands = vec![
        "cargo test".into(),
        "git status".into(),
        "cargo test".into(),
    ];
    let plan = dry(&rig, &request);
    let allowed = format!(
        "{},Glob,Grep,Bash(cargo test),Bash(git status)",
        rule(&rig, "Read")
    );
    let settings = json!({
        "disableAllHooks": false,
        "hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [{
            "type": "command",
            "command": "/opt/delega/hook",
            "args": ["permission-hook"],
            "timeout": 5,
            "onFailure": "block"
        }]}]}
    });
    // The settings object serializes with sorted keys, so compare it parsed.
    assert_eq!(settings_of(&plan.args), settings);
    let expected = claude_base(
        &rig,
        "Read,Glob,Grep,Bash",
        &allowed,
        &settings_of(&plan.args),
    );
    assert_eq!(plan.args[..plan.args.len() - 1], expected[..]);
    assert_eq!(plan.mapping.commands, ["cargo test", "git status"]);
    assert!(
        plan.env_names
            .contains(&"DELEGA_EXACT_BASH_GRANTS".to_string())
    );
}

#[test]
fn claude_grants_value_is_the_exact_command_list() {
    let rig = Rig::new();
    let mut request = rig.request(Provider::Claude, "p");
    request.permissions.commands = vec!["cargo test".into(), "echo \"quoted\"".into()];
    let scope = permissions::scope(&request).unwrap();
    let hook_command = hook_command();
    let built = claude::build(&Inputs {
        request: &request,
        scope: &scope,
        hook: Some(&hook_command),
        system: "s",
        native: &[],
    })
    .unwrap();
    let grants = built
        .env
        .iter()
        .find(|(name, _)| name == hook::GRANTS_ENV)
        .map(|(_, value)| value.clone())
        .expect("grants variable");
    assert_eq!(
        serde_json::from_str::<Value>(&grants).unwrap(),
        json!(["cargo test", "echo \"quoted\""])
    );
    assert!(built.needs_hook_version);
}

#[test]
fn claude_directories_are_added_and_readable_by_rule_but_not_editable() {
    let rig = Rig::new();
    let extra = rig.home.join("docs");
    std::fs::create_dir(&extra).unwrap();
    let mut request = rig.request(Provider::Claude, "p");
    request.permissions.mode = Mode::Write;
    // One relative entry, one absolute duplicate of it, and the working directory itself.
    request.permissions.directories = vec![extra.clone(), extra.clone(), rig.work.clone()];
    let plan = dry(&rig, &request);
    let adds: Vec<&String> = plan
        .args
        .windows(2)
        .filter(|w| w[0] == "--add-dir")
        .map(|w| &w[1])
        .collect();
    assert_eq!(
        adds,
        [
            &rig.work.to_string_lossy().into_owned(),
            &extra.to_string_lossy().into_owned()
        ]
    );
    let at = plan
        .args
        .iter()
        .position(|a| a == "--allowedTools")
        .unwrap();
    let allowed = &plan.args[at + 1];
    let read_extra = format!("Read(//{}/**)", extra.to_string_lossy().trim_matches('/'));
    assert!(allowed.contains(&read_extra), "{allowed}");
    assert!(!allowed.contains(&format!(
        "Edit(//{}/",
        extra.to_string_lossy().trim_matches('/')
    )));
    assert_eq!(
        plan.mapping.directories,
        [extra.to_string_lossy().into_owned()]
    );
}

#[test]
fn claude_model_effort_resume_and_ephemeral_are_placed_before_the_system_prompt() {
    let rig = Rig::new();
    let mut request = rig.request(Provider::Claude, "p");
    request.model = Some("some-model".into());
    request.effort = Some(Effort::High);
    request.session.resume = Some("sess-1".into());
    let plan = dry(&rig, &request);
    let tail: Vec<&str> = plan.args[plan.args.len() - 8..plan.args.len() - 1]
        .iter()
        .map(String::as_str)
        .collect();
    assert_eq!(
        tail,
        [
            "--resume",
            "sess-1",
            "--model",
            "some-model",
            "--effort",
            "high",
            "--append-system-prompt"
        ]
    );
    let mut request = rig.request(Provider::Claude, "p");
    request.session.ephemeral = true;
    assert!(
        dry(&rig, &request)
            .args
            .contains(&"--no-session-persistence".to_string())
    );
}

fn codex_head(sandbox: &str, extra: &[&str]) -> Vec<String> {
    let mut args: Vec<String> = [
        "--disable",
        "plugins",
        "--disable",
        "apps",
        "--disable",
        "hooks",
        "--disable",
        "multi_agent",
        "-c",
        "approval_policy=\"never\"",
        "-c",
    ]
    .map(String::from)
    .to_vec();
    args.push(format!("sandbox_mode=\"{sandbox}\""));
    for setting in extra {
        args.extend(["-c".to_string(), (*setting).to_string()]);
    }
    args
}

fn codex_tail() -> Vec<String> {
    [
        "-c",
        "mcp_servers=<one disabled entry per configured server, listed at start>",
        "exec",
        "--ignore-user-config",
        "--json",
        "--skip-git-repo-check",
        "-",
    ]
    .map(String::from)
    .to_vec()
}

fn codex_expected(sandbox: &str, extra: &[&str]) -> Vec<String> {
    let mut args = codex_head(sandbox, extra);
    args.push("-c".into());
    args.push("developer_instructions=<ELIDED>".into());
    args.extend(codex_tail());
    args
}

fn normalized(args: &[String]) -> Vec<String> {
    args.iter()
        .map(|a| {
            if a.starts_with("developer_instructions=<") && a.ends_with(" bytes elided>") {
                "developer_instructions=<ELIDED>".to_string()
            } else {
                a.clone()
            }
        })
        .collect()
}

#[test]
fn codex_read_mode_is_the_read_only_sandbox_with_approval_never() {
    let rig = Rig::new();
    let plan = dry(&rig, &rig.request(Provider::Codex, "p"));
    assert_eq!(normalized(&plan.args), codex_expected("read-only", &[]));
    assert_eq!(plan.mapping.mode, Mode::Read);
    assert!(plan.mapping.commands.is_empty());
    assert!(
        plan.notes
            .iter()
            .any(|n| n.contains("wider than requested: the read-only sandbox"))
    );
    assert!(
        plan.notes
            .iter()
            .any(|n| n.contains("permissions.commands is not enforced as a list"))
    );
}

#[test]
fn codex_write_mode_is_workspace_write_without_network_or_temporary_directories() {
    let rig = Rig::new();
    let mut request = rig.request(Provider::Codex, "p");
    request.permissions.mode = Mode::Write;
    let plan = dry(&rig, &request);
    let extra = [
        "sandbox_workspace_write.network_access=false",
        "sandbox_workspace_write.exclude_slash_tmp=true",
        "sandbox_workspace_write.exclude_tmpdir_env_var=true",
    ];
    assert_eq!(
        normalized(&plan.args),
        codex_expected("workspace-write", &extra)
    );
    assert!(
        plan.notes
            .iter()
            .any(|n| n.contains("wider than requested: the workspace-write sandbox"))
    );
}

#[test]
fn codex_full_mode_has_no_sandbox_and_still_never_asks() {
    let rig = Rig::new();
    let mut request = rig.request(Provider::Codex, "p");
    request.permissions.mode = Mode::Full;
    let plan = dry(&rig, &request);
    assert_eq!(
        normalized(&plan.args),
        codex_expected("danger-full-access", &[])
    );
    assert!(plan.notes.iter().any(|n| n.contains("no sandbox")));
}

#[test]
fn codex_directories_and_commands_are_not_translated_into_grants() {
    let rig = Rig::new();
    let mut request = rig.request(Provider::Codex, "p");
    request.permissions.mode = Mode::Write;
    request.permissions.commands = vec!["cargo test".into()];
    request.permissions.directories = vec![rig.home.clone()];
    let plan = dry(&rig, &request);
    let joined = plan.args.join("\n");
    assert!(
        !joined.contains("--add-dir"),
        "extra directories must not become writable"
    );
    assert!(!joined.contains("cargo test"));
    assert!(!joined.contains(&*rig.home.to_string_lossy()));
    assert!(plan.mapping.commands.is_empty());
    assert!(
        plan.notes
            .iter()
            .any(|n| n.contains("permissions.directories does not limit reading"))
    );
}

#[test]
fn codex_effort_model_resume_and_ephemeral_follow_the_documented_order() {
    let rig = Rig::new();
    let mut request = rig.request(Provider::Codex, "p");
    request.effort = Some(Effort::Low);
    request.model = Some("some-model".into());
    request.session.resume = Some("thread-7".into());
    request.session.ephemeral = false;
    let plan = dry(&rig, &request);
    let mut expected = codex_head("read-only", &["model_reasoning_effort=\"low\""]);
    expected.extend([
        "-c".to_string(),
        "developer_instructions=<ELIDED>".to_string(),
    ]);
    expected.extend([
        "-c".to_string(),
        "mcp_servers=<one disabled entry per configured server, listed at start>".to_string(),
    ]);
    expected.extend(
        [
            "exec",
            "resume",
            "thread-7",
            "--ignore-user-config",
            "--json",
            "--skip-git-repo-check",
            "--model",
            "some-model",
            "-",
        ]
        .map(String::from),
    );
    assert_eq!(normalized(&plan.args), expected);

    let mut request = rig.request(Provider::Codex, "p");
    request.session.ephemeral = true;
    let args = dry(&rig, &request).args;
    let at = args
        .iter()
        .position(|a| a == "--skip-git-repo-check")
        .unwrap();
    assert_eq!(args[at + 1], "--ephemeral");
}
