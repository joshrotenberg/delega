//! The committed schema and the shape of a report.

use super::*;
use crate::provider::doctor::{CommandHook, DenyInfo, EnvCounts, SANDBOX_UNAVAILABLE};
use crate::provider::{DenyFile, SkippedFile};

const REGENERATE: &str = "DELEGA_UPDATE_SCHEMAS=1 cargo test -p delega-core committed_schemas";

#[test]
fn committed_schemas_doctor_matches_the_generated_schema() {
    let generated = generate_schema();
    if generated == DOCTOR_SCHEMA {
        return;
    }
    if std::env::var_os("DELEGA_UPDATE_SCHEMAS").is_some() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("schemas/doctor.schema.json");
        std::fs::write(path, &generated).expect("write schema file");
        return;
    }
    panic!(
        "committed schema out of date: doctor.schema.json. Regenerate with `{REGENERATE}` and \
         commit the result."
    );
}

fn full_report() -> Report {
    Report {
        config: ConfigReport {
            path: Some("/h/.config/delega/config.toml".to_string()),
            source: Some("HOME".to_string()),
            found: true,
            read: true,
            settings: vec!["defaults.model".to_string(), "env.set".to_string()],
            env_pass: 0,
            env_set: 1,
        },
        schema_version: SCHEMA_VERSION,
        ready: false,
        version: "0.1.0".to_string(),
        cwd: "/work".to_string(),
        providers: vec![
            ProviderCheck {
                provider: Provider::Claude,
                executable: Some("/bin/claude".to_string()),
                executable_source: Some("PATH".to_string()),
                version: Some("2.1.400 (Claude Code)".to_string()),
                command_hook: Some(CommandHook {
                    required_version: "2.1.325".to_string(),
                    available: true,
                }),
                sandbox: Some(SandboxCheck {
                    system: "linux".to_string(),
                    requirements_met: true,
                    requires: vec!["bwrap".to_string(), "socat".to_string()],
                    missing: Vec::new(),
                }),
                env: Some(EnvCounts {
                    passed: 5,
                    base: 3,
                    network: 0,
                    provider: 1,
                    set_by_delega: 1,
                    withheld: 7,
                }),
                deny_rules: Some(DenyInfo {
                    files: vec![DenyFile {
                        path: "/h/.claude/settings.json".to_string(),
                        rules: 2,
                    }],
                    skipped: vec![SkippedFile {
                        path: "/w/.claude/settings.local.json".to_string(),
                        reason: "not owned by this user".to_string(),
                    }],
                    applied: 2,
                }),
                managed_settings: Vec::new(),
                ready: true,
            },
            ProviderCheck {
                provider: Provider::Codex,
                executable: None,
                executable_source: None,
                version: None,
                command_hook: None,
                sandbox: None,
                env: None,
                deny_rules: None,
                managed_settings: Vec::new(),
                ready: false,
            },
        ],
        process_inspection: ProcessInspection {
            available: false,
            detail: Some("ps could not be started".to_string()),
        },
        limits: Limits::current(),
        problems: vec![ReportedProblem {
            provider: None,
            kind: PROCESS_INSPECTION_UNAVAILABLE.to_string(),
            message: "m".to_string(),
            blocks: Blocks::Cleanup,
        }],
        skills: vec![SkillDigest {
            name: "run".to_string(),
            uri: "skill://delega/run/SKILL.md".to_string(),
            digest: "abc".to_string(),
        }],
    }
}

#[test]
fn a_report_satisfies_the_schema_and_a_wrong_version_does_not() {
    let schema: Value = serde_json::from_str(DOCTOR_SCHEMA).expect("schema is JSON");
    let validator = jsonschema::validator_for(&schema).expect("compile schema");
    let document: Value = serde_json::from_str(&full_report().to_json()).expect("report is JSON");
    assert!(validator.is_valid(&document), "{document}");
    let mut wrong = document.clone();
    wrong["schema_version"] = json!(2);
    assert!(!validator.is_valid(&wrong));
    let mut missing = document;
    missing.as_object_mut().expect("object").remove("limits");
    assert!(!validator.is_valid(&missing));
}

