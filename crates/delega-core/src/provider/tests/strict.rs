//! `permissions.strict`: a run the provider cannot confine as stated is refused
//! before start, with each unenforceable part named.

use super::{Rig, run_checked};
use crate::contract::outcome::Outcome;
use crate::contract::permissions::Mode;
use crate::contract::request::{Provider, Request};
use crate::provider::{HookCommand, ProviderOptions, dry_run};

fn options(rig: &Rig, provider: Provider, hook: bool) -> ProviderOptions {
    let mut options = rig.options(provider);
    options.hook = hook.then(|| HookCommand::new("/opt/delega/hook", ["permission-hook"]));
    options
}

fn request(rig: &Rig, provider: Provider, mode: Mode, commands: &[&str], strict: bool) -> Request {
    let mut request = rig.request(provider, "p");
    request.permissions.mode = mode;
    request.permissions.commands = commands.iter().map(ToString::to_string).collect();
    request.permissions.strict = strict;
    request
}

fn refusal(rig: &Rig, request: &Request, hook: bool) -> crate::provider::Refusal {
    match dry_run(request, &options(rig, request.provider, hook)) {
        Ok(_) => panic!("expected a refusal"),
        Err(refusal) => refusal,
    }
}

#[test]
fn codex_read_with_strict_names_the_read_confinement_and_the_command_case() {
    let rig = Rig::new();
    let r = refusal(
        &rig,
        &request(&rig, Provider::Codex, Mode::Read, &[], true),
        true,
    );
    assert_eq!(r.kind, "permissions_not_enforceable");
    assert!(
        r.message
            .starts_with("permissions.strict is set and Codex cannot enforce"),
        "{}",
        r.message
    );
    assert!(
        r.message.contains(
            "reads are not confined to the working directory and permissions.directories"
        ),
        "{}",
        r.message
    );
    assert!(
        r.message
            .contains("commands that need no write access and no network access can run"),
        "{}",
        r.message
    );
    assert!(!r.message.contains("has no effect"), "{}", r.message);
}

#[test]
fn codex_write_with_strict_names_the_same_two_parts() {
    let rig = Rig::new();
    let r = refusal(
        &rig,
        &request(&rig, Provider::Codex, Mode::Write, &[], true),
        true,
    );
    assert_eq!(r.kind, "permissions_not_enforceable");
    assert!(
        r.message.contains("reads are not confined"),
        "{}",
        r.message
    );
    assert!(
        r.message.contains("commands that need no write access"),
        "{}",
        r.message
    );
}

#[test]
fn codex_with_commands_and_strict_names_commands() {
    let rig = Rig::new();
    for mode in [Mode::Read, Mode::Write] {
        let r = refusal(
            &rig,
            &request(&rig, Provider::Codex, mode, &["cargo test"], true),
            true,
        );
        assert_eq!(r.kind, "permissions_not_enforceable");
        assert!(
            r.message.contains("permissions.commands has no effect"),
            "{}",
            r.message
        );
    }
}

#[test]
fn full_mode_with_strict_runs_on_both_providers() {
    let rig = Rig::new();
    for provider in [Provider::Claude, Provider::Codex] {
        let request = request(&rig, provider, Mode::Full, &[], true);
        dry_run(&request, &options(&rig, provider, false)).expect("full asks for nothing confined");
    }
}

#[test]
fn claude_with_commands_and_the_hook_runs_under_strict() {
    let rig = Rig::new();
    let request = request(&rig, Provider::Claude, Mode::Read, &["cargo test"], true);
    let plan = dry_run(&request, &options(&rig, Provider::Claude, true)).expect("runs");
    assert_eq!(plan.mapping.commands, vec!["cargo test".to_string()]);
}

#[test]
fn claude_with_commands_and_no_hook_is_refused_under_strict() {
    let rig = Rig::new();
    let r = refusal(
        &rig,
        &request(&rig, Provider::Claude, Mode::Read, &["cargo test"], true),
        false,
    );
    assert_eq!(r.kind, "permissions_not_enforceable");
    assert!(
        r.message
            .starts_with("permissions.strict is set and Claude Code cannot enforce"),
        "{}",
        r.message
    );
    assert!(
        r.message
            .contains("permissions.commands cannot be granted exactly because no command guard"),
        "{}",
        r.message
    );
}

#[test]
fn claude_without_commands_runs_under_strict_with_no_hook() {
    let rig = Rig::new();
    for mode in [Mode::Read, Mode::Write] {
        let request = request(&rig, Provider::Claude, mode, &[], true);
        dry_run(&request, &options(&rig, Provider::Claude, false)).expect("runs");
    }
}

#[test]
fn without_strict_every_case_runs_with_its_note() {
    let rig = Rig::new();
    for mode in [Mode::Read, Mode::Write] {
        let plan = dry_run(
            &request(&rig, Provider::Codex, mode, &["cargo test"], false),
            &options(&rig, Provider::Codex, true),
        )
        .expect("runs");
        assert!(
            plan.notes
                .iter()
                .any(|n| n.starts_with("wider than requested"))
        );
    }
    let plan = dry_run(
        &request(&rig, Provider::Claude, Mode::Read, &["cargo test"], false),
        &options(&rig, Provider::Claude, false),
    )
    .expect("runs");
    assert!(
        plan.notes
            .iter()
            .any(|n| n.starts_with("permissions.commands is not granted"))
    );
}

#[tokio::test]
async fn a_strict_refusal_starts_no_provider_process() {
    let rig = Rig::new();
    let request = request(&rig, Provider::Codex, Mode::Read, &[], true);
    let result = run_checked(request, options(&rig, Provider::Codex, true)).await;
    assert_eq!(result.outcome, Outcome::Refused, "{result:#?}");
    assert_eq!(
        result.error.expect("error").kind,
        "permissions_not_enforceable"
    );
    let started = std::fs::read_dir(&rig.record).expect("record").count();
    assert_eq!(started, 0, "the fake recorded a start");
    rig.assert_nothing_left().await;
}
