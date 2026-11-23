//! Enforcement end to end, through `run`, against fakes that honour their
//! arguments.
//!
//! The fake Claude Code reads `--tools`, `--allowedTools` and `--settings` and
//! allows or denies a scripted tool call by them, running the configured hook
//! command with the documented hook input. The fake Codex reads
//! `-c sandbox_mode=...` and lets a write through only where that sandbox
//! would. These tests prove delega's plumbing (the arguments it produces, the
//! hook it installs, the denials it records) against the CLI behaviour the fakes
//! assume. They do not prove the real CLIs behave that way.

use super::{Rig, run_checked};
use crate::contract::outcome::Outcome;
use crate::contract::permissions::Mode;
use crate::contract::request::Provider;
use crate::contract::result::RunResult;

async fn claude(
    rig: &Rig,
    mode: Mode,
    scenario: String,
    edit: impl FnOnce(&mut crate::contract::request::Request),
) -> RunResult {
    let mut request = rig.request(Provider::Claude, &scenario);
    request.permissions.mode = mode;
    edit(&mut request);
    let mut options = rig.options(Provider::Claude);
    options.hook = Some(Rig::guard_hook());
    let result = run_checked(request, options).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    result
}

fn denied(result: &RunResult) -> Vec<&str> {
    result
        .permissions
        .denied
        .iter()
        .map(|d| d.request.as_str())
        .collect()
}

