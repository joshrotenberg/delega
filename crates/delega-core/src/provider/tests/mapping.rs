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
    let allowed = rule(&rig, "Read");
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
        !plan.notes.iter().any(|n| n.contains("Glob and Grep")),
        "Glob and Grep are governed by the Read rules and need no note"
    );
    assert!(
        plan.notes
            .iter()
            .any(|n| n.contains("other than their deny rules were not applied"))
    );
}

#[test]
fn claude_write_mode_scopes_edit_and_write_tools_with_one_edit_rule() {
    let rig = Rig::new();
    let mut request = rig.request(Provider::Claude, "p");
    request.permissions.mode = Mode::Write;
    let plan = dry(&rig, &request);
    let allowed = format!("{},{}", rule(&rig, "Read"), rule(&rig, "Edit"));
    let expected = claude_base(
        &rig,
        "Read,Glob,Grep,Edit,Write",
        &allowed,
        &json!({"disableAllHooks": true}),
    );
    assert_eq!(plan.args[..plan.args.len() - 1], expected[..]);
    assert!(
        plan.mapping
            .settings
            .contains(&format!("allowedTools={allowed}"))
    );
    assert!(
        plan.notes
            .iter()
            .any(|n| n.contains("NotebookEdit is not granted"))
    );
}

#[test]
fn claude_build_sandbox_denies_only_session_paths_for_the_given_uid() {
    let deny_write = [
        "//tmp/claude-501/-*",
        #[cfg(target_os = "macos")]
        "//private/tmp/claude-501/-*",
    ];
    assert_eq!(
        claude::sandbox_value(501),
        json!({
            "allowUnsandboxedCommands": false,
            "autoAllowBashIfSandboxed": true,
            "enabled": true,
            "failIfUnavailable": true,
            "filesystem": { "denyWrite": deny_write },
        })
    );
}

#[test]
fn claude_build_mode_adds_unruled_bash_inside_a_sandbox_that_fails_closed() {
    let rig = Rig::new();
    let mut request = rig.request(Provider::Claude, "p");
    request.permissions.mode = Mode::Build;
    let plan = dry(&rig, &request);
    let allowed = format!("{},{}", rule(&rig, "Read"), rule(&rig, "Edit"));
    let settings = crate::canonical::sorted(json!({
        "disableAllHooks": true,
        "sandbox": claude::sandbox_value(rustix::process::geteuid().as_raw()),
    }));
    let expected = claude_base(&rig, "Read,Glob,Grep,Edit,Write,Bash", &allowed, &settings);
    assert_eq!(plan.args[..plan.args.len() - 1], expected[..]);
    assert!(
        plan.mapping
            .settings
            .contains(&format!("allowedTools={allowed}"))
    );
    assert_eq!(plan.mapping.mode, Mode::Build);
    assert!(plan.mapping.commands.is_empty());
    assert!(
        !plan
            .env_names
            .contains(&"DELEGA_EXACT_BASH_GRANTS".to_string()),
        "build needs no command guard"
    );
    let sandbox = plan
        .mapping
        .settings
        .iter()
        .find_map(|s| s.strip_prefix("sandbox="))
        .expect("sandbox mapping");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(sandbox).unwrap(),
        settings["sandbox"]
    );
    for part in [
        "wider than requested: commands inside the sandbox can read",
        "directory except its per-project session folders",
        "write files directly in it",
        "permissions.directories is granted as Read rules (used by Read, Glob and Grep) and is \
         not added to the sandbox, so delega does not make it writable, though one inside a \
         location commands can write stays writable",
        "bubblewrap and socat",
        "git -c name=value) are denied and listed in permissions.denied",
        "on macOS a live probe found /bin/ps refused inside the sandbox",
    ] {
        assert!(
            plan.notes.iter().any(|n| n.contains(part)),
            "missing note {part}: {:?}",
            plan.notes
        );
    }
}

