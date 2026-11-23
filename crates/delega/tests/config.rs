//! The user-level configuration file, through `delega run`, `delega doctor` and
//! (for the refusal) `delega mcp`, as a black box against the fake providers.
//!
//! Every test runs the built executable with an empty environment plus a
//! temporary HOME, `XDG_CONFIG_HOME` pointing into it and the fakes as the
//! provider executables, so nothing reads the real configuration, a real home
//! directory or a real provider CLI. The MCP server's use of the file (limits,
//! request defaults) is in `mcp_stdio.rs`.

mod support;

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Output;

use serde_json::Value;
use support::{Guarded, Rig, text};

/// A value no output may contain.
const SECRET: &str = "config-secret-value-4417";

/// Where the rig's `XDG_CONFIG_HOME` puts the file.
fn config_path(rig: &Rig) -> PathBuf {
    rig.home.join("delega").join("config.toml")
}

/// Write the configuration file, private to the user.
fn write_config(rig: &Rig, body: &str) -> PathBuf {
    let path = config_path(rig);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("create directory");
    std::fs::write(&path, body).expect("write config");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    path
}

fn run_json(rig: &Rig, args: &[&str]) -> Output {
    rig.command()
        .args(["run", "--output", "json", "--dry-run"])
        .args(args)
        .guarded_output()
        .expect("run delega")
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

/// The standard arguments of a dry run of `provider` in the rig's directory.
fn dry(rig: &Rig, provider: &str, extra: &[&str]) -> Output {
    let work = rig.work.to_string_lossy().into_owned();
    let mut args = vec!["--provider", provider, "-p", "hello", "-C", &work];
    args.extend_from_slice(extra);
    run_json(rig, &args)
}

fn dry_value(rig: &Rig, provider: &str, extra: &[&str]) -> Value {
    let out = dry(rig, provider, extra);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    json_of(&out)["dry_run"].clone()
}

/// The value after `flag` in the launch arguments.
fn arg_after(plan: &Value, flag: &str) -> Option<String> {
    let args = plan["args"].as_array().expect("args");
    let at = args.iter().position(|a| a == flag)?;
    args.get(at + 1)?.as_str().map(str::to_string)
}

fn names(plan: &Value, group: &str) -> Vec<String> {
    plan["env"][group]
        .as_array()
        .expect("group")
        .iter()
        .map(|v| v.as_str().expect("name").to_string())
        .collect()
}

fn assert_no_secret(out: &Output) {
    for (what, bytes) in [("stdout", &out.stdout), ("stderr", &out.stderr)] {
        assert!(!text(bytes).contains(SECRET), "{what} holds a value");
    }
}

const FULL: &str = r#"
schema_version = 1

[defaults]
provider = "claude"
model = "file-model"
effort = "high"
timeout_secs = 1200

[defaults.permissions]
mode = "write"

[env]
pass = ["CONFIG_PASSED"]

[env.set]
CONFIG_SET = "config-secret-value-4417"
"#;

#[test]
fn an_absent_file_changes_nothing_and_a_file_with_only_its_version_is_the_same() {
    let rig = Rig::new();
    assert!(!config_path(&rig).exists());
    let without = dry(&rig, "claude", &["--env", "CONFIG_PASSED"]);
    assert_eq!(without.status.code(), Some(0));
    write_config(&rig, "schema_version = 1\n");
    let with_empty = dry(&rig, "claude", &["--env", "CONFIG_PASSED"]);
    // The one difference is the report that a file was read: `applied.config`.
    let mut reported = json_of(&with_empty);
    let config = reported["dry_run"]["applied"]
        .as_object_mut()
        .and_then(|applied| applied.remove("config"))
        .expect("the empty file is reported as applied.config");
    assert_eq!(config["path"], config_path(&rig).to_string_lossy().as_ref());
    assert!(json_of(&without)["dry_run"]["applied"]["config"].is_null());
    assert_eq!(json_of(&without), reported);
    assert_eq!(text(&without.stderr), text(&with_empty.stderr));
    // No file at all, and no place to look for one, behaves the same.
    let nowhere = rig
        .command()
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .args([
            "run",
            "--output",
            "json",
            "--dry-run",
            "--provider",
            "claude",
            "-p",
            "hello",
        ])
        .arg("-C")
        .arg(&rig.work)
        .args(["--env", "CONFIG_PASSED"])
        .guarded_output()
        .expect("run delega");
    assert_eq!(nowhere.status.code(), Some(0), "{}", text(&nowhere.stderr));
}

#[test]
fn defaults_apply_when_nothing_else_says_otherwise() {
    let rig = Rig::new();
    write_config(&rig, FULL);
    // No provider flag: the file's provider (claude) is used.
    let out = rig
        .command()
        .args(["run", "--output", "json", "--dry-run", "-p", "hello", "-C"])
        .arg(&rig.work)
        .guarded_output()
        .expect("run delega");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let plan = json_of(&out)["dry_run"].clone();
    assert_eq!(plan["provider"], "claude");
    assert_eq!(arg_after(&plan, "--model").as_deref(), Some("file-model"));
    assert_eq!(arg_after(&plan, "--effort").as_deref(), Some("high"));
    assert_eq!(plan["time_limit_secs"], 1200);
    assert_eq!(plan["mapping"]["mode"], "write");
}

#[test]
fn the_files_environment_is_added_to_the_flags_and_no_value_is_printed() {
    let rig = Rig::new();
    write_config(&rig, FULL);
    let out = rig
        .command()
        .env("CONFIG_PASSED", SECRET)
        .env("FLAG_PASSED", SECRET)
        .args([
            "run",
            "--output",
            "json",
            "--dry-run",
            "--provider",
            "claude",
            "-p",
            "hello",
        ])
        .arg("-C")
        .arg(&rig.work)
        .args(["--env", "FLAG_PASSED", "--env", "FLAG_SET=flag-value-1"])
        .guarded_output()
        .expect("run delega");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let plan = json_of(&out)["dry_run"].clone();
    // Names from the file and from the flags both apply.
    assert_eq!(names(&plan, "named"), ["CONFIG_PASSED", "FLAG_PASSED"]);
    assert_eq!(names(&plan, "set_by_caller"), ["CONFIG_SET", "FLAG_SET"]);
    let env_names: Vec<&str> = plan["env_names"]
        .as_array()
        .expect("names")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for name in ["CONFIG_PASSED", "CONFIG_SET", "FLAG_PASSED", "FLAG_SET"] {
        assert!(env_names.contains(&name), "{name}: {env_names:?}");
    }
    assert_no_secret(&out);
    // The text form lists names only.
    let text_out = rig
        .command()
        .env("CONFIG_PASSED", SECRET)
        .args([
            "run",
            "--dry-run",
            "--provider",
            "claude",
            "-p",
            "hello",
            "-C",
        ])
        .arg(&rig.work)
        .guarded_output()
        .expect("run delega");
    assert_eq!(text_out.status.code(), Some(0));
    assert!(text(&text_out.stdout).contains("named by the caller: CONFIG_PASSED"));
    assert_no_secret(&text_out);
}

#[test]
fn a_real_run_hands_the_worker_the_files_variables_and_prints_no_value() {
    let rig = Rig::new();
    write_config(&rig, FULL);
    let out = rig
        .command()
        .env("CONFIG_PASSED", SECRET)
        .args(rig.run_args("claude", "success", &["--output", "json"]))
        .guarded_output()
        .expect("run delega");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let result = json_of(&out);
    assert_eq!(result["outcome"], "completed", "{result}");
    assert_eq!(result["timing"]["timeout_secs"], 1200);
    assert_eq!(result["permissions"]["mode"], "write");
    let recorded = rig.recorded("env");
    for name in ["CONFIG_PASSED", "CONFIG_SET"] {
        assert!(recorded.lines().any(|l| l == name), "{name}: {recorded}");
    }
    assert_no_secret(&out);
}

#[test]
fn the_files_executable_is_used_unless_the_override_variable_is_set() {
    let rig = Rig::new();
    let program = rig.path("bin-claude");
    std::os::unix::fs::symlink(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fakes/fake-claude.sh"),
        &program,
    )
    .expect("link");
    write_config(
        &rig,
        &format!(
            "schema_version = 1\n[providers.claude]\nexecutable = \"{}\"\n",
            program.display()
        ),
    );
    // Without DELEGA_CLAUDE_BIN, the file's executable is the program.
    let out = rig
        .command()
        .env_remove("DELEGA_CLAUDE_BIN")
        .args([
            "run",
            "--output",
            "json",
            "--dry-run",
            "--provider",
            "claude",
            "-p",
            "x",
            "-C",
        ])
        .arg(&rig.work)
        .guarded_output()
        .expect("run delega");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        json_of(&out)["dry_run"]["program"],
        program.to_string_lossy().as_ref()
    );
    // With it, the variable wins.
    let out = dry(&rig, "claude", &[]);
    let program_used = json_of(&out)["dry_run"]["program"]
        .as_str()
        .expect("program")
        .to_string();
    assert!(program_used.ends_with("fake-claude.sh"), "{program_used}");
    assert_ne!(program_used, program.to_string_lossy());
    // A file path that is not an executable refuses as provider_not_found.
    write_config(
        &rig,
        "schema_version = 1\n[providers.claude]\nexecutable = \"/nonexistent/claude\"\n",
    );
    let out = rig
        .command()
        .env_remove("DELEGA_CLAUDE_BIN")
        .args([
            "run",
            "--output",
            "json",
            "--dry-run",
            "--provider",
            "claude",
            "-p",
            "x",
            "-C",
        ])
        .arg(&rig.work)
        .guarded_output()
        .expect("run delega");
    assert_eq!(out.status.code(), Some(2));
    let result = json_of(&out);
    assert_eq!(result["error"]["kind"], "provider_not_found");
    assert!(
        result["error"]["message"]
            .as_str()
            .expect("message")
            .contains("the configuration file"),
        "{result}"
    );
}

