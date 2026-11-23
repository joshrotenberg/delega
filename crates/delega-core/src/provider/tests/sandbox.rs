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

/// A working directory no tool is in, for checks whose `PATH` is absolute.
fn nowhere() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

#[test]
fn linux_with_bwrap_and_socat_on_path_meets_the_checked_requirements() {
    let a = tempfile::tempdir().expect("tempdir");
    let b = tempfile::tempdir().expect("tempdir");
    tool(a.path(), "bwrap");
    tool(b.path(), "socat");
    let cwd = nowhere();
    let check = SandboxCheck::of("linux", Some(&path_of(&[a.path(), b.path()])), cwd.path());
    assert!(check.requirements_met, "{check:?}");
    assert_eq!(check.requires, ["bwrap", "socat"]);
    assert!(check.missing.is_empty());
    assert_eq!(check.problem(), None);
}

#[test]
fn linux_without_socat_names_it_and_stops_only_build_runs() {
    let dir = tempfile::tempdir().expect("tempdir");
    tool(dir.path(), "bwrap");
    let cwd = nowhere();
    let check = SandboxCheck::of("linux", Some(&path_of(&[dir.path()])), cwd.path());
    assert!(!check.requirements_met);
    assert_eq!(check.missing, ["socat"]);
    let problem = check.problem().expect("a problem");
    assert_eq!(problem.kind, SANDBOX_UNAVAILABLE);
    assert_eq!(problem.blocks, Blocks::Build);
    for expected in [
        "socat not found on PATH",
        "usually fail at startup",
        "Install socat",
        "does not check other causes",
    ] {
        assert!(
            problem.message.contains(expected),
            "{expected}\n{}",
            problem.message
        );
    }
}

#[test]
fn linux_without_either_tool_names_both_and_the_bubblewrap_package() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = nowhere();
    let check = SandboxCheck::of("linux", Some(&path_of(&[dir.path()])), cwd.path());
    assert_eq!(check.missing, ["bwrap", "socat"]);
    let message = check.problem().expect("a problem").message;
    assert!(
        message.contains("bwrap and socat not found on PATH")
            && message.contains("Install bubblewrap and socat"),
        "{message}"
    );
    let none = SandboxCheck::of("linux", None, cwd.path());
    assert_eq!(none.missing, ["bwrap", "socat"]);
}

