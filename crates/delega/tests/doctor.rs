//! `delega doctor` against the fake providers and a temporary home.
//!
//! Every test points `DELEGA_CLAUDE_BIN` and `DELEGA_CODEX_BIN` at fakes, so no
//! real provider CLI is started, and runs in the rig's directories, so nothing
//! reads the real home directory.

mod support;

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::Value;
use support::{Guarded, Rig, text};

/// A value no output may contain: it is set in variables the allowlist passes.
const SECRET: &str = "distinctive-value-7731";

fn doctor_command(rig: &Rig, args: &[&str]) -> Command {
    let mut command = rig.command();
    command
        .current_dir(&rig.work)
        .env("HTTPS_PROXY", format!("http://user:{SECRET}@proxy.example"))
        .env("LANG", format!("{SECRET}.UTF-8"))
        .arg("doctor")
        .args(args);
    command
}

/// Whether the report says process inspection did not work. The listing has a
/// deadline of its own in a release-like build, which a machine under heavy
/// load could miss, so the tests that expect it to work run again, up to eight
/// times, like the `run` tests that need a timely listing.
fn inspection_failed(out: &Output) -> bool {
    text(&out.stdout).contains("process inspection: unavailable")
        || text(&out.stdout).contains("\"process_inspection_unavailable\"")
}

fn doctor_with(rig: &Rig, args: &[&str], tweak: impl Fn(&mut Command)) -> Output {
    let mut out = None;
    for _ in 0..8 {
        let mut command = doctor_command(rig, args);
        tweak(&mut command);
        let run = command.guarded_output().expect("run delega doctor");
        let failed = inspection_failed(&run);
        out = Some(run);
        if !failed {
            break;
        }
    }
    out.expect("one attempt")
}

fn json_of(out: &Output) -> Value {
    serde_json::from_str(&text(&out.stdout)).unwrap_or_else(|e| {
        panic!(
            "not JSON: {e}\n{}\nstderr: {}",
            text(&out.stdout),
            text(&out.stderr)
        )
    })
}

fn provider<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["providers"]
        .as_array()
        .expect("providers")
        .iter()
        .find(|p| p["provider"] == name)
        .expect("provider entry")
}

fn problem_kinds(report: &Value) -> Vec<(String, String)> {
    report["problems"]
        .as_array()
        .expect("problems")
        .iter()
        .map(|p| {
            (
                p["provider"].as_str().unwrap_or("system").to_string(),
                p["kind"].as_str().expect("kind").to_string(),
            )
        })
        .collect()
}

fn assert_no_value(out: &Output) {
    for (what, bytes) in [("stdout", &out.stdout), ("stderr", &out.stderr)] {
        assert!(
            !text(bytes).contains(SECRET),
            "{what} holds an environment value"
        );
    }
}

