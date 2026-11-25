//! Refusals before start, executable lookup, version gate and MCP discovery.

use std::ffi::OsString;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;

use super::{Rig, both, run_checked};
use crate::contract::outcome::Outcome;
use crate::contract::request::{Provider, Request};
use crate::contract::result::Cleanup;
use crate::provider::{HookCommand, ProviderOptions, start};

fn hook() -> HookCommand {
    HookCommand::new("/opt/delega/hook", ["permission-hook"])
}

type Case = (&'static str, Box<dyn Fn(&mut Request)>, &'static str);

async fn refused_without_starting(provider: Provider) {
    let rig = Rig::new();
    let cases: Vec<Case> = vec![
        (
            "empty prompt",
            Box::new(|r| r.prompt = "  ".into()),
            "invalid_request",
        ),
        (
            "timeout zero",
            Box::new(|r| r.timeout_secs = 0),
            "invalid_request",
        ),
        (
            "missing cwd",
            Box::new(|r| r.cwd = Some("/nonexistent/delega/work".into())),
            "cwd_unusable",
        ),
        (
            "missing directory",
            Box::new(|r| r.permissions.directories = vec!["/nonexistent/delega/dir".into()]),
            "directory_unusable",
        ),
        (
            "bad native arg",
            Box::new(|r| {
                r.native_args = vec!["--permission-mode".into(), "bypassPermissions".into()]
            }),
            "native_arg_rejected",
        ),
        (
            "bad schema",
            Box::new(|r| r.answer_schema = Some(json!({"type": 12}))),
            "invalid_answer_schema",
        ),
    ];
    for (name, edit, kind) in cases {
        let mut request = rig.request(provider, "success");
        edit(&mut request);
        let result = run_checked(request, rig.options(provider)).await;
        assert_eq!(result.outcome, Outcome::Refused, "{name}: {result:#?}");
        assert_eq!(result.error.expect("error").kind, kind, "{name}");
        assert_eq!(result.timing.started_at, None, "{name}");
        assert_eq!(result.timing.duration_ms, 0, "{name}");
        assert_eq!(result.cleanup, Cleanup::Confirmed, "{name}");
        assert_eq!(result.provider, Some(provider), "{name}");
    }
    assert!(
        rig.pid("pid").is_none(),
        "no provider was started for a refusal"
    );
}
both!(
    refuses_before_start_and_starts_nothing,
    refused_without_starting
);

async fn missing_executable(provider: Provider) {
    let rig = Rig::new();
    let mut options = rig.options(provider);
    options.executable = Some("/nonexistent/delega/provider".into());
    let result = run_checked(rig.request(provider, "success"), options).await;
    assert_eq!(result.outcome, Outcome::Refused);
    let error = result.error.expect("error");
    assert_eq!(error.kind, "provider_not_found");
    assert!(error.message.contains("DELEGA_"), "{}", error.message);

    let mut options = rig.options(provider);
    options.executable = Some("relative/provider".into());
    let result = run_checked(rig.request(provider, "success"), options).await;
    assert_eq!(result.error.expect("error").kind, "provider_not_found");
}
both!(refuses_a_missing_or_relative_executable, missing_executable);