#[test]
fn a_profile_overrides_the_file_and_a_flag_overrides_the_profile() {
    let rig = Rig::new();
    write_config(
        &rig,
        "schema_version = 1\n[defaults]\nmodel = \"file-model\"\ntimeout_secs = 1200\n",
    );
    let profile = rig.file(
        "profile.json",
        r#"{"schema_version": 1, "model": "profile-model", "timeout_secs": 900}"#,
    );
    let profile = profile.to_string_lossy().into_owned();

    // The file alone.
    let plan = dry_value(&rig, "claude", &[]);
    assert_eq!(arg_after(&plan, "--model").as_deref(), Some("file-model"));
    assert_eq!(plan["time_limit_secs"], 1200);
    // A profile over the file.
    let plan = dry_value(&rig, "claude", &["--request", &profile]);
    assert_eq!(
        arg_after(&plan, "--model").as_deref(),
        Some("profile-model")
    );
    assert_eq!(plan["time_limit_secs"], 900);
    // A profile that sets only one of them leaves the other to the file.
    let partial = rig.file(
        "partial.json",
        r#"{"schema_version": 1, "model": "partial-model"}"#,
    );
    let plan = dry_value(&rig, "claude", &["--request", &partial.to_string_lossy()]);
    assert_eq!(
        arg_after(&plan, "--model").as_deref(),
        Some("partial-model")
    );
    assert_eq!(plan["time_limit_secs"], 1200);
    // A flag over both.
    let plan = dry_value(
        &rig,
        "claude",
        &[
            "--request",
            &profile,
            "--model",
            "flag-model",
            "--timeout",
            "30s",
        ],
    );
    assert_eq!(arg_after(&plan, "--model").as_deref(), Some("flag-model"));
    assert_eq!(plan["time_limit_secs"], 30);
}

