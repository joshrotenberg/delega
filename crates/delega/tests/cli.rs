//! Runs the built `delega` binary and checks its output and exit status.

use std::process::{Command, Output};

fn delega(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_delega"))
        .args(args)
        .output()
        .expect("run delega")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("utf-8 output")
}

#[test]
fn schema_request_prints_the_request_schema_and_exits_zero() {
    let out = delega(&["schema", "request"]);
    assert_eq!(out.status.code(), Some(0));
    let schema: serde_json::Value = serde_json::from_str(&text(&out.stdout)).expect("JSON");
    assert_eq!(schema["title"], "Request");
    assert_eq!(schema["additionalProperties"], false);
}

#[test]
fn schema_result_prints_the_result_schema_and_exits_zero() {
    let out = delega(&["schema", "result"]);
    assert_eq!(out.status.code(), Some(0));
    let schema: serde_json::Value = serde_json::from_str(&text(&out.stdout)).expect("JSON");
    assert_eq!(schema["title"], "RunResult");
}

#[test]
fn schema_output_equals_the_committed_files() {
    let request = include_str!("../../../schemas/request.schema.json");
    let result = include_str!("../../../schemas/result.schema.json");
    assert_eq!(text(&delega(&["schema", "request"]).stdout), request);
    assert_eq!(text(&delega(&["schema", "result"]).stdout), result);
}

#[test]
fn version_prints_the_core_version_and_exits_zero() {
    let out = delega(&["version"]);
    assert_eq!(out.status.code(), Some(0));
    // The workspace version is shared by every crate.
    assert_eq!(text(&out.stdout).trim(), env!("CARGO_PKG_VERSION"));
}

