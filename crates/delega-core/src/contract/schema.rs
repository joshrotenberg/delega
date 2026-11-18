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
pub const REQUEST_SCHEMA: &str = include_str!("../../../../schemas/request.schema.json");
/// The committed result schema, embedded at build time.
pub const RESULT_SCHEMA: &str = include_str!("../../../../schemas/result.schema.json");

// The schema states what a document may look like: structure, types, enums,
// numeric ranges, list lengths, the schema version and the rule that
// `ephemeral` and `resume` exclude each other. It does not state content rules
// such as "not blank" or byte-length limits on strings; `Request::validate`
// enforces those. The refinements below are the one place the generated schema
// is adjusted, so that the limits in `limits.rs` and the schema cannot drift.
// One known difference: a JSON Schema `integer` also accepts `1.0`, which
// serde rejects.

fn set(schema: &mut Schema, pointer: &str, key: &str, value: Value) {
    if let Some(Value::Object(target)) = schema.pointer_mut(pointer) {
        target.insert(key.to_string(), value);
    }
}

/// Add the version constant, the timeout range and the `native_args` length.
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
    // A schema is a JSON value with string keys, so serializing cannot fail.
    let mut text = serde_json::to_string_pretty(schema).unwrap_or_default();
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    const REGENERATE: &str = "DELEGA_UPDATE_SCHEMAS=1 cargo test -p delega-core committed_schemas";

    fn schema_path(file: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../schemas")
            .join(file)
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
            ("version omitted", request_with(json!({})), true),
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