/// A program that prints `body` for `--version` and fails for anything else.
fn script(rig: &Rig, name: &str, body: &str) -> PathBuf {
    let path = rig.file(name, &format!("#!/bin/sh\n{body}\n"));
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

fn missing(rig: &Rig) -> PathBuf {
    rig.path("no-such-program")
}

fn listing(dir: &PathBuf) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("read dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_clean_setup_reports_both_providers_as_text_and_exits_zero() {
    let rig = Rig::new();
    let out = doctor_with(&rig, &[], |_| {});
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let report = text(&out.stdout);
    for expected in [
        "a run can go ahead",
        "claude: ready",
        "codex: ready",
        "version: 2.1.400 (Claude Code)",
        "command hook: available (needs 2.1.325 or later)",
        "(from DELEGA_CLAUDE_BIN)",
        "(from DELEGA_CODEX_BIN)",
        "process inspection: available",
        "time limit 600 s by default",
        "16 live workers",
        "problems: none",
        "skills:",
    ] {
        assert!(report.contains(expected), "{expected}\n{report}");
    }
    assert!(out.stderr.is_empty(), "{}", text(&out.stderr));
    assert_no_value(&out);
}

#[test]
fn a_clean_setup_reports_both_providers_as_json_with_the_documented_shape() {
    let rig = Rig::new();
    let out = doctor_with(&rig, &["--output", "json"], |_| {});
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let report = json_of(&out);
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["ready"], true);
    assert_eq!(report["problems"], serde_json::json!([]));
    assert_eq!(report["process_inspection"]["available"], true);
    let claude = provider(&report, "claude");
    assert_eq!(claude["ready"], true);
    assert_eq!(claude["version"], "2.1.400 (Claude Code)");
    assert_eq!(claude["command_hook"]["available"], true);
    assert_eq!(claude["executable_source"], "DELEGA_CLAUDE_BIN");
    assert_eq!(claude["deny_rules"]["applied"], 0);
    // The proxy variable is passed; its value is not in any output.
    assert!(claude["env"]["network"].as_u64().expect("network") >= 1);
    assert!(claude["env"]["passed"].as_u64().expect("passed") >= 1);
    let codex = provider(&report, "codex");
    assert_eq!(codex["ready"], true);
    assert_eq!(codex["command_hook"], Value::Null);
    assert_eq!(codex["deny_rules"], Value::Null);
    // The limits are the code's constants.
    let limits = &report["limits"];
    assert_eq!(limits["run"]["default_timeout_secs"], 600);
    assert_eq!(limits["supervisor"]["max_live_workers"], 16);
    assert_eq!(limits["supervisor"]["wait_ceiling_secs"], 300);
    assert_no_value(&out);
}

#[test]
fn the_json_keys_are_sorted_at_every_depth() {
    fn sorted(value: &Value) -> bool {
        match value {
            Value::Object(map) => {
                let keys: Vec<&String> = map.keys().collect();
                keys.windows(2).all(|w| w[0] < w[1]) && map.values().all(sorted)
            }
            Value::Array(items) => items.iter().all(sorted),
            _ => true,
        }
    }
    let rig = Rig::new();
    let out = doctor_with(&rig, &["--output", "json"], |_| {});
    assert!(sorted(&json_of(&out)), "{}", text(&out.stdout));
}

#[test]
fn the_skills_are_the_ones_version_reports() {
    let rig = Rig::new();
    let out = doctor_with(&rig, &["--output", "json"], |_| {});
    let version = rig.delega(&["version", "--output", "json"]);
    assert_eq!(json_of(&out)["skills"], json_of(&version)["skills"]);
    assert_eq!(json_of(&out)["version"], json_of(&version)["version"]);
}

#[test]
fn one_missing_executable_is_a_problem_for_that_provider_and_the_exit_is_zero() {
    let rig = Rig::new();
    let absent = missing(&rig);
    let out = doctor_with(&rig, &["--output", "json"], |c| {
        c.env("DELEGA_CODEX_BIN", &absent);
    });
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stdout));
    let report = json_of(&out);
    assert_eq!(report["ready"], true);
    assert_eq!(
        problem_kinds(&report),
        [("codex".to_string(), "provider_not_found".to_string())]
    );
    let codex = provider(&report, "codex");
    assert_eq!(codex["ready"], false);
    assert_eq!(codex["executable"], Value::Null);
    assert_eq!(codex["version"], Value::Null);
    // The environment a worker would get is still reported.
    assert!(codex["env"]["passed"].as_u64().is_some());
    assert_eq!(provider(&report, "claude")["ready"], true);
    assert_no_value(&out);
    let text = doctor_with(&rig, &[], |c| {
        c.env("DELEGA_CODEX_BIN", &absent);
    });
    let shown = support::text(&text.stdout);
    assert!(shown.contains("codex: not ready"), "{shown}");
    assert!(shown.contains("executable: not found"), "{shown}");
    assert!(shown.contains("provider_not_found"), "{shown}");
}