#[test]
fn the_json_has_sorted_keys_at_every_depth() {
    fn check(value: &Value) {
        match value {
            Value::Object(map) => {
                let keys: Vec<&String> = map.keys().collect();
                let mut sorted = keys.clone();
                sorted.sort();
                assert_eq!(keys, sorted);
                map.values().for_each(check);
            }
            Value::Array(items) => items.iter().for_each(check),
            _ => {}
        }
    }
    let document: Value = serde_json::from_str(&full_report().to_json()).expect("JSON");
    check(&document);
}

#[test]
fn the_limits_come_from_the_constants() {
    let limits = Limits::current();
    assert_eq!(limits.run.default_timeout_secs, DEFAULT_TIMEOUT_SECS);
    assert_eq!(
        limits.supervisor.max_live_workers,
        supervisor::MAX_LIVE_WORKERS
    );
    assert_eq!(
        limits.supervisor.max_outstanding_request_checks,
        supervisor::MAX_PENDING_PREFLIGHTS
    );
}

/// The full report with Claude Code's sandbox missing `socat`, as
/// `diagnose_at` reports it.
fn report_without_socat() -> Report {
    let mut report = full_report();
    let sandbox = SandboxCheck {
        system: "linux".to_string(),
        requirements_met: false,
        requires: vec!["bwrap".to_string(), "socat".to_string()],
        missing: vec!["socat".to_string()],
    };
    let problem = sandbox.problem().expect("a problem");
    report.providers[0].sandbox = Some(sandbox);
    report.problems.push(ReportedProblem {
        provider: Some(Provider::Claude),
        kind: problem.kind,
        message: problem.message,
        blocks: problem.blocks,
    });
    report
}

#[test]
fn the_text_names_a_missing_sandbox_tool_and_that_it_stops_build_runs() {
    let text = report_without_socat().to_text();
    for expected in [
        "claude: ready",
        "  sandbox for build runs: socat not found on PATH",
        "  claude: sandbox_unavailable (can stop build runs): socat not found on PATH; Claude Code's \
         sandbox needs bwrap and socat on Linux, so build runs with Claude usually fail at \
         startup. Install socat. Doctor does not check other causes",
    ] {
        assert!(text.contains(expected), "{expected}\n{text}");
    }
    let available = full_report().to_text();
    assert!(
        available.contains(
            "  sandbox for build runs: bwrap and socat found on PATH; not otherwise checked"
        ),
        "{available}"
    );
    let mut macos = full_report();
    macos.providers[0].sandbox = Some(SandboxCheck::of("macos", None, std::path::Path::new("/")));
    let text = macos.to_text();
    assert!(
        text.contains(
            "  sandbox for build runs: no tool needed on this system; not otherwise checked"
        ),
        "{text}"
    );
}

#[test]
fn the_json_names_a_missing_sandbox_tool_and_satisfies_the_schema() {
    let report = report_without_socat();
    let value: Value = serde_json::from_str(&report.to_json()).expect("json");
    let schema: Value = serde_json::from_str(DOCTOR_SCHEMA).expect("schema");
    assert!(jsonschema::is_valid(&schema, &value));
    assert_eq!(
        value["providers"][0]["sandbox"],
        json!({
            "missing": ["socat"],
            "requirements_met": false,
            "requires": ["bwrap", "socat"],
            "system": "linux"
        })
    );
    assert_eq!(value["providers"][1]["sandbox"], Value::Null);
    let problem = value["problems"]
        .as_array()
        .expect("problems")
        .iter()
        .find(|p| p["kind"] == SANDBOX_UNAVAILABLE)
        .expect("the sandbox problem");
    assert_eq!(problem["blocks"], "build");
    assert_eq!(problem["provider"], "claude");
}

