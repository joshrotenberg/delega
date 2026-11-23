//! JSON Schemas for the request and the result.
//!
//! The schemas are generated from the types. The committed copies under
//! `schemas/` are embedded in the build and are what `delega schema` prints;
//! a test fails when a committed copy differs from the generated schema.
//!
//! To regenerate after changing a contract type, run
//! `DELEGA_UPDATE_SCHEMAS=1 cargo test -p delega-core committed_schemas`
//! and commit the changed files. Embedding reads the files at compile time, so
//! rerun the tests once more afterwards.

use schemars::Schema;
use serde_json::{Value, json};

use super::SCHEMA_VERSION;
use super::limits::*;
use super::request::Request;
use super::result::RunResult;

/// The committed request schema, embedded at build time.
pub const REQUEST_SCHEMA: &str = include_str!("../../schemas/request.schema.json");
/// The committed result schema, embedded at build time.
pub const RESULT_SCHEMA: &str = include_str!("../../schemas/result.schema.json");

// The schema states what a document may look like: structure, types, enums,
// numeric ranges, list lengths, the schema version, the rule that
// `ephemeral` and `resume` exclude each other and the rule that `build` takes
// no `commands`. It does not state content rules such as "not blank" or
// byte-length limits on strings; `Request::validate` enforces those. The
// refinements below are the one place the generated schema is adjusted, so
// that the limits in `limits.rs` and the schema cannot drift.
// One known difference: a JSON Schema `integer` also accepts `1.0`, which
// serde rejects.

/// Set `key` on the object at `pointer`. Panics when the pointer matches no
/// object, so a renamed field cannot silently lose its refinement.
fn set(schema: &mut Schema, pointer: &str, key: &str, value: Value) {
    match schema.pointer_mut(pointer) {
        Some(Value::Object(target)) => {
            target.insert(key.to_string(), value);
        }
        _ => panic!("schema refinement: no object at {pointer}"),
    }
}

/// Add the version constant, the timeout range, the `native_args` length and
/// the type of `answer_schema`.
pub(super) fn refine_request(schema: &mut Schema) {
    set(
        schema,
        "/properties/schema_version",
        "const",
        json!(SCHEMA_VERSION),
    );
    set(
        schema,
        "/properties/timeout_secs",
        "minimum",
        json!(MIN_TIMEOUT_SECS),
    );
    set(
        schema,
        "/properties/timeout_secs",
        "maximum",
        json!(MAX_TIMEOUT_SECS),
    );
    set(
        schema,
        "/properties/native_args",
        "maxItems",
        json!(MAX_NATIVE_ARGS),
    );
    // `validate` requires a JSON Schema, which is an object or a boolean.
    set(
        schema,
        "/properties/answer_schema",
        "type",
        json!(["object", "boolean", "null"]),
    );
}

/// The same refinements for the partial request that the MCP `start` tool and
/// profiles use, so the tool's schema states the ranges and lengths of the
/// request's. Every field of a partial request may be absent or `null`, so the
/// version is `1` or `null` instead of the constant `1`.
pub(super) fn refine_partial_request(schema: &mut Schema) {
    refine_request(schema);
    match schema.pointer_mut("/properties/schema_version") {
        Some(Value::Object(target)) => {
            target.remove("const");
            target.insert("enum".to_string(), json!([SCHEMA_VERSION, null]));
        }
        _ => panic!("schema refinement: no object at /properties/schema_version"),
    }
}

/// A confirmed cleanup has no leftovers; an unconfirmed one gives a reason.
pub(super) fn refine_cleanup(schema: &mut Schema) {
    schema.insert(
        "if".to_string(),
        json!({"properties": {"confirmed": {"const": true}}, "required": ["confirmed"]}),
    );
    schema.insert(
        "then".to_string(),
        json!({"properties": {
            "pids": {"maxItems": 0},
            "process_groups": {"maxItems": 0},
            "reason": {"type": "null"}
        }}),
    );
    schema.insert(
        "else".to_string(),
        json!({"required": ["reason"], "properties": {"reason": {"type": "string"}}}),
    );
}