#[test]
fn a_file_in_the_working_directory_or_the_repository_is_never_read() {
    let rig = Rig::new();
    let body = "schema_version = 1\n[defaults]\nmodel = \"project-model\"\ntimeout_secs = 99\n";
    for path in [
        rig.work.join("config.toml"),
        rig.work.join("delega.toml"),
        rig.work.join(".delega/config.toml"),
        rig.work.join("delega/config.toml"),
        rig.work.join(".config/delega/config.toml"),
    ] {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("directory");
        std::fs::write(&path, body).expect("write");
    }
    std::fs::create_dir(rig.work.join(".git")).expect("git directory");
    // The rig's user-level place is empty, so nothing is configured: no file in
    // the working directory or the repository is looked at.
    let out = rig
        .command()
        .current_dir(&rig.work)
        .args([
            "run",
            "--output",
            "json",
            "--dry-run",
            "--provider",
            "claude",
            "-p",
            "x",
        ])
        .guarded_output()
        .expect("run delega");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let plan = json_of(&out)["dry_run"].clone();
    assert!(arg_after(&plan, "--model").is_none(), "{plan}");
    assert_eq!(plan["time_limit_secs"], 600);
}

#[test]
fn delega_config_names_another_file() {
    let rig = Rig::new();
    write_config(
        &rig,
        "schema_version = 1\n[defaults]\nmodel = \"default-place\"\n",
    );
    let other = rig.file(
        "other.toml",
        "schema_version = 1\n[defaults]\nmodel = \"other-place\"\n",
    );
    std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    let out = rig
        .command()
        .env("DELEGA_CONFIG", &other)
        .args([
            "run",
            "--output",
            "json",
            "--dry-run",
            "--provider",
            "claude",
            "-p",
            "x",
            "-C",
        ])
        .arg(&rig.work)
        .guarded_output()
        .expect("run delega");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        arg_after(&json_of(&out)["dry_run"], "--model").as_deref(),
        Some("other-place")
    );
    // A named file that does not exist is refused, not ignored.
    let out = rig
        .command()
        .env("DELEGA_CONFIG", rig.path("nowhere.toml"))
        .args([
            "run",
            "--output",
            "json",
            "--dry-run",
            "--provider",
            "claude",
            "-p",
            "x",
        ])
        .guarded_output()
        .expect("run delega");
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json_of(&out)["error"]["kind"], "config_unreadable");
}

