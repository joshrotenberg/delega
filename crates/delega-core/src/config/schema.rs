//! The JSON Schema of the configuration file.
//!
//! The schema is generated from the types in [`super`] and refined here with
//! the bounds the file accepts, so the types, the committed schema
//! (`schemas/config.schema.json`) and the check that reads the file cannot
//! drift: [`super::check`] validates the file against this very schema.
//!
//! To regenerate the committed copy after changing a type, run
//! `DELEGA_UPDATE_SCHEMAS=1 cargo test -p delega-core committed_schemas` and
//! commit the changed file.

use schemars::Schema;
use serde_json::{Value, json};

use super::{
    Config, MAX_LIVE_WORKERS_BOUND, MAX_QUEUED_PER_WORKER_BOUND, MAX_RUNNING_RUNS_BOUND,
    SCHEMA_VERSION, WAIT_BOUND_SECS,
};
use crate::contract::limits::{MAX_TIMEOUT_SECS, MIN_TIMEOUT_SECS};

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

fn range(schema: &mut Schema, field: &str, min: u64, max: u64) {
    let pointer = format!("/properties/{field}");
    set(schema, &pointer, "minimum", json!(min));
    set(schema, &pointer, "maximum", json!(max));
}

/// The version constant.
pub(super) fn refine_config(schema: &mut Schema) {
    set(
        schema,
        "/properties/schema_version",
        "const",
        json!(SCHEMA_VERSION),
    );
}

/// The range of `timeout_secs`, the request's.
pub(super) fn refine_defaults(schema: &mut Schema) {
    range(schema, "timeout_secs", MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS);
}

/// The range of each limit.
pub(super) fn refine_mcp(schema: &mut Schema) {
    range(schema, "max_live_workers", 1, MAX_LIVE_WORKERS_BOUND as u64);
    range(schema, "max_running_runs", 1, MAX_RUNNING_RUNS_BOUND as u64);
    range(
        schema,
        "max_queued_per_worker",
        0,
        MAX_QUEUED_PER_WORKER_BOUND as u64,
    );
    range(schema, "wait_default_secs", 0, WAIT_BOUND_SECS);
    range(schema, "wait_ceiling_secs", 1, WAIT_BOUND_SECS);
}

/// The schema as the generator makes it, refined.
pub(super) fn schema() -> Schema {
    schemars::schema_for!(Config)
}

/// The schema as a value, for the validator.
pub(super) fn value() -> Value {
    schema().to_value()
}

/// The committed schema's text: pretty JSON with every object's keys in
/// alphabetical order, ending in a newline.
pub fn generate_schema() -> String {
    let schema = schema();
    let canonical = Schema::try_from(crate::canonical::sorted(schema.clone().to_value()))
        .unwrap_or_else(|_| schema.clone());
    let mut text = serde_json::to_string_pretty(&canonical).unwrap_or_default();
    text.push('\n');
    text
}