#[test]
fn the_exit_status_follows_ready() {
    let mut report = full_report();
    assert_eq!(report.exit_status(), 1);
    report.ready = true;
    assert_eq!(report.exit_status(), 0);
}

#[test]
fn the_text_names_the_file_its_state_and_its_keys() {
    let report = full_report().to_text();
    for expected in [
        "configuration file:",
        "path: /h/.config/delega/config.toml (from HOME)",
        "state: found and read",
        "sets: defaults.model, env.set",
        "environment: 0 names passed, 1 variables set (values are never shown)",
    ] {
        assert!(report.contains(expected), "{expected}\n{report}");
    }
    let mut report = full_report();
    report.config = ConfigReport::none();
    let text = report.to_text();
    assert!(text.contains("path: none"), "{text}");
    assert!(
        text.contains("state: not found; nothing is configured"),
        "{text}"
    );
    report.config = ConfigReport {
        path: Some("/c.toml".to_string()),
        source: Some("DELEGA_CONFIG".to_string()),
        found: true,
        read: false,
        ..ConfigReport::none()
    };
    let text = report.to_text();
    assert!(
        text.contains("state: found but refused, so nothing from it is applied"),
        "{text}"
    );
}

#[test]
fn a_refused_file_is_reported_as_found_and_not_read() {
    use crate::config::{Location, Source, Status};
    let status = Status {
        location: Some(Location {
            path: std::path::PathBuf::from("/c.toml"),
            source: Source::DelegaConfig,
        }),
        found: true,
        result: Err(Refusal {
            kind: "config_invalid".to_string(),
            message: "the configuration file /c.toml is not valid: x is not a known key"
                .to_string(),
        }),
        applied: None,
    };
    let report = ConfigReport::of(&status);
    assert!(report.found && !report.read);
    assert!(report.settings.is_empty());
    assert_eq!(report.source.as_deref(), Some("DELEGA_CONFIG"));
}

#[test]
fn the_limits_follow_the_supervisor_limits_given() {
    let lowered = crate::supervisor::Limits {
        max_live_workers: 3,
        wait_default: std::time::Duration::from_secs(7),
        wait_ceiling: std::time::Duration::from_secs(9),
        max_waits_in_flight: 5,
        ..crate::supervisor::Limits::default()
    };
    let limits = Limits::with_supervisor(&lowered);
    assert_eq!(limits.supervisor.max_live_workers, 3);
    assert_eq!(limits.supervisor.default_wait_secs, 7);
    assert_eq!(limits.supervisor.wait_ceiling_secs, 9);
    assert_eq!(limits.supervisor.max_waits_in_flight, 5);
    assert_eq!(
        limits.supervisor.max_running_runs,
        supervisor::MAX_RUNNING_RUNS
    );
    assert_eq!(
        Limits::with_supervisor(&crate::supervisor::Limits::default()),
        Limits::current()
    );
}

/// A value no output may contain.
const MANAGED_SECRET: &str = "sk-distinctive-managed-value-9093";