#[tokio::test]
async fn claude_denies_a_write_in_read_mode_and_records_it() {
    let rig = Rig::new();
    let target = rig.work.join("new.txt");
    let result = claude(
        &rig,
        Mode::Read,
        format!("tool-write:{}", target.display()),
        |_| {},
    )
    .await;
    assert!(!target.exists(), "the write must not happen");
    assert_eq!(denied(&result), [format!("Write: {}", target.display())]);
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn claude_allows_a_write_inside_cwd_and_denies_one_outside_in_write_mode() {
    let rig = Rig::new();
    let inside = rig.work.join("inside.txt");
    let result = claude(
        &rig,
        Mode::Write,
        format!("tool-write:{}", inside.display()),
        |_| {},
    )
    .await;
    assert!(inside.exists());
    assert!(denied(&result).is_empty(), "{:?}", denied(&result));

    let rig = Rig::new();
    let outside = rig.home.join("outside.txt");
    let result = claude(
        &rig,
        Mode::Write,
        format!("tool-write:{}", outside.display()),
        |_| {},
    )
    .await;
    assert!(
        !outside.exists(),
        "a write outside the working directory must not happen"
    );
    assert_eq!(denied(&result), [format!("Write: {}", outside.display())]);
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn claude_denies_a_write_into_an_extra_readable_directory() {
    let rig = Rig::new();
    let extra = rig.home.join("docs");
    std::fs::create_dir(&extra).unwrap();
    let extra = std::fs::canonicalize(extra).unwrap();
    let target = extra.join("x.txt");
    let result = claude(
        &rig,
        Mode::Write,
        format!("tool-write:{}", target.display()),
        |r| {
            r.permissions.directories = vec![extra.clone()];
        },
    )
    .await;
    assert!(!target.exists());
    assert_eq!(denied(&result).len(), 1);
}

#[tokio::test]
async fn claude_reads_the_working_directory_and_listed_directories_but_not_elsewhere() {
    let rig = Rig::new();
    let extra = std::fs::canonicalize(&rig.config).unwrap();
    for (path, listed, expect_denied) in [
        (rig.work.join("a.txt"), false, false),
        (extra.join("b.txt"), true, false),
        (extra.join("b.txt"), false, true),
        (rig.home.join("c.txt"), true, true),
    ] {
        let result = claude(
            &rig,
            Mode::Read,
            format!("tool-read:{}", path.display()),
            |r| {
                if listed {
                    r.permissions.directories = vec![extra.clone()];
                }
            },
        )
        .await;
        assert_eq!(
            !denied(&result).is_empty(),
            expect_denied,
            "{} listed={listed}",
            path.display()
        );
    }
}

#[tokio::test]
async fn claude_grep_and_glob_follow_the_scoped_read_rules_in_read_write_and_build_modes() {
    let rig = Rig::new();
    let extra = std::fs::canonicalize(&rig.config).unwrap();
    for mode in [Mode::Read, Mode::Write, Mode::Build] {
        for tool in ["grep", "glob"] {
            for (path, listed, expect_denied) in [
                (rig.work.join("src"), false, false),
                (extra.join("docs"), true, false),
                (extra.join("docs"), false, true),
                (rig.home.join("elsewhere"), true, true),
            ] {
                let result = claude(&rig, mode, format!("tool-{tool}:{}", path.display()), |r| {
                    if listed {
                        r.permissions.directories = vec![extra.clone()];
                    }
                })
                .await;
                let name = if tool == "grep" { "Grep" } else { "Glob" };
                let want = if expect_denied {
                    vec![format!("{name}: {}", path.display())]
                } else {
                    Vec::new()
                };
                assert_eq!(
                    denied(&result),
                    want,
                    "{mode:?} {tool} {} listed={listed}",
                    path.display()
                );
            }
        }
    }
}

#[tokio::test]
async fn claude_runs_a_listed_command_through_the_hook_and_denies_an_unlisted_one() {
    let rig = Rig::new();
    let listed = claude(&rig, Mode::Read, "tool-bash:cargo test".to_string(), |r| {
        r.permissions.commands = vec!["cargo test".into(), "git status".into()];
    })
    .await;
    assert_eq!(listed.text, "ran: cargo test");
    assert!(denied(&listed).is_empty());
    assert_eq!(
        std::fs::read_to_string(rig.record.join("hook"))
            .unwrap()
            .trim(),
        "0",
        "the fake ran the configured hook, which allowed the command"
    );

    for command in [
        "rm -rf x",
        "cargo test --release",
        "cargo test; rm x",
        "git",
    ] {
        let rig = Rig::new();
        let result = claude(&rig, Mode::Read, format!("tool-bash:{command}"), |r| {
            r.permissions.commands = vec!["cargo test".into()];
        })
        .await;
        assert_eq!(result.text, "could not run it", "{command}");
        assert_eq!(denied(&result), [format!("Bash: {command}")], "{command}");
    }
}

#[tokio::test]
async fn claude_denies_every_command_when_none_are_listed_or_no_hook_is_configured() {
    let rig = Rig::new();
    let result = claude(&rig, Mode::Read, "tool-bash:ls".to_string(), |_| {}).await;
    assert_eq!(denied(&result), ["Bash: ls"]);

    let rig = Rig::new();
    let mut request = rig.request(Provider::Claude, "tool-bash:ls");
    request.permissions.commands = vec!["ls".into()];
    let result = run_checked(request, rig.options(Provider::Claude)).await;
    assert_eq!(
        denied(&result),
        ["Bash: ls"],
        "no hook, so commands are not granted"
    );
}

#[tokio::test]
async fn claude_full_mode_allows_writes_and_commands_anywhere() {
    let rig = Rig::new();
    let outside = rig.home.join("anywhere.txt");
    let result = claude(
        &rig,
        Mode::Full,
        format!("tool-write:{}", outside.display()),
        |_| {},
    )
    .await;
    assert!(outside.exists());
    assert!(denied(&result).is_empty());
    let result = claude(
        &rig,
        Mode::Full,
        "tool-bash:anything at all".to_string(),
        |_| {},
    )
    .await;
    assert_eq!(result.text, "ran: anything at all");
}

async fn codex(rig: &Rig, mode: Mode, scenario: String) -> RunResult {
    let mut request = rig.request(Provider::Codex, &scenario);
    request.permissions.mode = mode;
    let result = run_checked(request, rig.options(Provider::Codex)).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    result
}

#[tokio::test]
async fn codex_read_only_sandbox_blocks_writes_but_not_listed_or_unlisted_commands() {
    let rig = Rig::new();
    let target = rig.work.join("new.txt");
    let result = codex(&rig, Mode::Read, format!("tool-write:{}", target.display())).await;
    assert!(!target.exists(), "the sandbox blocks the write");
    // Codex reports a blocked write as a failed command, not as a denial.
    assert!(result.permissions.denied.is_empty());
    assert!(
        result
            .provider_notes
            .iter()
            .any(|n| n.contains("not listed in permissions.denied"))
    );

    // No command was granted, and a command still runs: this is the documented
    // gap between the request and what the sandbox enforces.
    let result = codex(&rig, Mode::Read, "tool-command:cat /etc/hosts".to_string()).await;
    assert_eq!(result.text, "ran: cat /etc/hosts");
    assert!(
        result
            .provider_notes
            .iter()
            .any(|n| n.contains("wider than requested"))
    );
}

#[tokio::test]
async fn codex_workspace_write_sandbox_allows_the_working_directory_only() {
    let rig = Rig::new();
    let inside = rig.work.join("inside.txt");
    codex(
        &rig,
        Mode::Write,
        format!("tool-write:{}", inside.display()),
    )
    .await;
    assert!(inside.exists());
    let outside = rig.home.join("outside.txt");
    codex(
        &rig,
        Mode::Write,
        format!("tool-write:{}", outside.display()),
    )
    .await;
    assert!(!outside.exists());
}

#[tokio::test]
async fn codex_full_mode_has_no_sandbox() {
    let rig = Rig::new();
    let outside = rig.home.join("anywhere.txt");
    codex(
        &rig,
        Mode::Full,
        format!("tool-write:{}", outside.display()),
    )
    .await;
    assert!(outside.exists());
}

/// A CLI that splits `--allowedTools` on commas and all whitespace, ignoring
/// parentheses, reads a multi-word command rule as fragments. delega accepts
/// the input, so the naive reading must never grant more than the real one.
#[tokio::test]
async fn a_naive_whitespace_reading_of_the_rules_grants_nothing_extra() {
    let naive = |rig: &Rig| {
        let mut options = rig.options(Provider::Claude);
        options.hook = Some(Rig::guard_hook());
        options.env_set.push(("FAKE_SPLIT".into(), "naive".into()));
        options
    };
    let rig = Rig::new();
    let outside = rig.home.join("x.txt");
    let mut request = rig.request(
        Provider::Claude,
        &format!("tool-write:{}", outside.display()),
    );
    request.permissions.commands = vec!["cargo test".into(), "git status".into()];
    let result = run_checked(request, naive(&rig)).await;
    assert!(!outside.exists());
    assert_eq!(denied(&result).len(), 1);

    // Writing in read mode is denied under the naive reading as well.
    let rig = Rig::new();
    let inside = rig.work.join("in.txt");
    let mut request = rig.request(
        Provider::Claude,
        &format!("tool-write:{}", inside.display()),
    );
    request.permissions.commands = vec!["cargo test".into()];
    let result = run_checked(request, naive(&rig)).await;
    assert!(!inside.exists());
    assert_eq!(denied(&result).len(), 1);

    // A working directory with a space is accepted; read under the naive reading
    // fails closed (the rule is split) and never reads wider.
    let rig = Rig::new();
    let spaced = rig.work.join("my dir");
    std::fs::create_dir(&spaced).unwrap();
    let mut request = rig.request(
        Provider::Claude,
        &format!("tool-read:{}/f.txt", spaced.display()),
    );
    request.cwd = Some(spaced.clone());
    let result = run_checked(request, naive(&rig)).await;
    assert_eq!(denied(&result).len(), 1);
}