#[test]
fn no_executable_for_either_provider_exits_one() {
    let rig = Rig::new();
    let absent = missing(&rig);
    let out = doctor_with(&rig, &["--output", "json"], |c| {
        c.env("DELEGA_CLAUDE_BIN", &absent)
            .env("DELEGA_CODEX_BIN", &absent);
    });
    assert_eq!(out.status.code(), Some(1));
    let report = json_of(&out);
    assert_eq!(report["ready"], false);
    assert_eq!(problem_kinds(&report).len(), 2);
    assert_no_value(&out);
}

#[test]
fn a_failing_version_probe_blocks_only_command_grants_for_claude_as_run_does() {
    let rig = Rig::new();
    let failing = script(&rig, "failing-claude", "exit 3");
    let out = doctor_with(&rig, &["--output", "json"], |c| {
        c.env("DELEGA_CLAUDE_BIN", &failing);
    });
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stdout));
    let report = json_of(&out);
    assert_eq!(
        problem_kinds(&report),
        [("claude".to_string(), "version_probe_failed".to_string())]
    );
    assert_eq!(report["problems"][0]["blocks"], "command_grants");
    let claude = provider(&report, "claude");
    // `run` goes ahead without a version unless exact commands are granted.
    assert_eq!(claude["ready"], true);
    assert_eq!(claude["version"], Value::Null);
    assert_eq!(claude["command_hook"]["available"], false);
    // Codex: a failing probe is a warning that blocks nothing.
    let both = doctor_with(&rig, &["--output", "json"], |c| {
        c.env("DELEGA_CLAUDE_BIN", &failing)
            .env("DELEGA_CODEX_BIN", &failing);
    });
    assert_eq!(both.status.code(), Some(0));
    let report = json_of(&both);
    let codex_problem = report["problems"]
        .as_array()
        .expect("problems")
        .iter()
        .find(|p| p["provider"] == "codex")
        .expect("codex problem");
    assert_eq!(codex_problem["kind"], "version_probe_failed");
    assert_eq!(codex_problem["blocks"], "nothing");
    assert_eq!(provider(&report, "codex")["ready"], true);
}

#[test]
fn a_version_with_escape_sequences_is_shown_without_them_in_text() {
    let rig = Rig::new();
    let noisy = script(
        &rig,
        "noisy-claude",
        r"printf '2.1.400 \033[31mred\033]0;title\007\n'",
    );
    let out = doctor_with(&rig, &[], |c| {
        c.env("DELEGA_CLAUDE_BIN", &noisy);
    });
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(
        !out.stdout.contains(&0x1b) && !out.stdout.contains(&0x07),
        "control characters reached the output"
    );
    assert!(text(&out.stdout).contains("version: 2.1.400 ?[31mred?]0;title?"));
    // An escape in a path is sanitized too, in the problem and the executable.
    let escaped = rig.path("bad\u{1b}[31mname");
    let out = doctor_with(&rig, &[], |c| {
        c.env("DELEGA_CODEX_BIN", &escaped);
    });
    assert!(!out.stdout.contains(&0x1b), "{}", text(&out.stdout));
    assert!(text(&out.stdout).contains("bad?[31mname"));
    // JSON escapes them.
    let json = doctor_with(&rig, &["--output", "json"], |c| {
        c.env("DELEGA_CLAUDE_BIN", &noisy);
    });
    assert!(!json.stdout.contains(&0x1b));
}