async fn lookup(provider: Provider) {
    use std::ffi::OsString;
    let rig = Rig::new();
    let bin = rig.home.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let name = match provider {
        Provider::Claude => "claude",
        Provider::Codex => "codex",
    };
    symlink(Rig::fake(provider), bin.join(name)).unwrap();
    let variable = crate::provider::program::override_variable(provider);

    // The name on an absolute PATH entry.
    let mut options = rig.options(provider);
    options.executable = None;
    options.parent_env = Some(vec![
        (
            OsString::from("PATH"),
            format!("/usr/bin:{}", bin.display()).into(),
        ),
        (OsString::from("HOME"), rig.home.clone().into_os_string()),
    ]);
    let result = run_checked(rig.request(provider, "success"), options).await;
    assert_eq!(result.outcome, Outcome::Completed, "PATH: {result:#?}");

    // The override variable, ahead of PATH.
    let mut options = rig.options(provider);
    options.executable = None;
    options.parent_env = Some(vec![
        (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
        (
            OsString::from(variable),
            Rig::fake(provider).into_os_string(),
        ),
    ]);
    let result = run_checked(rig.request(provider, "success"), options).await;
    assert_eq!(
        result.outcome,
        Outcome::Completed,
        "override variable: {result:#?}"
    );

    // A relative PATH entry is never searched.
    let mut options = rig.options(provider);
    options.executable = None;
    options.parent_env = Some(vec![(OsString::from("PATH"), OsString::from("bin:"))]);
    let mut request = rig.request(provider, "success");
    request.cwd = Some(rig.home.clone());
    let result = run_checked(request, options).await;
    assert_eq!(result.error.expect("error").kind, "provider_not_found");
}
both!(
    finds_the_executable_by_override_variable_then_absolute_path,
    lookup
);

#[tokio::test]
async fn claude_without_a_hook_does_not_grant_commands_and_says_so() {
    let rig = Rig::new();
    let mut request = rig.request(Provider::Claude, "success");
    request.permissions.commands = vec!["cargo test".into()];
    let result = run_checked(request, rig.options(Provider::Claude)).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    assert!(
        result
            .provider_notes
            .iter()
            .any(|n| n.contains("permissions.commands is not granted")),
        "{:?}",
        result.provider_notes
    );
    let argv = rig.argv();
    let tools = &argv[argv.iter().position(|a| a == "--tools").unwrap() + 1];
    assert!(!tools.contains("Bash"), "{tools}");
    assert!(!argv.join(" ").contains("cargo test"));
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn claude_with_a_hook_and_a_supported_version_installs_the_guard() {
    let rig = Rig::new();
    let mut request = rig.request(Provider::Claude, "success");
    request.permissions.commands = vec!["cargo test".into()];
    let mut options = rig.options(Provider::Claude);
    options.hook = Some(hook());
    let result = run_checked(request, options).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    let argv = rig.argv();
    let tools = &argv[argv.iter().position(|a| a == "--tools").unwrap() + 1];
    assert!(tools.contains("Bash"));
    assert!(argv.iter().any(|a| a.contains("Bash(cargo test)")));
    assert!(
        rig.env_names()
            .contains(&"DELEGA_EXACT_BASH_GRANTS".to_string())
    );
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn claude_older_than_the_hook_interface_is_refused_before_start() {
    for version in ["2.1.100 (Claude Code)", "garbage", "2.1.324"] {
        let rig = Rig::new();
        let mut request = rig.request(Provider::Claude, "success");
        request.permissions.commands = vec!["cargo test".into()];
        let mut options = rig.options(Provider::Claude);
        options.hook = Some(hook());
        options
            .env_set
            .push(("FAKE_VERSION".into(), version.into()));
        let result = run_checked(request, options).await;
        assert_eq!(result.outcome, Outcome::Refused, "{version}: {result:#?}");
        assert_eq!(
            result.error.expect("error").kind,
            "unsupported_provider_version"
        );
        assert_eq!(result.timing.started_at, None);
        assert!(rig.pid("pid").is_none(), "the run never started");
        assert_eq!(result.cleanup, Cleanup::Confirmed);
    }
}

#[tokio::test]
async fn claude_without_commands_does_not_need_a_known_version() {
    let rig = Rig::new();
    let mut options = rig.options(Provider::Claude);
    options
        .env_set
        .push(("FAKE_VERSION".into(), "garbage".into()));
    let result = run_checked(rig.request(Provider::Claude, "success"), options).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    assert_eq!(
        result.provenance.provider_version.as_deref(),
        Some("garbage")
    );
    rig.assert_nothing_left().await;
}

async fn a_credential_in_the_version_line_is_redacted_in_the_result(provider: Provider) {
    let rig = Rig::new();
    let mut options = rig.options(provider);
    options.env_set.push((
        "FAKE_VERSION".into(),
        "9.9.9 key sk-ant-abcdefghijklmnopqrstuvwx".into(),
    ));
    let result = run_checked(rig.request(provider, "success"), options).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    assert_eq!(
        result.provenance.provider_version.as_deref(),
        Some("9.9.9 key [redacted]")
    );
    rig.assert_nothing_left().await;
}
both!(
    version_line_credential_is_redacted,
    a_credential_in_the_version_line_is_redacted_in_the_result
);

#[tokio::test]
async fn a_version_line_is_redacted_before_it_is_cut_so_no_part_of_a_key_is_kept() {
    let rig = Rig::new();
    let mut options = rig.options(Provider::Claude);
    // The key starts 90 characters in and runs across the 100-character cut.
    let line = format!("2.1.400 {} sk-ant-abcdefghijklmnopqrstuvwx", "a".repeat(81));
    options.env_set.push(("FAKE_VERSION".into(), line));
    let result = run_checked(rig.request(Provider::Claude, "success"), options).await;
    let stored = result.provenance.provider_version.expect("version");
    assert!(!stored.contains("sk-ant"), "{stored}");
    assert!(!stored.contains("abcdefghij"), "{stored}");
    assert!(stored.ends_with("[redacted]"), "{stored}");
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn the_hook_version_gate_reads_the_version_line_before_redaction() {
    // An environment value equal to the version text is removed from the
    // stored line; the gate must still read the version the provider printed.
    let rig = Rig::new();
    let mut request = rig.request(Provider::Claude, "success");
    request.permissions.commands = vec!["cargo test".into()];
    let mut options = rig.options(Provider::Claude);
    options.hook = Some(hook());
    options
        .env_set
        .push(("MY_SERVICE_API_TOKEN".into(), "2.1.400".into()));
    let result = run_checked(request, options).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    assert_eq!(
        result.provenance.provider_version.as_deref(),
        Some("[redacted] (Claude Code)")
    );
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn claude_command_with_a_wildcard_is_refused() {
    let rig = Rig::new();
    let mut request = rig.request(Provider::Claude, "success");
    request.permissions.commands = vec!["ls *.rs".into()];
    let mut options = rig.options(Provider::Claude);
    options.hook = Some(hook());
    let result = run_checked(request, options).await;
    assert_eq!(result.outcome, Outcome::Refused, "{result:#?}");
    assert_eq!(result.error.expect("error").kind, "unsupported_command");
}

#[tokio::test]
async fn codex_disables_every_listed_mcp_server_before_the_run() {
    let rig = Rig::new();
    let result = run_checked(
        rig.request(Provider::Codex, "success"),
        rig.options(Provider::Codex),
    )
    .await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    assert_eq!(
        rig.mcp_argv(),
        [
            "--disable",
            "plugins",
            "--disable",
            "apps",
            "--disable",
            "hooks",
            "--disable",
            "multi_agent",
            "mcp",
            "list",
            "--json"
        ]
    );
    let argv = rig.argv();
    let expected = "mcp_servers={\"ambient\"={enabled=false,command=\"__delega_disabled__\"},\
                    \"server.with.dots\"={enabled=false,url=\"http://127.0.0.1:9\"}}";
    assert!(argv.iter().any(|a| a == expected), "{argv:?}");
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn codex_fails_without_starting_when_mcp_discovery_fails() {
    for (mode, expected) in [
        ("bad-json", "invalid JSON"),
        ("failure", "it exited with code 1"),
        ("odd-transport", "cannot be disabled"),
    ] {
        let rig = Rig::new();
        let mut options = rig.options(Provider::Codex);
        options.env_set.push(("FAKE_MCP".into(), mode.into()));
        let result = run_checked(rig.request(Provider::Codex, "success"), options).await;
        assert_eq!(result.outcome, Outcome::Failed, "{mode}: {result:#?}");
        let error = result.error.expect("error");
        assert_eq!(error.kind, "provider_preparation_failed", "{mode}");
        assert!(
            error.message.contains(expected),
            "{mode}: {}",
            error.message
        );
        assert!(rig.pid("pid").is_none(), "{mode}: the run never started");
    }
}

#[tokio::test]
async fn an_interrupt_during_preparation_stops_the_probe_and_ends_interrupted() {
    let rig = Rig::new();
    let mut options = rig.options(Provider::Codex);
    options.env_set.push(("FAKE_MCP".into(), "hang".into()));
    let handle = start(rig.request(Provider::Codex, "success"), options);
    // The probe is running once it has recorded its arguments, which it does
    // before it hangs.
    rig.wait_record("mcp-argv").await;
    handle.interrupt();
    let result = handle.wait().await;
    assert_eq!(result.validate(), Ok(()), "{result:#?}");
    assert_eq!(result.outcome, Outcome::Interrupted, "{result:#?}");
    assert_eq!(result.cleanup, Cleanup::Confirmed, "{result:#?}");
    assert!(rig.pid("pid").is_none());
}

#[tokio::test]
async fn preparation_counts_against_the_time_limit() {
    let rig = Rig::new();
    let mut options = rig.options(Provider::Codex);
    options.env_set.push(("FAKE_MCP".into(), "hang".into()));
    let mut request = rig.request(Provider::Codex, "success");
    request.timeout_secs = 8;
    let handle = start(request, options);
    rig.wait_record("mcp-argv").await;

    // The discovery probe is running. Jump past the shared run budget,
    // then let its real child and the process listing finish on real time.
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(8)).await;
    tokio::time::resume();
    let result = tokio::time::timeout(Duration::from_secs(5), handle.wait())
        .await
        .expect("the preparation deadline settles the run");
    assert_eq!(result.validate(), Ok(()), "{result:#?}");
    assert_eq!(result.outcome, Outcome::TimedOut, "{result:#?}");
    assert_eq!(result.error.as_ref().expect("error").kind, "timed_out");
    assert_eq!(result.cleanup, Cleanup::Confirmed, "{result:#?}");
    assert_eq!(result.timing.started_at, None);
    assert!(rig.pid("pid").is_none());
}

#[tokio::test]
async fn an_interrupt_before_the_run_task_starts_ends_interrupted_without_a_process() {
    for provider in [Provider::Claude, Provider::Codex] {
        let rig = Rig::new();
        let handle = start(rig.request(provider, "success"), rig.options(provider));
        handle.interrupt();
        let result = handle.wait().await;
        assert_eq!(result.validate(), Ok(()), "{result:#?}");
        assert_eq!(result.outcome, Outcome::Interrupted, "{result:#?}");
        assert!(rig.pid("pid").is_none());
    }
}

#[test]
fn cleanup_verdicts_combine_to_confirmed_only_when_both_are() {
    use crate::provider::facts::merge_cleanup;
    let bad = |pid: u32, why: &str| Cleanup::Unconfirmed {
        pids: vec![pid],
        process_groups: vec![pid],
        reason: why.to_string(),
    };
    assert_eq!(
        merge_cleanup(Cleanup::Confirmed, Cleanup::Confirmed),
        Cleanup::Confirmed
    );
    assert_eq!(merge_cleanup(Cleanup::Confirmed, bad(1, "a")), bad(1, "a"));
    assert_eq!(merge_cleanup(bad(2, "b"), Cleanup::Confirmed), bad(2, "b"));
    assert_eq!(
        merge_cleanup(bad(1, "a"), bad(2, "b")),
        Cleanup::Unconfirmed {
            pids: vec![1, 2],
            process_groups: vec![1, 2],
            reason: "a; b".into()
        }
    );
}

#[test]
fn provider_versions_parse_from_the_start_of_the_output() {
    use crate::provider::program::parse_version;
    assert_eq!(parse_version("2.1.325 (Claude Code)"), Some((2, 1, 325)));
    assert_eq!(parse_version("  10.20.30\n"), Some((10, 20, 30)));
    for bad in ["", "2.1", "v2.1.3", "codex-cli 0.9.1", "2.1.x"] {
        assert_eq!(parse_version(bad), None, "{bad}");
    }
}

#[test]
fn a_configured_executable_comes_after_the_override_variable_and_before_path() {
    use crate::provider::program::{override_variable, resolve};
    for provider in [Provider::Claude, Provider::Codex] {
        let fake = Rig::fake(provider);
        let other = PathBuf::from("/bin/sh");
        let variable = override_variable(provider);
        let parent = |value: Option<&str>| {
            let mut env = vec![(OsString::from("PATH"), OsString::from("/nonexistent"))];
            if let Some(value) = value {
                env.push((OsString::from(variable), OsString::from(value)));
            }
            env
        };
        let mut options = ProviderOptions::default();
        match provider {
            Provider::Claude => options.claude_executable = Some(fake.clone()),
            Provider::Codex => options.codex_executable = Some(fake.clone()),
        }

        // Alone, it is used; PATH holds nothing.
        assert_eq!(resolve(provider, &options, &parent(None)).unwrap(), fake);
        // An empty override variable counts as unset.
        assert_eq!(
            resolve(provider, &options, &parent(Some(""))).unwrap(),
            fake
        );
        // The override variable wins over the file.
        assert_eq!(
            resolve(provider, &options, &parent(Some("/bin/sh"))).unwrap(),
            other
        );
        // The executable option wins over both.
        options.executable = Some(PathBuf::from("/bin/ls"));
        assert_eq!(
            resolve(provider, &options, &parent(Some("/bin/sh"))).unwrap(),
            PathBuf::from("/bin/ls")
        );
        // A configured path that is not an executable is refused and says where
        // it came from, without falling back to PATH.
        options.executable = None;
        match provider {
            Provider::Claude => options.claude_executable = Some("/nonexistent/claude".into()),
            Provider::Codex => options.codex_executable = Some("/nonexistent/codex".into()),
        }
        let refusal = resolve(provider, &options, &parent(None)).unwrap_err();
        assert_eq!(refusal.kind, "provider_not_found");
        assert!(
            refusal.message.contains("the configuration file"),
            "{refusal}"
        );
    }
}