/// Files that must be refused, with the kind each is refused with.
fn refused_files() -> Vec<(&'static str, String, &'static str)> {
    vec![
        (
            "an unknown key",
            "schema_version = 1\ncolour = \"red\"\n".to_string(),
            "config_invalid",
        ),
        (
            "a bad value",
            format!("schema_version = 1\n[defaults]\ntimeout_secs = \"{SECRET}\"\n"),
            "config_invalid",
        ),
        (
            "a prompt",
            format!("schema_version = 1\n[defaults]\nprompt = \"{SECRET}\"\n"),
            "config_invalid",
        ),
        (
            "a working directory",
            format!("schema_version = 1\n[defaults]\ncwd = \"/{SECRET}\"\n"),
            "config_invalid",
        ),
        (
            "a missing version",
            "[defaults]\nmodel = \"m\"\n".to_string(),
            "config_invalid",
        ),
        (
            "broken TOML",
            format!("schema_version = 1\nmodel = \"{SECRET}\n"),
            "config_invalid",
        ),
        (
            "an env name the flag would refuse",
            format!("schema_version = 1\n[env]\npass = [\"NAME={SECRET}\"]\n"),
            "invalid_env_name",
        ),
        (
            "a forbidden env name",
            "schema_version = 1\n[env.set]\nDELEGA_X = \"v\"\n".to_string(),
            "forbidden_env_name",
        ),
    ]
}