#[test]
fn an_unsupported_claude_version_stops_only_exact_command_grants() {
    let rig = Rig::new();
    let old = script(&rig, "old-claude", "echo '2.0.1 (Claude Code)'");
    let out = doctor_with(&rig, &["--output", "json"], |c| {
        c.env("DELEGA_CLAUDE_BIN", &old);
    });
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stdout));
    let report = json_of(&out);
    assert_eq!(
        problem_kinds(&report),
        [(
            "claude".to_string(),
            "unsupported_provider_version".to_string()
        )]
    );
    assert_eq!(report["problems"][0]["blocks"], "command_grants");
    let claude = provider(&report, "claude");
    assert_eq!(claude["version"], "2.0.1 (Claude Code)");
    assert_eq!(claude["command_hook"]["available"], false);
    assert_eq!(claude["command_hook"]["required_version"], "2.1.325");
    // A version at the minimum is supported.
    let exact = script(&rig, "exact-claude", "echo '2.1.325 (Claude Code)'");
    let out = doctor_with(&rig, &["--output", "json"], |c| {
        c.env("DELEGA_CLAUDE_BIN", &exact);
    });
    let report = json_of(&out);
    assert_eq!(report["problems"], serde_json::json!([]));
    assert_eq!(
        provider(&report, "claude")["command_hook"]["available"],
        true
    );
}

#[test]
fn unreadable_settings_in_the_current_directory_stop_claude_runs() {
    let rig = Rig::new();
    let absent = missing(&rig);
    std::fs::create_dir(rig.work.join(".claude")).expect("create .claude");
    std::fs::write(
        rig.work.join(".claude/settings.json"),
        format!("not json {SECRET}"),
    )
    .expect("write settings");
    let out = doctor_with(&rig, &["--output", "json"], |c| {
        c.env("DELEGA_CODEX_BIN", &absent);
    });
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stdout));
    let report = json_of(&out);
    let kinds = problem_kinds(&report);
    assert!(
        kinds.contains(&("claude".to_string(), "settings_unreadable".to_string())),
        "{kinds:?}"
    );
    let claude = provider(&report, "claude");
    assert_eq!(claude["ready"], false);
    // The executable and version are still reported.
    assert_eq!(claude["version"], "2.1.400 (Claude Code)");
    // The message names the file, never its content.
    let message = report["problems"][0]["message"].as_str().expect("message");
    assert!(message.contains("settings.json"), "{message}");
    assert_no_value(&out);
    // With Codex available the run can still go ahead.
    let with_codex = doctor_with(&rig, &[], |_| {});
    assert_eq!(with_codex.status.code(), Some(0));
    assert!(text(&with_codex.stdout).contains("settings_unreadable"));
}

#[test]
fn deny_rules_are_counted_with_their_files_and_never_printed() {
    let rig = Rig::new();
    std::fs::create_dir(rig.work.join(".claude")).expect("create .claude");
    std::fs::write(
        rig.work.join(".claude/settings.json"),
        format!(r#"{{"permissions":{{"deny":["Read(./{SECRET}.txt)","Bash(rm:*)"]}}}}"#),
    )
    .expect("write settings");
    let out = doctor_with(&rig, &["--output", "json"], |_| {});
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stdout));
    let report = json_of(&out);
    let deny = &provider(&report, "claude")["deny_rules"];
    assert!(deny["applied"].as_u64().expect("applied") >= 2, "{deny}");
    let files = deny["files"].as_array().expect("files");
    assert_eq!(files.len(), 1, "{deny}");
    assert!(
        files[0]["path"]
            .as_str()
            .expect("path")
            .ends_with(".claude/settings.json")
    );
    assert_eq!(files[0]["rules"], 2);
    assert_no_value(&out);
    let shown = doctor_with(&rig, &[], |_| {});
    assert!(text(&shown.stdout).contains("deny rules: "));
    assert!(!text(&shown.stdout).contains("rm:*"));
    assert_no_value(&shown);
}