#[test]
fn a_tool_file_that_is_not_executable_counts_as_missing() {
    let dir = tempfile::tempdir().expect("tempdir");
    tool(dir.path(), "bwrap");
    let socat = dir.path().join("socat");
    std::fs::write(&socat, "").expect("write");
    std::fs::set_permissions(&socat, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    let cwd = nowhere();
    let check = SandboxCheck::of("linux", Some(&path_of(&[dir.path()])), cwd.path());
    assert_eq!(check.missing, ["socat"]);
}

#[test]
fn relative_and_empty_path_entries_are_taken_from_the_working_directory() {
    // A `which` running in the worker's directory finds tools on a relative
    // entry, so doctor must too, or it reports tools missing that Claude finds.
    let cwd = tempfile::tempdir().expect("tempdir");
    let bin = cwd.path().join("bin");
    std::fs::create_dir(&bin).expect("mkdir");
    tool(&bin, "bwrap");
    tool(&bin, "socat");
    let relative = SandboxCheck::of("linux", Some(OsStr::new("bin")), cwd.path());
    assert!(relative.requirements_met, "{relative:?}");

    tool(cwd.path(), "bwrap");
    let empty = SandboxCheck::of("linux", Some(OsStr::new("::bin")), cwd.path());
    assert!(empty.missing.is_empty(), "{empty:?}");
    let only_empty = SandboxCheck::of("linux", Some(OsStr::new(":")), cwd.path());
    assert_eq!(only_empty.missing, ["socat"]);

    // The same relative entry from another directory finds nothing.
    let elsewhere = nowhere();
    let other = SandboxCheck::of("linux", Some(OsStr::new("bin")), elsewhere.path());
    assert_eq!(other.missing, ["bwrap", "socat"]);
}

#[test]
fn macos_needs_no_tool_and_meets_the_checked_requirements() {
    let cwd = nowhere();
    let check = SandboxCheck::of("macos", Some(OsStr::new("")), cwd.path());
    assert!(check.requirements_met);
    assert!(check.requires.is_empty() && check.missing.is_empty());
    assert_eq!(check.problem(), None);
}

#[test]
fn another_system_has_no_sandbox_and_stops_build_runs() {
    let cwd = nowhere();
    let check = SandboxCheck::of("freebsd", None, cwd.path());
    assert!(!check.requirements_met);
    let problem = check.problem().expect("a problem");
    assert_eq!(problem.blocks, Blocks::Build);
    assert!(
        problem.message.contains("no sandbox"),
        "{}",
        problem.message
    );
}

/// Options for the fake Claude with `path` as the caller's `PATH`.
fn claude_options(rig: &Rig, path: OsString) -> crate::provider::ProviderOptions {
    let mut options = rig.options(Provider::Claude);
    options.parent_env = Some(vec![
        (OsString::from("PATH"), path),
        (OsString::from("HOME"), rig.home.clone().into_os_string()),
    ]);
    options
}

#[tokio::test]
async fn doctor_looks_for_the_tools_on_the_worker_path_and_keeps_claude_ready() {
    let rig = Rig::new();
    let tools = tempfile::tempdir().expect("tempdir");
    tool(tools.path(), "bwrap");
    tool(tools.path(), "socat");
    let path = path_of(&[tools.path(), Path::new("/usr/bin"), Path::new("/bin")]);
    let (found, problems) =
        doctor::check_on(Provider::Claude, &claude_options(&rig, path), "linux")
            .await
            .expect("doctor check");
    let sandbox = found.sandbox.expect("a sandbox entry for Claude");
    assert_eq!(sandbox.system, "linux");
    assert!(sandbox.requirements_met, "{sandbox:?}");
    assert!(problems.iter().all(|p| p.kind != SANDBOX_UNAVAILABLE));
    assert!(found.ready, "{problems:?}");
}

#[tokio::test]
async fn missing_tools_stop_only_build_runs_and_leave_claude_ready() {
    let rig = Rig::new();
    let empty = tempfile::tempdir().expect("tempdir");
    let options = claude_options(&rig, path_of(&[empty.path()]));
    let (found, problems) = doctor::check_on(Provider::Claude, &options, "linux")
        .await
        .expect("doctor check");
    let sandbox = found.sandbox.expect("a sandbox entry for Claude");
    assert_eq!(sandbox.missing, ["bwrap", "socat"]);
    let reported: Vec<_> = problems
        .iter()
        .filter(|p| p.kind == SANDBOX_UNAVAILABLE)
        .collect();
    assert_eq!(reported.len(), 1, "{problems:?}");
    assert_eq!(reported[0].blocks, Blocks::Build);
    assert!(
        !problems.iter().any(|p| p.blocks == Blocks::Run),
        "{problems:?}"
    );
    assert!(found.ready);
}

#[tokio::test]
async fn codex_gets_no_sandbox_entry() {
    let rig = Rig::new();
    let (found, problems) =
        doctor::check_on(Provider::Codex, &rig.options(Provider::Codex), "linux")
            .await
            .expect("doctor check");
    assert_eq!(found.sandbox, None);
    assert!(problems.iter().all(|p| p.kind != SANDBOX_UNAVAILABLE));
}

/// The whole report, with the fake Claude as the only provider found and the
/// sandbox tools missing, is ready and exits 0: a problem that stops only
/// build runs must not make the report say no run can go ahead.
#[tokio::test]
async fn a_missing_sandbox_tool_with_claude_the_only_ready_provider_still_exits_zero() {
    let rig = Rig::new();
    let empty = tempfile::tempdir().expect("tempdir");
    let mut options = claude_options(&rig, path_of(&[empty.path()]));
    // Only Claude's executable is set; Codex is looked for on the empty PATH
    // and not found.
    options.executable = None;
    options.parent_env.as_mut().expect("parent env").push((
        OsString::from("DELEGA_CLAUDE_BIN"),
        Rig::fake(Provider::Claude).into(),
    ));
    let status = crate::config::Status {
        location: None,
        found: false,
        result: Ok(None),
        applied: None,
    };
    let system = options.system_paths.clone();
    // Process inspection has a deadline a loaded machine can miss; try again,
    // as the CLI tests do, so only a working listing is judged.
    let mut report = None;
    for _ in 0..8 {
        let built = crate::doctor::diagnose_on(&options, &status, &system, "linux")
            .await
            .expect("doctor runs");
        let inspected = built.process_inspection.available;
        report = Some(built);
        if inspected {
            break;
        }
    }
    let report = report.expect("one attempt");
    assert!(report.process_inspection.available, "{report:?}");
    let kinds: Vec<(Option<Provider>, &str, Blocks)> = report
        .problems
        .iter()
        .map(|p| (p.provider, p.kind.as_str(), p.blocks))
        .collect();
    assert!(
        kinds.contains(&(Some(Provider::Claude), SANDBOX_UNAVAILABLE, Blocks::Build)),
        "{kinds:?}"
    );
    assert!(
        kinds.contains(&(Some(Provider::Codex), "provider_not_found", Blocks::Run)),
        "{kinds:?}"
    );
    let claude = &report.providers[0];
    let codex = &report.providers[1];
    assert!(claude.ready && !codex.ready, "{report:?}");
    assert!(report.ready, "{kinds:?}");
    assert_eq!(report.exit_status(), 0);
}