#[test]
fn an_invalid_file_is_refused_by_run_as_a_refused_result_before_anything_else() {
    for (what, body, kind) in refused_files() {
        let rig = Rig::new();
        write_config(&rig, &body);
        // Even a request that is otherwise invalid (no prompt) is refused for
        // the file first.
        let out = rig
            .command()
            .args(["run", "--output", "json"])
            .guarded_output()
            .expect("run delega");
        assert_eq!(out.status.code(), Some(2), "{what}: {}", text(&out.stderr));
        let result = json_of(&out);
        assert_eq!(result["outcome"], "refused", "{what}: {result}");
        assert_eq!(result["error"]["kind"], kind, "{what}: {result}");
        let message = result["error"]["message"].as_str().expect("message");
        assert!(
            message.contains(&config_path(&rig).display().to_string()),
            "{what}: {message}"
        );
        assert_no_secret(&out);
        // The same in text, where the line names the kind and nothing is on stdout.
        let out = rig
            .command()
            .args(["run", "-p", "x", "--provider", "claude"])
            .guarded_output()
            .expect("run delega");
        assert_eq!(out.status.code(), Some(2), "{what}");
        assert!(out.stdout.is_empty(), "{what}");
        assert!(
            text(&out.stderr).starts_with(&format!("delega run: refused: {kind}: ")),
            "{what}: {}",
            text(&out.stderr)
        );
        assert_no_secret(&out);
        // A dry run refuses the same way, and a run never starts a provider.
        let out = dry(&rig, "claude", &[]);
        assert_eq!(out.status.code(), Some(2), "{what}");
        assert_eq!(json_of(&out)["error"]["kind"], kind, "{what}");
        assert!(!rig.record.join("argv").exists(), "{what}: a provider ran");
    }
}

#[test]
fn an_unsafe_file_is_refused_by_run() {
    let rig = Rig::new();
    let path = write_config(&rig, FULL);
    for mode in [0o660, 0o606, 0o666] {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");
        let out = dry(&rig, "claude", &[]);
        assert_eq!(out.status.code(), Some(2), "{mode:o}");
        let result = json_of(&out);
        assert_eq!(result["error"]["kind"], "config_unreadable", "{mode:o}");
        assert!(
            result["error"]["message"]
                .as_str()
                .expect("message")
                .contains("written by group or others"),
            "{result}"
        );
        assert_no_secret(&out);
    }
    // A file that is a directory, and a file over the limit.
    std::fs::remove_file(&path).expect("remove");
    std::fs::create_dir(&path).expect("directory");
    let out = dry(&rig, "claude", &[]);
    assert_eq!(json_of(&out)["error"]["kind"], "config_unreadable");
    std::fs::remove_dir(&path).expect("remove");
    write_config(
        &rig,
        &format!("schema_version = 1\n# {}\n", "x".repeat(1 << 20)),
    );
    let out = dry(&rig, "claude", &[]);
    let result = json_of(&out);
    assert_eq!(result["error"]["kind"], "config_unreadable");
    assert!(
        result["error"]["message"]
            .as_str()
            .expect("message")
            .contains("limit of 1048576 bytes")
    );
}

#[test]
fn a_progress_run_with_a_refused_file_ends_with_the_refused_line() {
    let rig = Rig::new();
    write_config(&rig, "schema_version = 1\ncolour = 1\n");
    let out = rig
        .command()
        .args(["run", "--progress", "-p", "x", "--provider", "claude"])
        .guarded_output()
        .expect("run delega");
    assert_eq!(out.status.code(), Some(2));
    let err = text(&out.stderr);
    let lines: Vec<&str> = err.lines().collect();
    assert_eq!(lines[0], "progress: finished: refused", "{err}");
    assert!(
        lines[1].starts_with("delega run: refused: config_invalid: "),
        "{err}"
    );
}

#[test]
fn mcp_refuses_to_start_on_a_file_it_cannot_use() {
    for (what, body, kind) in refused_files() {
        let rig = Rig::new();
        write_config(&rig, &body);
        let out = rig
            .command()
            .arg("mcp")
            .guarded_output()
            .expect("run delega mcp");
        assert_eq!(out.status.code(), Some(2), "{what}");
        assert!(out.stdout.is_empty(), "{what}: nothing is served");
        let err = text(&out.stderr);
        assert!(
            err.starts_with(&format!("delega mcp: refused: {kind}: ")),
            "{what}: {err}"
        );
        assert_no_secret(&out);
    }
    let rig = Rig::new();
    let path = write_config(&rig, FULL);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o664)).expect("chmod");
    let out = rig
        .command()
        .arg("mcp")
        .guarded_output()
        .expect("run delega mcp");
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(text(&out.stderr).contains("delega mcp: refused: config_unreadable: "));
    assert_no_secret(&out);
}