#[test]
fn unavailable_process_inspection_is_named_and_exits_one() {
    let rig = Rig::new();
    let out = rig
        .command()
        .current_dir(&rig.work)
        .env("DELEGA_TEST_NO_PROCESS_LISTING", "1")
        .args(["doctor", "--output", "json"])
        .guarded_output()
        .expect("run delega doctor");
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stdout));
    let report = json_of(&out);
    assert_eq!(report["ready"], false);
    assert_eq!(report["process_inspection"]["available"], false);
    assert_eq!(
        problem_kinds(&report),
        [(
            "system".to_string(),
            "process_inspection_unavailable".to_string()
        )]
    );
    assert_eq!(report["problems"][0]["provider"], Value::Null);
    assert_eq!(report["problems"][0]["blocks"], "cleanup");
    let message = report["problems"][0]["message"].as_str().expect("message");
    assert!(message.contains("not confirmed"), "{message}");
    // Both providers themselves are fine.
    assert_eq!(provider(&report, "claude")["ready"], true);
    assert_eq!(provider(&report, "codex")["ready"], true);
    let shown = rig
        .command()
        .current_dir(&rig.work)
        .env("DELEGA_TEST_NO_PROCESS_LISTING", "1")
        .arg("doctor")
        .guarded_output()
        .expect("run delega doctor");
    assert_eq!(shown.status.code(), Some(1));
    assert!(text(&shown.stdout).contains("process inspection: unavailable"));
}

#[test]
fn doctor_writes_nothing_and_leaves_no_record_of_a_started_worker() {
    let rig = Rig::new();
    let before = (listing(&rig.work), listing(&rig.home), listing(&rig.record));
    let out = doctor_with(&rig, &[], |_| {});
    assert_eq!(out.status.code(), Some(0));
    let after = (listing(&rig.work), listing(&rig.home), listing(&rig.record));
    assert_eq!(before, after);
}

#[test]
fn doctor_cannot_run_without_a_usable_current_directory_and_exits_two() {
    let rig = Rig::new();
    let gone = rig.path("gone");
    std::fs::create_dir(&gone).expect("create directory");
    let mut command = Command::new("/bin/sh");
    rig.configure(&mut command);
    let out = command
        .arg("-c")
        .arg(format!(
            "cd '{0}' && rmdir '{0}' && exec '{1}' doctor",
            gone.display(),
            env!("CARGO_BIN_EXE_delega")
        ))
        .guarded_output()
        .expect("run sh");
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert!(out.stdout.is_empty());
    assert!(
        text(&out.stderr).contains("delega doctor:"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn a_version_probe_that_does_not_answer_is_reported_as_timed_out() {
    // The program sleeps far longer than the probe's limit, so the verdict does
    // not depend on how fast the machine is; the test takes about the limit.
    let rig = Rig::new();
    let silent = script(&rig, "silent-codex", "exec sleep 120");
    let out = doctor_with(&rig, &["--output", "json"], |c| {
        c.env("DELEGA_CODEX_BIN", &silent);
    });
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stdout));
    let report = json_of(&out);
    assert_eq!(
        problem_kinds(&report),
        [("codex".to_string(), "version_probe_timed_out".to_string())]
    );
    assert_eq!(report["problems"][0]["blocks"], "nothing");
    assert_eq!(provider(&report, "codex")["ready"], true);
}

#[test]
fn an_executable_that_cannot_be_started_stops_every_run_with_it() {
    let rig = Rig::new();
    let broken = rig.file("broken", "#!/no/such/interpreter\n");
    std::fs::set_permissions(&broken, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    for name in ["claude", "codex"] {
        let variable = format!("DELEGA_{}_BIN", name.to_uppercase());
        let out = doctor_with(&rig, &["--output", "json"], |c| {
            c.env(&variable, &broken);
        });
        // The other provider is ready, so the exit is 0.
        assert_eq!(out.status.code(), Some(0), "{name}: {}", text(&out.stdout));
        let report = json_of(&out);
        assert_eq!(
            problem_kinds(&report),
            [(name.to_string(), "provider_not_startable".to_string())],
            "{name}"
        );
        assert_eq!(report["problems"][0]["blocks"], "run", "{name}");
        assert_eq!(provider(&report, name)["ready"], false, "{name}");
        // Run fails to start with it.
        let ran = rig
            .command()
            .env(&variable, &broken)
            .args(rig.run_args(name, "success", &[]))
            .guarded_output()
            .expect("run delega");
        assert_ne!(ran.status.code(), Some(0), "{name}");
    }
}
