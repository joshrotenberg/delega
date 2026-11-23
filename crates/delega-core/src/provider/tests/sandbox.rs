//! Doctor's check of the tools Claude Code's sandbox needs for `build` runs.
//!
//! Every test builds its own `PATH` from temporary directories and a temporary
//! home, and no test starts a sandbox tool: doctor only looks for the files.

use std::ffi::{OsStr, OsString};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use super::Rig;
use crate::contract::request::Provider;
use crate::provider::doctor::{self, Blocks, SANDBOX_UNAVAILABLE, SandboxCheck};

/// Put an executable file called `name` in `dir`. It is never run.
fn tool(dir: &Path, name: &str) {
    let path = dir.join(name);
    std::fs::write(&path, "#!/bin/sh\nexit 1\n").expect("write tool");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

fn path_of(dirs: &[&Path]) -> OsString {
    std::env::join_paths(dirs).expect("join PATH")
}

#[test]
fn linux_with_bwrap_and_socat_on_path_reports_the_sandbox_available() {
    let a = tempfile::tempdir().expect("tempdir");
    let b = tempfile::tempdir().expect("tempdir");
    tool(a.path(), "bwrap");
    tool(b.path(), "socat");
    let check = SandboxCheck::of("linux", Some(&path_of(&[a.path(), b.path()])));
    assert!(check.available, "{check:?}");
    assert_eq!(check.requires, ["bwrap", "socat"]);
    assert!(check.missing.is_empty());
    assert_eq!(check.problem(), None);
}

#[test]
fn linux_without_socat_names_it_and_stops_only_build_runs() {
    let dir = tempfile::tempdir().expect("tempdir");
    tool(dir.path(), "bwrap");
    let check = SandboxCheck::of("linux", Some(&path_of(&[dir.path()])));
    assert!(!check.available);
    assert_eq!(check.missing, ["socat"]);
    let problem = check.problem().expect("a problem");
    assert_eq!(problem.kind, SANDBOX_UNAVAILABLE);
    assert_eq!(problem.blocks, Blocks::Build);
    assert!(
        problem.message.contains("socat was not found")
            && problem.message.contains("build run")
            && problem.message.contains("install socat"),
        "{}",
        problem.message
    );
}

#[test]
fn linux_without_either_tool_names_both_and_the_bubblewrap_package() {
    let dir = tempfile::tempdir().expect("tempdir");
    let check = SandboxCheck::of("linux", Some(&path_of(&[dir.path()])));
    assert_eq!(check.missing, ["bwrap", "socat"]);
    let message = check.problem().expect("a problem").message;
    assert!(
        message.contains("bwrap and socat were not found")
            && message.contains("install bubblewrap and socat"),
        "{message}"
    );
    let none = SandboxCheck::of("linux", None);
    assert_eq!(none.missing, ["bwrap", "socat"]);
}

#[test]
fn a_tool_that_is_not_executable_or_only_on_a_relative_entry_counts_as_missing() {
    let dir = tempfile::tempdir().expect("tempdir");
    tool(dir.path(), "bwrap");
    let socat = dir.path().join("socat");
    std::fs::write(&socat, "").expect("write");
    std::fs::set_permissions(&socat, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    let check = SandboxCheck::of("linux", Some(&path_of(&[dir.path()])));
    assert_eq!(check.missing, ["socat"]);

    let relative = SandboxCheck::of("linux", Some(OsStr::new("relative/bin::")));
    assert_eq!(relative.missing, ["bwrap", "socat"]);
}

#[test]
fn macos_needs_no_tool_and_reports_the_sandbox_available() {
    let check = SandboxCheck::of("macos", Some(OsStr::new("")));
    assert!(check.available);
    assert!(check.requires.is_empty() && check.missing.is_empty());
    assert_eq!(check.problem(), None);
}

#[test]
fn another_system_has_no_sandbox_and_stops_build_runs() {
    let check = SandboxCheck::of("freebsd", None);
    assert!(!check.available);
    let problem = check.problem().expect("a problem");
    assert_eq!(problem.blocks, Blocks::Build);
    assert!(
        problem.message.contains("no sandbox"),
        "{}",
        problem.message
    );
}

/// Doctor's check of Claude with the fake and `path` as the caller's `PATH`.
async fn claude_check(rig: &Rig, path: OsString) -> (doctor::ProviderCheck, Vec<doctor::Problem>) {
    let mut options = rig.options(Provider::Claude);
    options.parent_env = Some(vec![
        (OsString::from("PATH"), path),
        (OsString::from("HOME"), rig.home.clone().into_os_string()),
    ]);
    doctor::check(Provider::Claude, &options)
        .await
        .expect("doctor check")
}

#[tokio::test]
async fn doctor_looks_for_the_tools_on_the_worker_path_and_keeps_claude_ready() {
    let rig = Rig::new();
    let tools = tempfile::tempdir().expect("tempdir");
    tool(tools.path(), "bwrap");
    tool(tools.path(), "socat");
    let path = path_of(&[tools.path(), Path::new("/usr/bin"), Path::new("/bin")]);
    let (found, problems) = claude_check(&rig, path).await;
    let sandbox = found.sandbox.expect("a sandbox entry for Claude");
    assert_eq!(sandbox.system, std::env::consts::OS);
    assert!(sandbox.available, "{sandbox:?}");
    assert!(problems.iter().all(|p| p.kind != SANDBOX_UNAVAILABLE));
    assert!(found.ready, "{problems:?}");
}

#[tokio::test]
async fn missing_tools_stop_only_build_runs_and_leave_claude_ready() {
    let rig = Rig::new();
    let empty = tempfile::tempdir().expect("tempdir");
    let (found, problems) = claude_check(&rig, path_of(&[empty.path()])).await;
    let sandbox = found.sandbox.expect("a sandbox entry for Claude");
    let reported: Vec<_> = problems
        .iter()
        .filter(|p| p.kind == SANDBOX_UNAVAILABLE)
        .collect();
    if cfg!(target_os = "linux") {
        assert_eq!(sandbox.missing, ["bwrap", "socat"]);
        assert_eq!(reported.len(), 1, "{problems:?}");
        assert_eq!(reported[0].blocks, Blocks::Build);
    } else {
        assert!(reported.is_empty(), "{problems:?}");
    }
    assert!(
        !problems.iter().any(|p| p.blocks == Blocks::Run),
        "{problems:?}"
    );
    assert!(found.ready);
}

#[tokio::test]
async fn codex_gets_no_sandbox_entry() {
    let rig = Rig::new();
    let (found, problems) = doctor::check(Provider::Codex, &rig.options(Provider::Codex))
        .await
        .expect("doctor check");
    assert_eq!(found.sandbox, None);
    assert!(problems.iter().all(|p| p.kind != SANDBOX_UNAVAILABLE));
}