/// A report whose providers carry what a scan of a temporary root found: for
/// Claude Code a settings file, a drop-in directory with one file that parses
/// and one that does not, an MCP file and managed preferences for the system
/// and for a user, and for Codex a requirements file. Every file holds
/// [`MANAGED_SECRET`], and so do the names of the drop-in files and of the
/// user's directory; one drop-in name also carries a forged report line.
fn report_with_managed_settings() -> (tempfile::TempDir, Report) {
    use crate::provider::managed::{Platform, SystemPaths, scan};
    let root = tempfile::tempdir().expect("temporary root");
    let write = |relative: &str, content: String| {
        let path = root.path().join(relative);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create directories");
        std::fs::write(path, content).expect("write file");
    };
    let claude = "Library/Application Support/ClaudeCode";
    write(
        &format!("{claude}/managed-settings.json"),
        format!(
            r#"{{"allowManagedPermissionRulesOnly":true,"apiKeyHelper":"/bin/{MANAGED_SECRET}",
                "env":{{"TOKEN":"{MANAGED_SECRET}"}},"model":"{MANAGED_SECRET}",
                "permissions":{{"deny":["Read(./{MANAGED_SECRET})"]}}}}"#
        ),
    );
    write(
        &format!("{claude}/managed-settings.d/10-{MANAGED_SECRET}.json"),
        format!("not json {MANAGED_SECRET}"),
    );
    write(
        &format!("{claude}/managed-settings.d/20-x\nproblems: forged-{MANAGED_SECRET}.json"),
        r#"{"disableAllHooks":true}"#.to_string(),
    );
    write(
        &format!(
            "Library/Managed Preferences/u-{MANAGED_SECRET}\nforged/com.anthropic.claudecode.plist"
        ),
        MANAGED_SECRET.to_string(),
    );
    write(
        &format!("{claude}/managed-mcp.json"),
        format!(r#"{{"mcpServers":{{"{MANAGED_SECRET}":{{"command":"x"}}}}}}"#),
    );
    write(
        "Library/Managed Preferences/com.anthropic.claudecode.plist",
        MANAGED_SECRET.to_string(),
    );
    write(
        "etc/codex/requirements.toml",
        format!(
            "allowed_sandbox_modes = [\"read-only\"]\ndefault_permissions = \"{MANAGED_SECRET}\"\n\
             \"{MANAGED_SECRET}\\nproblems: forged\" = true\n"
        ),
    );
    let system = SystemPaths {
        root: root.path().to_path_buf(),
        platform: Platform::Macos,
    };
    let mut report = full_report();
    for found in &mut report.providers {
        found.managed_settings = scan(found.provider, &system);
    }
    (root, report)
}

#[test]
fn managed_settings_are_shown_by_path_and_key_name_and_never_by_value() {
    let (root, report) = report_with_managed_settings();
    let shown = report.to_text();
    let dir = root
        .path()
        .join("Library/Application Support/ClaudeCode")
        .display()
        .to_string();
    for expected in [
        "  managed settings: 5 found\n".to_string(),
        "    These managed keys can change how Claude Code treats what delega passes at launch; \
         see https://code.claude.com/docs/en/settings-reference.\n"
            .to_string(),
        "doctor does not say whether a key is on".to_string(),
        "docs/providers.md".to_string(),
        format!("    {dir}/managed-settings.json (read)"),
        "      permissions: allowManagedPermissionRulesOnly, permissions.deny\n".to_string(),
        "      authentication: apiKeyHelper\n".to_string(),
        "      environment: env\n".to_string(),
        format!("    {dir}/managed-settings.d (partly read; 2 files, 1 not read)\n"),
        "      hooks: disableAllHooks\n".to_string(),
        "Library/Managed Preferences (present, not inspected; 1 file)\n".to_string(),
        format!("    {dir}/managed-mcp.json (present)"),
        "com.anthropic.claudecode.plist (present, not inspected)".to_string(),
        "  managed settings: 1 found\n".to_string(),
        "    These managed keys can change how Codex treats what delega passes at launch; see "
            .to_string(),
        "etc/codex/requirements.toml (read)".to_string(),
        "      codex_requirements: allowed_sandbox_modes, default_permissions\n".to_string(),
        "      1 other top-level key is not named: doctor names only keys it knows".to_string(),
    ] {
        assert!(shown.contains(&expected), "{expected}\n{shown}");
    }
    assert!(!shown.contains(MANAGED_SECRET), "{shown}");
    // No name read from a directory is shown, so none can forge a line.
    assert!(!shown.contains("forged"), "{shown}");
    assert_eq!(
        shown.lines().filter(|l| l.starts_with("problems")).count(),
        1,
        "{shown}"
    );
    // A key that does not affect a launch is not shown.
    assert!(!shown.contains("model:"), "{shown}");
}