fn doctor(rig: &Rig) -> Output {
    // The process listing has a deadline of its own; the fields read here do not
    // depend on it, and neither does the refusal status.
    rig.command()
        .args(["doctor", "--output", "json"])
        .current_dir(&rig.work)
        .guarded_output()
        .expect("run delega doctor")
}

#[test]
fn doctor_reports_the_file_it_read_and_applies_its_executables_environment_and_limits() {
    let rig = Rig::new();
    let program = rig.path("bin-codex");
    std::os::unix::fs::symlink(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fakes/fake-codex.sh"),
        &program,
    )
    .expect("link");
    let path = write_config(
        &rig,
        &format!(
            "schema_version = 1\n[defaults]\nmodel = \"m\"\n[env]\npass = [\"A_ONE\", \"A_TWO\"]\n\
             [env.set]\nB_ONE = \"{SECRET}\"\n[providers.codex]\nexecutable = \"{}\"\n\
             [mcp]\nmax_live_workers = 3\nwait_default_secs = 10\nwait_ceiling_secs = 60\n",
            program.display()
        ),
    );
    let out = rig
        .command()
        .env_remove("DELEGA_CODEX_BIN")
        .args(["doctor", "--output", "json"])
        .current_dir(&rig.work)
        .guarded_output()
        .expect("run delega doctor");
    let report = json_of(&out);
    let config = &report["config"];
    assert_eq!(config["path"], path.to_string_lossy().as_ref());
    assert_eq!(config["source"], "XDG_CONFIG_HOME");
    assert_eq!(config["found"], true);
    assert_eq!(config["read"], true);
    assert_eq!(config["env_pass"], 2);
    assert_eq!(config["env_set"], 1);
    let settings: Vec<&str> = config["settings"]
        .as_array()
        .expect("settings")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(
        settings,
        [
            "defaults.model",
            "env.pass",
            "env.set",
            "mcp.max_live_workers",
            "mcp.wait_ceiling_secs",
            "mcp.wait_default_secs",
            "providers.codex.executable"
        ]
    );
    // The file's executable is the one doctor checks, and says where it came from.
    let codex = report["providers"]
        .as_array()
        .expect("providers")
        .iter()
        .find(|p| p["provider"] == "codex")
        .expect("codex");
    assert_eq!(codex["executable"], program.to_string_lossy().as_ref());
    assert_eq!(codex["executable_source"], "config");
    // The environment counts what a run would pass: the base and the file's set
    // variable (the two names are not set here).
    let claude = report["providers"]
        .as_array()
        .expect("providers")
        .iter()
        .find(|p| p["provider"] == "claude")
        .expect("claude");
    let without = {
        let rig = Rig::new();
        let out = doctor(&rig);
        json_of(&out)["providers"][0]["env"]["passed"]
            .as_u64()
            .expect("count")
    };
    assert_eq!(
        claude["env"]["passed"].as_u64().expect("count"),
        without + 1
    );
    // The limits in force are the file's.
    let sup = &report["limits"]["supervisor"];
    assert_eq!(sup["max_live_workers"], 3);
    assert_eq!(sup["default_wait_secs"], 10);
    assert_eq!(sup["wait_ceiling_secs"], 60);
    assert_eq!(sup["max_running_runs"], 4);
    assert_no_secret(&out);
    // Text form names the file and the keys, never a value.
    let out = rig
        .command()
        .env_remove("DELEGA_CODEX_BIN")
        .args(["doctor"])
        .current_dir(&rig.work)
        .guarded_output()
        .expect("run delega doctor");
    let report = text(&out.stdout);
    for expected in [
        "configuration file:",
        "(from XDG_CONFIG_HOME)",
        "state: found and read",
        "sets: defaults.model, env.pass, env.set",
        "2 names passed, 1 variables set (values are never shown)",
        "(from config)",
    ] {
        assert!(report.contains(expected), "{expected}\n{report}");
    }
    assert_no_secret(&out);
}