#[test]
fn version_json_has_name_version_and_schema_version() {
    let out = delega(&["version", "--output", "json"]);
    assert_eq!(out.status.code(), Some(0));
    let value: serde_json::Value = serde_json::from_str(&text(&out.stdout)).expect("JSON");
    assert_eq!(value["name"], "delega");
    assert_eq!(value["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(value["schema_version"], 1);
    // The revision is the commit this tree is at, or `unknown` outside the
    // repository. A stale value from an earlier commit fails here.
    let head = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string());
    let prefix = Command::new("git")
        .args(["rev-parse", "--show-prefix"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string());
    let expected = match (head, prefix.as_deref()) {
        (Some(head), Some("crates/delega/")) => head,
        _ => "unknown".to_string(),
    };
    assert_eq!(value["revision"], expected.as_str());
    assert!(value["features"].is_array());
}

#[test]
fn unimplemented_subcommands_say_so_on_stderr_and_exit_two() {
    for args in [
        &["run", "--provider", "codex", "-p", "hi"][..],
        &["mcp"],
        &["init", "--dry-run"],
        &["doctor"],
    ] {
        let out = delega(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(
            text(&out.stderr).contains("not implemented yet"),
            "{args:?}"
        );
        assert!(out.stdout.is_empty(), "{args:?} wrote to stdout");
    }
}

#[test]
fn skill_list_shows_both_embedded_skills_one_per_line() {
    let out = delega(&["skill", "list"]);
    assert_eq!(out.status.code(), Some(0));
    let listing = text(&out.stdout);
    let lines: Vec<&str> = listing.lines().collect();
    assert_eq!(lines.len(), 2, "{listing}");
    assert!(lines[0].starts_with("delegate "), "{listing}");
    assert!(lines[1].starts_with("run "), "{listing}");
    assert!(
        lines[1].contains("delega run"),
        "description missing: {listing}"
    );
}

#[test]
fn skill_list_json_is_a_versioned_catalog_with_sha256_digests() {
    let out = delega(&["skill", "list", "--output", "json"]);
    assert_eq!(out.status.code(), Some(0));
    let value: serde_json::Value = serde_json::from_str(&text(&out.stdout)).expect("JSON");
    assert_eq!(value["schema_version"], 1);
    let skills = value["skills"].as_array().expect("skills array");
    let names: Vec<&str> = skills
        .iter()
        .map(|s| s["name"].as_str().expect("name"))
        .collect();
    assert_eq!(names, ["delegate", "run"]);
    for skill in skills {
        let digest = skill["digest"].as_str().expect("digest");
        assert_eq!(digest.len(), 64, "{digest}");
        assert!(
            digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "{digest}"
        );
        let name = skill["name"].as_str().expect("name");
        assert_eq!(skill["uri"], format!("skill://delega/{name}/SKILL.md"));
        assert!(
            !skill["description"]
                .as_str()
                .expect("description")
                .is_empty()
        );
        assert!(skill.get("content").is_none());
    }
}

fn skill_file(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../skills")
        .join(name)
        .join("SKILL.md");
    std::fs::read(path).expect("read skill file")
}

#[test]
fn skill_show_prints_the_skill_file_byte_for_byte() {
    for name in ["run", "delegate"] {
        let out = delega(&["skill", "show", name]);
        assert_eq!(out.status.code(), Some(0), "{name}");
        assert_eq!(out.stdout, skill_file(name), "{name}");
    }
}

#[test]
fn skill_show_json_carries_the_content_and_catalog_fields() {
    let out = delega(&["skill", "show", "run", "--output", "json"]);
    assert_eq!(out.status.code(), Some(0));
    let value: serde_json::Value = serde_json::from_str(&text(&out.stdout)).expect("JSON");
    assert_eq!(value["name"], "run");
    assert_eq!(value["uri"], "skill://delega/run/SKILL.md");
    assert_eq!(
        value["content"].as_str().expect("content").as_bytes(),
        skill_file("run")
    );
    let listed: serde_json::Value = serde_json::from_str(&text(
        &delega(&["skill", "list", "--output", "json"]).stdout,
    ))
    .expect("JSON");
    let entry = listed["skills"]
        .as_array()
        .expect("skills")
        .iter()
        .find(|s| s["name"] == "run")
        .expect("run listed");
    assert_eq!(value["digest"], entry["digest"]);
    assert_eq!(value["description"], entry["description"]);
}

#[test]
fn skill_show_unknown_name_exits_two_naming_the_known_skills() {
    let out = delega(&["skill", "show", "nope"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let err = text(&out.stderr);
    assert!(err.contains("nope"), "{err}");
    assert!(err.contains("delegate") && err.contains("run"), "{err}");
}

#[test]
fn unknown_subcommand_is_a_usage_error_with_exit_two() {
    let out = delega(&["frobnicate"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stderr).contains("Usage"));
}

#[test]
fn bad_schema_argument_is_a_usage_error_with_exit_two() {
    let out = delega(&["schema", "response"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stderr).contains("invalid value"));
}

#[test]
fn conflicting_permission_flags_are_a_usage_error_with_exit_two() {
    let out = delega(&["run", "--write", "--full"]);
    assert_eq!(out.status.code(), Some(2));
    let err = text(&out.stderr);
    assert!(err.contains("cannot be used with"), "{err}");
    assert!(!err.contains("not implemented"), "{err}");
}

#[test]
fn resume_with_ephemeral_is_a_usage_error_with_exit_two() {
    let out = delega(&["run", "--resume", "s1", "--ephemeral"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stderr).contains("cannot be used with"));
}

#[test]
fn malformed_timeout_is_a_usage_error_naming_the_accepted_forms() {
    let out = delega(&["run", "--timeout", "soon"]);
    assert_eq!(out.status.code(), Some(2));
    let err = text(&out.stderr);
    assert!(err.contains("invalid duration"), "{err}");
    assert!(!err.contains("not implemented"), "{err}");
}

#[test]
fn timeout_forms_are_accepted_before_the_command_reports_not_implemented() {
    for t in ["90s", "20m", "2h", "45"] {
        let out = delega(&["run", "--timeout", t]);
        assert!(text(&out.stderr).contains("not implemented yet"), "{t}");
    }
}

#[test]
fn help_lists_every_planned_subcommand() {
    let out = delega(&["--help"]);
    assert_eq!(out.status.code(), Some(0));
    let help = text(&out.stdout);
    for name in ["run", "mcp", "init", "schema", "skill", "doctor", "version"] {
        assert!(
            help.lines().any(|l| l.trim_start().starts_with(name)),
            "help does not list {name}"
        );
    }
}