/// The invariants of a result that are structural: the version, `error`
/// present exactly when not completed, `provider` present unless refused, and
/// `structured_valid` only with `structured`. A `null` counts as absent, as it
/// does when reading.
pub(super) fn refine_result(schema: &mut Schema) {
    set(
        schema,
        "/properties/schema_version",
        "const",
        json!(SCHEMA_VERSION),
    );
    schema.insert(
        "allOf".to_string(),
        json!([
            {
                "if": {"properties": {"outcome": {"const": "completed"}}, "required": ["outcome"]},
                "then": {"properties": {"error": {"type": "null"}}},
                "else": {"required": ["error"], "properties": {"error": {"not": {"type": "null"}}}}
            },
            {
                "if": {"properties": {"outcome": {"const": "refused"}}, "required": ["outcome"]},
                "else": {"required": ["provider"], "properties": {"provider": {"not": {"type": "null"}}}}
            },
            {
                "if": {"required": ["structured_valid"], "properties": {"structured_valid": {"type": "boolean"}}},
                "then": {"required": ["structured"], "properties": {"structured": {"not": {"type": "null"}}}}
            }
        ]),
    );
}

/// Add the list lengths for commands and directories.
pub(super) fn refine_permissions(schema: &mut Schema) {
    set(
        schema,
        "/properties/commands",
        "maxItems",
        json!(MAX_COMMANDS),
    );
    set(
        schema,
        "/properties/directories",
        "maxItems",
        json!(MAX_DIRECTORIES),
    );
    // `build` runs every command inside the sandbox, so it takes no command list.
    schema.insert(
        "not".to_string(),
        json!({
            "required": ["mode", "commands"],
            "properties": {
                "mode": {"const": "build"},
                "commands": {"type": "array", "minItems": 1}
            }
        }),
    );
}

/// Exclude `ephemeral: true` together with a `resume` id.
pub(super) fn refine_session(schema: &mut Schema) {
    schema.insert(
        "not".to_string(),
        json!({
            "required": ["ephemeral", "resume"],
            "properties": {
                "ephemeral": {"const": true},
                "resume": {"type": "string"}
            }
        }),
    );
}

/// Generate the request schema from the type, as pretty JSON ending in a
/// newline.
pub fn generate_request_schema() -> String {
    render(&schemars::schema_for!(Request))
}

/// Generate the result schema from the type, as pretty JSON ending in a
/// newline.
pub fn generate_result_schema() -> String {
    render(&schemars::schema_for!(RunResult))
}