#[test]
fn claude_build_mode_grants_extra_directories_as_read_rules_without_add_dir() {
    let rig = Rig::new();
    let extra = rig.home.join("docs");
    std::fs::create_dir(&extra).unwrap();
    let resolved = std::fs::canonicalize(&extra).unwrap();
    // The parent of the working directory too: listing it must not make the
    // working directory read-only or writable beyond the sandbox.
    let parent = std::fs::canonicalize(rig.work.parent().unwrap()).unwrap();
    let mut request = rig.request(Provider::Claude, "p");
    request.permissions.mode = Mode::Build;
    request.permissions.directories = vec![extra.clone(), parent.clone()];
    let plan = dry(&rig, &request);
    let adds: Vec<&String> = plan
        .args
        .windows(2)
        .filter(|w| w[0] == "--add-dir")
        .map(|w| &w[1])
        .collect();
    assert_eq!(
        adds,
        [&std::fs::canonicalize(&rig.work)
            .unwrap()
            .to_string_lossy()
            .into_owned()],
        "only the working directory is added; the sandbox makes --add-dir writable"
    );
    let at = plan
        .args
        .iter()
        .position(|a| a == "--allowedTools")
        .unwrap();
    for dir in [&resolved, &parent] {
        let read = format!("Read(//{}/**)", dir.to_string_lossy().trim_matches('/'));
        assert!(plan.args[at + 1].contains(&read), "{}", plan.args[at + 1]);
    }
    let settings = settings_of(&plan.args);
    assert_eq!(
        settings["sandbox"]["filesystem"],
        claude::sandbox_value(rustix::process::geteuid().as_raw())["filesystem"],
        "only Claude Code's session folders are denied"
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
    let allowed = format!("{},Bash(cargo test),Bash(git status)", rule(&rig, "Read"));
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
        deny: &crate::provider::deny::DenyRules::default(),
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
fn every_claude_launch_turns_off_memory_files_and_auto_memory() {
    // Without these the worker can get the user's auto-memory index from their
    // home directory, whatever --setting-sources says (#106).
    let rig = Rig::new();
    for mode in [Mode::Read, Mode::Write, Mode::Build, Mode::Full] {
        let mut request = rig.request(Provider::Claude, "p");
        request.permissions.mode = mode;
        let scope = permissions::scope(&request).unwrap();
        let hook_command = hook_command();
        let built = claude::build(&Inputs {
            request: &request,
            scope: &scope,
            hook: Some(&hook_command),
            system: "s",
            native: &[],
            deny: &crate::provider::deny::DenyRules::default(),
        })
        .unwrap();
        for name in [
            "CLAUDE_CODE_DISABLE_AUTO_MEMORY",
            "CLAUDE_CODE_DISABLE_CLAUDE_MDS",
        ] {
            let value = built
                .env
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.as_str());
            assert_eq!(value, Some("1"), "{name} in {mode:?}");
        }
        assert!(
            built
                .args
                .windows(2)
                .any(|w| w == ["--setting-sources", ""]),
            "{:?}",
            built.args
        );
    }
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
    args.extend(["-c".to_string(), "allow_login_shell=false".to_string()]);
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
fn codex_build_mode_is_workspace_write_without_network_but_with_temporary_directories() {
    let rig = Rig::new();
    let mut request = rig.request(Provider::Codex, "p");
    request.permissions.mode = Mode::Build;
    let plan = dry(&rig, &request);
    let extra = [
        "sandbox_workspace_write.network_access=false",
        "sandbox_workspace_write.exclude_slash_tmp=false",
        "sandbox_workspace_write.exclude_tmpdir_env_var=false",
    ];
    assert_eq!(
        normalized(&plan.args),
        codex_expected("workspace-write", &extra)
    );
    assert_eq!(plan.mapping.mode, Mode::Build);
    assert!(plan.notes.iter().any(|n| {
        n.contains("wider than requested: the workspace-write sandbox")
            && n.contains("under /tmp and $TMPDIR, including a permissions.directories entry there, except .git")
            && n.contains("on macOS a live probe found /bin/ps refused inside the sandbox")
    }));
    assert!(
        plan.notes
            .iter()
            .any(|n| n.contains(".git unwritable") && n.contains("commit the worker's changes")),
        "{:?}",
        plan.notes
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

#[test]
fn ask_rules_keep_read_only_bash_from_bypassing_the_host_without_overriding_the_guard_or_sandbox() {
    use crate::contract::permissions::OnRequest;
    use crate::provider::{hook::HookCommand, plan};

    let rig = Rig::new();
    for (mode, guard, asks, bash_asks) in [
        (Mode::Read, false, true, true),
        (Mode::Write, false, true, true),
        (Mode::Read, true, true, false),
        (Mode::Write, true, true, false),
        (Mode::Build, false, true, false),
        (Mode::Full, false, true, false),
        (Mode::Write, false, false, false),
        (Mode::Build, false, false, false),
    ] {
        let mut request = rig.request(Provider::Claude, "fixture");
        request.permissions.mode = mode;
        if asks {
            request.permissions.on_request = OnRequest::Ask;
        }
        let mut options = rig.options(Provider::Claude);
        if guard {
            options.hook = Some(HookCommand::new("/bin/false", Vec::<String>::new()));
            request.permissions.commands.push("ls".into());
        }
        std::fs::write(
            rig.config.join("settings.json"),
            r#"{"permissions":{"deny":["WebSearch"],"ask":["ImportedRule"]}}"#,
        )
        .unwrap();

        let plan = plan::build(&request, &options).unwrap();
        let at = plan
            .args
            .iter()
            .position(|arg| arg == "--settings")
            .unwrap();
        let settings: serde_json::Value = serde_json::from_str(&plan.args[at + 1]).unwrap();
        let expected = if bash_asks {
            serde_json::json!(["Bash", "WebFetch", "WebSearch"])
        } else if asks && mode != Mode::Full {
            serde_json::json!(["WebFetch", "WebSearch"])
        } else {
            serde_json::Value::Null
        };
        assert_eq!(
            settings["permissions"]["ask"], expected,
            "{mode:?}, guard={guard}"
        );
        assert_eq!(
            settings["permissions"]["deny"],
            serde_json::json!(["WebSearch"])
        );
        assert_eq!(settings["hooks"].is_object(), guard);
        assert_eq!(settings["sandbox"].is_object(), mode == Mode::Build);
        if matches!(mode, Mode::Write | Mode::Build) {
            assert!(plan.notes.iter().any(|note| if asks {
                note.contains("NotebookEdit is offered") && note.contains("Edit allow rule")
            } else {
                note.contains("NotebookEdit is not granted")
            }));
        }
    }
}

#[tokio::test]
async fn codex_ask_is_refused_before_executable_lookup_or_start() {
    use crate::contract::permissions::OnRequest;

    let rig = Rig::new();
    let mut request = rig.request(Provider::Codex, "fixture");
    request.permissions.on_request = OnRequest::Ask;
    let mut options = rig.options(Provider::Codex);
    options.executable = Some(rig.work.join("absent"));
    let result = crate::provider::run(request, options).await;
    assert_eq!(result.outcome, crate::contract::outcome::Outcome::Refused);
    let error = result.error.unwrap();
    assert_eq!(error.kind, "permissions_not_supported");
    assert!(error.message.contains("codex exec"));
    assert!(result.timing.started_at.is_none());
    assert!(rig.argv().is_empty());
}
