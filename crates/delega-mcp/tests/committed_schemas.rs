//! The committed schemas of the tools under `schemas/mcp/`.
//!
//! One file per tool, `<tool>.schema.json`, holding the tool's name and the
//! JSON Schema of its arguments (`inputSchema`) and of its structured result
//! (`outputSchema`), as the server lists them. A test fails when a committed
//! file differs from what the code generates, or when a file belongs to no tool.
//!
//! To regenerate after changing a tool or a type it returns, run
//! `DELEGA_UPDATE_SCHEMAS=1 cargo test -p delega-mcp committed_tool_schemas`
//! and commit the changed files.

#![cfg(unix)]

use std::path::PathBuf;

use serde_json::{Value, json};

fn directory() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schemas/mcp")
}

/// The text of the file for a tool: pretty JSON ending in a newline.
fn render(name: &str, input: &Value, output: &Value) -> String {
    let document = json!({"name": name, "inputSchema": input, "outputSchema": output});
    let mut text = serde_json::to_string_pretty(&document).expect("serialize schema");
    text.push('\n');
    text
}

#[test]
fn committed_tool_schemas_match_the_tools() {
    let update = std::env::var_os("DELEGA_UPDATE_SCHEMAS").is_some();
    let dir = directory();
    if update {
        std::fs::create_dir_all(&dir).expect("create schemas/mcp");
    }
    let mut expected_files = Vec::new();
    for schema in delega_mcp::tool_schemas() {
        let file = format!("{}.schema.json", schema.name);
        let text = render(&schema.name, &schema.input, &schema.output);
        let path = dir.join(&file);
        if update {
            std::fs::write(&path, &text).expect("write schema");
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_else(|err| {
            panic!(
                "{}: {err}; run with DELEGA_UPDATE_SCHEMAS=1",
                path.display()
            )
        });
        assert_eq!(
            committed, text,
            "{file} is stale; run `DELEGA_UPDATE_SCHEMAS=1 cargo test -p delega-mcp committed_tool_schemas`"
        );
        expected_files.push(file);
    }
    let mut present: Vec<String> = std::fs::read_dir(&dir)
        .expect("read schemas/mcp")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    present.sort();
    expected_files.sort();
    assert_eq!(
        present, expected_files,
        "schemas/mcp holds a file that is no tool's"
    );
}

#[test]
fn the_schemas_are_json_schemas_for_objects() {
    for schema in delega_mcp::tool_schemas() {
        for (what, document) in [("input", &schema.input), ("output", &schema.output)] {
            assert_eq!(document["type"], "object", "{} {what}", schema.name);
            assert!(
                document["$schema"]
                    .as_str()
                    .is_some_and(|s| s.contains("2020-12")),
                "{} {what}: {document}",
                schema.name
            );
        }
    }
}

#[test]
fn the_start_schema_has_the_request_fields_and_a_profile() {
    let schemas = delega_mcp::tool_schemas();
    let start = schemas.iter().find(|s| s.name == "start").expect("start");
    let properties = start.input["properties"].as_object().expect("properties");
    for field in [
        "provider",
        "prompt",
        "cwd",
        "name",
        "instructions",
        "model",
        "effort",
        "timeout_secs",
        "permissions",
        "answer_schema",
        "session",
        "native_args",
        "profile",
    ] {
        assert!(properties.contains_key(field), "{field}");
    }
}

#[test]
fn the_start_schema_states_the_ranges_of_the_request_schema() {
    use delega_core::contract::limits::{
        MAX_COMMANDS, MAX_DIRECTORIES, MAX_NATIVE_ARGS, MAX_TIMEOUT_SECS, MIN_TIMEOUT_SECS,
    };
    let schemas = delega_mcp::tool_schemas();
    let start = schemas.iter().find(|s| s.name == "start").expect("start");
    let properties = &start.input["properties"];
    // The numbers are the contract's, carried over by the same refinement the
    // request schema gets, not written again here.
    assert_eq!(properties["timeout_secs"]["minimum"], MIN_TIMEOUT_SECS);
    assert_eq!(properties["timeout_secs"]["maximum"], MAX_TIMEOUT_SECS);
    assert_eq!(properties["native_args"]["maxItems"], MAX_NATIVE_ARGS);
    let defs = &start.input["$defs"];
    assert_eq!(
        defs["PartialPermissions"]["properties"]["commands"]["maxItems"],
        MAX_COMMANDS
    );
    assert_eq!(
        defs["PartialPermissions"]["properties"]["directories"]["maxItems"],
        MAX_DIRECTORIES
    );
    assert!(defs["PartialSession"]["not"].is_object(), "{defs}");
    // Enums come with the types: effort and provider.
    let text = start.input.to_string();
    assert!(
        text.contains("\"low\"") && text.contains("\"codex\""),
        "{text}"
    );
}

#[test]
fn the_follow_up_schema_has_the_same_time_limit_range_as_start() {
    use delega_core::contract::limits::{MAX_TIMEOUT_SECS, MIN_TIMEOUT_SECS};
    let schemas = delega_mcp::tool_schemas();
    let range = |name: &str| {
        let tool = schemas.iter().find(|s| s.name == name).expect("tool");
        let property = &tool.input["properties"]["timeout_secs"];
        (property["minimum"].clone(), property["maximum"].clone())
    };
    assert_eq!(
        range("follow_up"),
        (MIN_TIMEOUT_SECS.into(), MAX_TIMEOUT_SECS.into())
    );
    assert_eq!(range("follow_up"), range("start"));
}