fn render(schema: &schemars::Schema) -> String {
    // Back to a `Schema`, so that its own ordering of the well-known keywords
    // (`$schema`, `title`, `description`, `type`, ...) still applies.
    let canonical = Schema::try_from(crate::canonical::sorted(schema.clone().to_value()))
        .unwrap_or_else(|_| schema.clone());
    // A schema is a JSON value with string keys, so serializing cannot fail.
    let mut text = serde_json::to_string_pretty(&canonical).unwrap_or_default();
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    const REGENERATE: &str = "DELEGA_UPDATE_SCHEMAS=1 cargo test -p delega-core committed_schemas";

    fn schema_path(file: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("schemas")
            .join(file)
    }

    fn result_with(edit: impl FnOnce(&mut Value)) -> Value {
        let mut doc = json!({
            "schema_version": 1, "run": "r", "provider": "claude",
            "outcome": "completed", "text": "",
            "cleanup": {"confirmed": true},
            "timing": {"queued_at": "t", "finished_at": "t", "duration_ms": 0, "timeout_secs": 600},
            "provenance": {"delega_version": "0.1.0"}
        });
        edit(&mut doc);
        doc
    }

    fn remove(doc: &mut Value, key: &str) {
        doc.as_object_mut().unwrap().remove(key);
    }

    /// The same agreement check for results.
    #[test]
    fn result_schema_agrees_with_serde_and_validate() {
        let schema: Value = serde_json::from_str(RESULT_SCHEMA).unwrap();
        let validator = jsonschema::validator_for(&schema).expect("compile schema");
        let err = || json!({"kind": "k", "message": "m"});
        type Edit = Box<dyn Fn(&mut Value)>;
        let cases: Vec<(&str, Edit, bool)> = vec![
            ("completed", Box::new(|_| {}), true),
            (
                "version 2",
                Box::new(|d| d["schema_version"] = json!(2)),
                false,
            ),
            (
                "version missing",
                Box::new(|d| remove(d, "schema_version")),
                false,
            ),
            (
                "completed with error",
                Box::new(move |d| d["error"] = err()),
                false,
            ),
            (
                "completed with null error",
                Box::new(|d| d["error"] = json!(null)),
                true,
            ),
            (
                "failed with error",
                Box::new(move |d| {
                    d["outcome"] = json!("failed");
                    d["error"] = err();
                }),
                true,
            ),
            (
                "failed without error",
                Box::new(|d| d["outcome"] = json!("failed")),
                false,
            ),
            (
                "failed with null error",
                Box::new(|d| {
                    d["outcome"] = json!("failed");
                    d["error"] = json!(null);
                }),
                false,
            ),
            (
                "timed_out without error",
                Box::new(|d| d["outcome"] = json!("timed_out")),
                false,
            ),
            (
                "unknown outcome",
                Box::new(|d| d["outcome"] = json!("done")),
                false,
            ),
            (
                "refused without provider",
                Box::new(move |d| {
                    d["outcome"] = json!("refused");
                    d["error"] = err();
                    remove(d, "provider");
                }),
                true,
            ),
            (
                "refused with started_at",
                Box::new(move |d| {
                    d["outcome"] = json!("refused");
                    d["error"] = err();
                    d["timing"]["started_at"] = json!("t");
                }),
                true,
            ),
            (
                "completed without provider",
                Box::new(|d| remove(d, "provider")),
                false,
            ),
            (
                "completed with null provider",
                Box::new(|d| d["provider"] = json!(null)),
                false,
            ),
            (
                "unknown provider",
                Box::new(|d| d["provider"] = json!("gemini")),
                false,
            ),
            (
                "cleanup unconfirmed",
                Box::new(|d| {
                    d["cleanup"] = json!({"confirmed": false, "pids": [1], "reason": "r"})
                }),
                true,
            ),
            (
                "cleanup unconfirmed without reason",
                Box::new(|d| d["cleanup"] = json!({"confirmed": false, "pids": [1]})),
                false,
            ),
            (
                "cleanup confirmed with pids",
                Box::new(|d| d["cleanup"] = json!({"confirmed": true, "pids": [1]})),
                false,
            ),
            (
                "cleanup confirmed with groups",
                Box::new(|d| d["cleanup"] = json!({"confirmed": true, "process_groups": [1]})),
                false,
            ),
            (
                "cleanup confirmed with reason",
                Box::new(|d| d["cleanup"] = json!({"confirmed": true, "reason": "r"})),
                false,
            ),
            (
                "cleanup confirmed with empty lists",
                Box::new(|d| d["cleanup"] = json!({"confirmed": true, "pids": []})),
                true,
            ),
            (
                "structured with valid",
                Box::new(|d| {
                    d["structured"] = json!({"a": 1});
                    d["structured_valid"] = json!(true);
                }),
                true,
            ),
            (
                "structured alone",
                Box::new(|d| d["structured"] = json!(1)),
                true,
            ),
            (
                "valid without structured",
                Box::new(|d| d["structured_valid"] = json!(true)),
                false,
            ),
            (
                "valid with null structured",
                Box::new(|d| {
                    d["structured"] = json!(null);
                    d["structured_valid"] = json!(false);
                }),
                false,
            ),
            (
                "unknown field is tolerated",
                Box::new(|d| d["added_later"] = json!(1)),
                true,
            ),
            (
                "negative duration",
                Box::new(|d| d["timing"]["duration_ms"] = json!(-1)),
                false,
            ),
        ];
        for (name, edit, expected) in cases {
            let doc = result_with(|d| edit(d));
            let by_schema = validator.is_valid(&doc);
            let by_serde = serde_json::from_value::<RunResult>(doc.clone())
                .map(|r| r.validate().is_ok())
                .unwrap_or(false);
            assert_eq!(by_schema, by_serde, "schema and serde disagree on {name}");
            assert_eq!(by_serde, expected, "unexpected verdict for {name}");
        }
    }

    #[test]
    fn committed_schemas_match_the_generated_schemas() {
        let cases = [
            (
                "request.schema.json",
                generate_request_schema(),
                REQUEST_SCHEMA,
            ),
            (
                "result.schema.json",
                generate_result_schema(),
                RESULT_SCHEMA,
            ),
        ];
        let update = std::env::var_os("DELEGA_UPDATE_SCHEMAS").is_some();
        let mut stale = Vec::new();
        for (file, generated, committed) in cases {
            if generated == committed {
                continue;
            }
            if update {
                std::fs::write(schema_path(file), &generated).expect("write schema file");
            } else {
                stale.push(file);
            }
        }
        assert!(
            stale.is_empty(),
            "committed schema out of date: {stale:?}. Regenerate with `{REGENERATE}` and commit the result."
        );
    }

    fn request_with(extra: Value) -> Value {
        let mut doc = json!({"provider": "claude", "prompt": "do it"});
        for (k, v) in extra.as_object().expect("object").iter() {
            doc[k] = v.clone();
        }
        doc
    }

    /// The schema accepts a document exactly when serde plus `validate` do, for
    /// every case the schema is meant to decide.
    #[test]
    fn request_schema_agrees_with_serde_and_validate() {
        let schema: Value = serde_json::from_str(REQUEST_SCHEMA).unwrap();
        let validator = jsonschema::validator_for(&schema).expect("compile schema");
        let many = |n: usize, v: Value| Value::Array(vec![v; n]);
        let cases: Vec<(&str, Value, bool)> = vec![
            ("minimal", request_with(json!({})), true),
            (
                "version as string",
                request_with(json!({"schema_version": "1"})),
                false,
            ),
            (
                "answer_schema object",
                request_with(json!({"answer_schema": {"type": "string"}})),
                true,
            ),
            (
                "answer_schema boolean",
                request_with(json!({"answer_schema": true})),
                true,
            ),
            (
                "answer_schema null",
                request_with(json!({"answer_schema": null})),
                true,
            ),
            (
                "answer_schema string",
                request_with(json!({"answer_schema": "x"})),
                false,
            ),
            (
                "answer_schema number",
                request_with(json!({"answer_schema": 3})),
                false,
            ),
            (
                "answer_schema array",
                request_with(json!({"answer_schema": [1]})),
                false,
            ),
            (
                "version 1",
                request_with(json!({"schema_version": 1})),
                true,
            ),
            (
                "version 2",
                request_with(json!({"schema_version": 2})),
                false,
            ),
            (
                "version 0",
                request_with(json!({"schema_version": 0})),
                false,
            ),
            (
                "timeout minimum",
                request_with(json!({"timeout_secs": 1})),
                true,
            ),
            (
                "timeout maximum",
                request_with(json!({"timeout_secs": 86400})),
                true,
            ),
            (
                "timeout zero",
                request_with(json!({"timeout_secs": 0})),
                false,
            ),
            (
                "timeout too large",
                request_with(json!({"timeout_secs": 86401})),
                false,
            ),
            (
                "timeout negative",
                request_with(json!({"timeout_secs": -1})),
                false,
            ),
            (
                "timeout fraction",
                request_with(json!({"timeout_secs": 1.5})),
                false,
            ),
            (
                "timeout string",
                request_with(json!({"timeout_secs": "10"})),
                false,
            ),
            (
                "old timeout name",
                request_with(json!({"timeout": 10})),
                false,
            ),
            ("unknown field", request_with(json!({"promt": "x"})), false),
            (
                "unknown provider",
                json!({"provider": "gemini", "prompt": "x"}),
                false,
            ),
            ("missing provider", json!({"prompt": "x"}), false),
            ("missing prompt", json!({"provider": "codex"}), false),
            (
                "prompt not a string",
                request_with(json!({"prompt": 3})),
                false,
            ),
            (
                "bad effort",
                request_with(json!({"effort": "extreme"})),
                false,
            ),
            (
                "bad mode",
                request_with(json!({"permissions": {"mode": "admin"}})),
                false,
            ),
            (
                "on_request ask",
                request_with(json!({"permissions": {"on_request": "ask"}})),
                false,
            ),
            (
                "unknown permissions field",
                request_with(json!({"permissions": {"mod": "write"}})),
                false,
            ),
            (
                "unknown session field",
                request_with(json!({"session": {"keep": true}})),
                false,
            ),
            (
                "ephemeral and resume",
                request_with(json!({"session": {"ephemeral": true, "resume": "s"}})),
                false,
            ),
            (
                "ephemeral false and resume",
                request_with(json!({"session": {"ephemeral": false, "resume": "s"}})),
                true,
            ),
            (
                "ephemeral and null resume",
                request_with(json!({"session": {"ephemeral": true, "resume": null}})),
                true,
            ),
            (
                "null optional",
                request_with(json!({"cwd": null, "model": null})),
                true,
            ),
            (
                "commands at limit",
                request_with(json!({"permissions": {"commands": many(MAX_COMMANDS, json!("ls"))}})),
                true,
            ),
            (
                "commands over limit",
                request_with(
                    json!({"permissions": {"commands": many(MAX_COMMANDS + 1, json!("ls"))}}),
                ),
                false,
            ),
            (
                "directories over limit",
                request_with(
                    json!({"permissions": {"directories": many(MAX_DIRECTORIES + 1, json!("/a"))}}),
                ),
                false,
            ),
            (
                "native_args over limit",
                request_with(json!({"native_args": many(MAX_NATIVE_ARGS + 1, json!("--a"))})),
                false,
            ),
            (
                "native_args not strings",
                request_with(json!({"native_args": [1]})),
                false,
            ),
        ];
        for (name, doc, expected) in cases {
            let by_schema = validator.is_valid(&doc);
            let by_serde = serde_json::from_value::<Request>(doc.clone())
                .map(|r| r.validate().is_ok())
                .unwrap_or(false);
            assert_eq!(by_schema, by_serde, "schema and serde disagree on {name}");
            assert_eq!(by_serde, expected, "unexpected verdict for {name}");
        }
    }
}
