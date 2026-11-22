//! The committed schema and the shape of a report.

use super::*;
use crate::provider::doctor::{CommandHook, DenyInfo, EnvCounts};
use crate::provider::{DenyFile, SkippedFile};

const REGENERATE: &str = "DELEGA_UPDATE_SCHEMAS=1 cargo test -p delega-core committed_schemas";

#[test]
fn committed_schemas_doctor_matches_the_generated_schema() {
    let generated = generate_schema();
    if generated == DOCTOR_SCHEMA {
        return;
    }
    if std::env::var_os("DELEGA_UPDATE_SCHEMAS").is_some() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../schemas/doctor.schema.json");
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
                ready: true,
            },
            ProviderCheck {
                provider: Provider::Codex,
                executable: None,
                executable_source: None,
                version: None,
                command_hook: None,
                env: None,
                deny_rules: None,
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