#[test]
fn the_json_names_managed_keys_and_holds_no_value_and_satisfies_the_schema() {
    let (_root, report) = report_with_managed_settings();
    let json = report.to_json();
    assert!(!json.contains(MANAGED_SECRET), "{json}");
    assert!(!json.contains("forged"), "{json}");
    let document: Value = serde_json::from_str(&json).expect("report is JSON");
    let schema: Value = serde_json::from_str(DOCTOR_SCHEMA).expect("schema is JSON");
    let validator = jsonschema::validator_for(&schema).expect("compile schema");
    assert!(validator.is_valid(&document), "{document}");
    let claude = document["providers"][0]["managed_settings"]
        .as_array()
        .expect("sources");
    let shape: Vec<(&str, &str)> = claude
        .iter()
        .map(|s| {
            (
                s["kind"].as_str().expect("kind"),
                s["state"].as_str().expect("state"),
            )
        })
        .collect();
    assert_eq!(
        shape,
        [
            ("settings_file", "read"),
            ("settings_drop_ins", "partly_read"),
            ("mcp_file", "read"),
            ("managed_preferences", "not_inspected"),
            ("user_managed_preferences", "not_inspected"),
        ]
    );
    let names: Vec<&str> = claude[0]["keys"]
        .as_array()
        .expect("keys")
        .iter()
        .map(|k| k["name"].as_str().expect("name"))
        .collect();
    assert_eq!(
        names,
        [
            "allowManagedPermissionRulesOnly",
            "permissions.deny",
            "apiKeyHelper",
            "env"
        ]
    );
    // A key is a name and a category, nothing else: no sentence about it.
    for key in claude[0]["keys"].as_array().expect("keys") {
        let fields: Vec<&String> = key.as_object().expect("object").keys().collect();
        assert_eq!(fields, ["category", "name"]);
        assert!(key["category"].as_str().is_some_and(|c| !c.contains(' ')));
    }
    assert_eq!(claude[0]["files"], Value::Null);
    assert_eq!(claude[1]["keys"][0]["name"], "disableAllHooks");
    assert_eq!(
        claude[1]["files"],
        json!({"found": 2, "limit_reached": false, "unreadable": 1})
    );
    assert_eq!(claude[1]["note"], Value::Null);
    assert_eq!(claude[4]["files"]["found"], 1);
    // Every path ends in a fixed name, never one read from a directory.
    for (source, end) in claude.iter().zip([
        "/ClaudeCode/managed-settings.json",
        "/ClaudeCode/managed-settings.d",
        "/ClaudeCode/managed-mcp.json",
        "/Managed Preferences/com.anthropic.claudecode.plist",
        "/Library/Managed Preferences",
    ]) {
        let path = source["path"].as_str().expect("path");
        assert!(path.ends_with(end), "{path}");
    }
    let codex = &document["providers"][1]["managed_settings"];
    assert_eq!(codex[0]["kind"], "requirements_file");
    assert_eq!(codex[0]["keys"][0]["name"], "allowed_sandbox_modes");
    assert_eq!(codex[0]["keys"][1]["name"], "default_permissions");
    // The key with a hostile name is counted and nowhere named.
    assert_eq!(codex[0]["keys"].as_array().expect("keys").len(), 2);
    assert_eq!(codex[0]["other_keys"], 1);
    assert_eq!(claude[0]["other_keys"], 0);
}

#[test]
fn managed_settings_are_a_notice_that_adds_no_problem_and_keeps_the_exit_status() {
    let plain = full_report();
    let (_root, report) = report_with_managed_settings();
    assert_eq!(report.problems, plain.problems);
    assert_eq!(report.ready, plain.ready);
    assert_eq!(report.exit_status(), plain.exit_status());
    let mut ready = report;
    ready.ready = true;
    assert_eq!(ready.exit_status(), 0);
}

