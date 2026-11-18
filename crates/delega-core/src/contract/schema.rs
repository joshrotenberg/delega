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

use super::request::Request;
use super::result::RunResult;

/// The committed request schema, embedded at build time.
pub const REQUEST_SCHEMA: &str = include_str!("../../../../schemas/request.schema.json");
/// The committed result schema, embedded at build time.
pub const RESULT_SCHEMA: &str = include_str!("../../../../schemas/result.schema.json");

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

    #[test]
    fn embedded_schemas_are_json_objects() {
        for text in [REQUEST_SCHEMA, RESULT_SCHEMA] {
            let value: serde_json::Value = serde_json::from_str(text).expect("valid JSON");
            assert!(value.is_object());
        }
    }
}