#[test]
fn doctor_without_a_file_says_so_and_changes_nothing_else() {
    let rig = Rig::new();
    let out = doctor(&rig);
    let report = json_of(&out);
    assert_eq!(report["config"]["found"], false);
    assert_eq!(report["config"]["read"], false);
    assert_eq!(
        report["config"]["path"],
        config_path(&rig).to_string_lossy().as_ref()
    );
    assert_eq!(report["config"]["settings"], serde_json::json!([]));
    assert_eq!(report["limits"]["supervisor"]["max_live_workers"], 16);
    assert_eq!(report["limits"]["supervisor"]["default_wait_secs"], 30);
    let text_out = rig
        .command()
        .arg("doctor")
        .guarded_output()
        .expect("run delega doctor");
    assert!(text(&text_out.stdout).contains("state: not found; nothing is configured"));
    // With no HOME and no variable, there is nowhere to look.
    let out = rig
        .command()
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .args(["doctor", "--output", "json"])
        .guarded_output()
        .expect("run delega doctor");
    assert_eq!(json_of(&out)["config"]["path"], Value::Null);
}

#[test]
fn doctor_reports_a_refused_file_as_a_problem_that_stops_runs_and_keeps_reporting() {
    for (what, body, kind) in refused_files() {
        let rig = Rig::new();
        write_config(&rig, &body);
        let out = doctor(&rig);
        assert_eq!(out.status.code(), Some(1), "{what}: {}", text(&out.stderr));
        let report = json_of(&out);
        assert_eq!(report["ready"], false, "{what}");
        assert_eq!(report["config"]["found"], true, "{what}");
        assert_eq!(report["config"]["read"], false, "{what}");
        let problem = report["problems"]
            .as_array()
            .expect("problems")
            .iter()
            .find(|p| p["kind"] == kind)
            .unwrap_or_else(|| panic!("{what}: no {kind} problem in {report}"));
        assert_eq!(problem["blocks"], "run", "{what}");
        assert_eq!(problem["provider"], Value::Null, "{what}");
        assert!(
            problem["message"]
                .as_str()
                .expect("message")
                .contains(&config_path(&rig).display().to_string()),
            "{what}"
        );
        // The rest is still there: both providers, checked as if there were no file.
        let providers = report["providers"].as_array().expect("providers");
        assert_eq!(providers.len(), 2, "{what}");
        assert!(providers.iter().all(|p| p["ready"] == true), "{what}");
        assert!(report["skills"].as_array().expect("skills").len() >= 2);
        assert_no_secret(&out);
        let text_out = rig
            .command()
            .arg("doctor")
            .guarded_output()
            .expect("run delega doctor");
        assert_eq!(text_out.status.code(), Some(1), "{what}");
        let shown = text(&text_out.stdout);
        assert!(shown.contains("a run would be stopped"), "{what}: {shown}");
        assert!(shown.contains("found but refused"), "{what}: {shown}");
        assert!(
            shown.contains(&format!("system: {kind} (stops every run)")),
            "{what}: {shown}"
        );
        assert_no_secret(&text_out);
    }
}

#[test]
fn doctor_reports_an_unsafe_file_and_applies_nothing_from_it() {
    let rig = Rig::new();
    let path = write_config(
        &rig,
        "schema_version = 1\n[providers.claude]\nexecutable = \"/nonexistent/claude\"\n",
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).expect("chmod");
    let out = rig
        .command()
        .env_remove("DELEGA_CLAUDE_BIN")
        .args(["doctor", "--output", "json"])
        .current_dir(&rig.work)
        .guarded_output()
        .expect("run delega doctor");
    let report = json_of(&out);
    let kinds: Vec<&str> = report["problems"]
        .as_array()
        .expect("problems")
        .iter()
        .filter_map(|p| p["kind"].as_str())
        .collect();
    // The unsafe file's executable was not applied, so the claude lookup found
    // nothing on PATH and reports that, beside the file's own problem.
    assert!(kinds.contains(&"config_unreadable"), "{kinds:?}");
    assert!(kinds.contains(&"provider_not_found"), "{kinds:?}");
    assert!(
        !text(&out.stdout).contains("/nonexistent/claude"),
        "nothing from the refused file is reported"
    );
}