#[test]
fn no_managed_settings_block_is_shown_when_none_was_found() {
    let report = full_report();
    assert!(!report.to_text().contains("managed settings"));
    let document: Value = serde_json::from_str(&report.to_json()).expect("JSON");
    assert_eq!(document["providers"][0]["managed_settings"], json!([]));
    assert_eq!(document["providers"][1]["managed_settings"], json!([]));
}

#[test]
fn a_codex_file_with_only_unrecognized_keys_is_said_to_hold_none_doctor_recognizes() {
    use crate::provider::managed::{Platform, SystemPaths, scan};
    let root = tempfile::tempdir().expect("temporary root");
    std::fs::create_dir_all(root.path().join("etc/codex")).expect("create directory");
    std::fs::write(
        root.path().join("etc/codex/managed_config.toml"),
        "model = \"x\"\nmodel_provider = \"y\"\n",
    )
    .expect("write file");
    std::fs::write(root.path().join("etc/codex/requirements.toml"), "").expect("write file");
    let system = SystemPaths {
        root: root.path().to_path_buf(),
        platform: Platform::Linux,
    };
    let mut report = full_report();
    report.providers[1].managed_settings = scan(Provider::Codex, &system);
    let shown = report.to_text();
    // The file may well change a launch: doctor only says it knows none of
    // its keys, and how many there are.
    for expected in [
        "managed_config.toml (read, holds no keys doctor recognizes)\n",
        "      2 other top-level keys are not named: ",
        "requirements.toml (read, holds none of the keys doctor lists)\n",
    ] {
        assert!(shown.contains(expected), "{expected}\n{shown}");
    }
    assert!(!shown.contains("sets nothing"), "{shown}");
}

#[test]
fn the_text_says_once_per_provider_what_managed_keys_can_do_and_nothing_per_key() {
    let (_root, report) = report_with_managed_settings();
    let shown = report.to_text();
    // One sentence for each provider that has sources, whatever their number.
    assert_eq!(
        shown.matches("These managed keys can change how").count(),
        2
    );
    assert_eq!(shown.matches("settings-reference").count(), 1);
    // A key line is a category and names: no verb says what a key does.
    for line in shown.lines().filter(|l| l.starts_with("      ")) {
        for claim in [
            "ignored", "refuse", "reject", "enforce", "limits", "replaces",
        ] {
            assert!(!line.contains(claim), "{line}");
        }
    }
}

#[test]
fn a_single_file_that_changed_under_the_read_is_not_said_to_have_more_files() {
    use crate::provider::managed::{
        ManagedFileCounts, ManagedSource, ManagedSourceKind, ManagedSourceState,
    };
    let counts = ManagedFileCounts {
        found: 1,
        unreadable: 1,
        limit_reached: true,
    };
    let mut report = full_report();
    report.providers[1].managed_settings = vec![
        ManagedSource {
            path: "/etc/codex/requirements.toml".to_string(),
            kind: ManagedSourceKind::RequirementsFile,
            state: ManagedSourceState::PartlyRead,
            files: Some(counts),
            note: Some("the file changed size while doctor read it".to_string()),
            keys: Vec::new(),
            other_keys: 0,
        },
        ManagedSource {
            path: "/Library/Managed Preferences".to_string(),
            kind: ManagedSourceKind::UserManagedPreferences,
            state: ManagedSourceState::PartlyRead,
            files: Some(ManagedFileCounts {
                found: 0,
                unreadable: 0,
                limit_reached: true,
            }),
            note: None,
            keys: Vec::new(),
            other_keys: 0,
        },
    ];
    let shown = report.to_text();
    // One file: its state and its note, no count and no talk of more files.
    assert!(
        shown.contains(
            "    /etc/codex/requirements.toml (partly read)\n      the file changed size while \
             doctor read it\n"
        ),
        "{shown}"
    );
    // A directory still says so.
    assert!(
        shown.contains(
            "    /Library/Managed Preferences (partly read; 0 files, a limit was reached so \
             there may be more)\n"
        ),
        "{shown}"
    );
    assert_eq!(shown.matches("there may be more").count(), 1, "{shown}");
}
