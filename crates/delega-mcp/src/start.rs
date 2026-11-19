//! The `start` arguments: a profile file and request fields, merged the way
//! `delega run` merges a profile and flags.

use std::path::PathBuf;

use delega_core::contract::partial::PartialRequest;
use delega_core::contract::request::Request;
use delega_core::contract::validate::Problem;
use delega_core::profile::{self, ErrorDetail};
use schemars::generate::SchemaSettings;
use serde_json::{Value, json};

/// What `start` reads: every request field, all optional, and `profile`.
const PROFILE_DESCRIPTION: &str = "Path of a profile file: a partial request saved as JSON, \
    with schema_version. Fields given in this call replace the profile's; lists replace, they \
    are not added. A relative path resolves against the server's working directory.";

/// The input schema of `start`: the schema of a partial request, which rejects
/// unknown fields, with `profile` added.
pub fn input_schema() -> Value {
    let root = SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<PartialRequest>();
    let mut schema = serde_json::to_value(root).unwrap_or_else(|_| json!({"type": "object"}));
    if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        properties.insert(
            "profile".to_string(),
            json!({"type": ["string", "null"], "description": PROFILE_DESCRIPTION}),
        );
    }
    schema
}

fn join(problems: &[Problem]) -> String {
    problems
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

/// Build the request from the arguments of `start`.
///
/// The profile, if named, is read first and every other argument replaces the
/// matching field, with the code `delega run` uses ([`profile::merge`]). Only a
/// request with no provider or no prompt cannot be built; any other problem is
/// left to the supervisor, which refuses the run, so the caller reads it with
/// `read` as it would any refusal. The error is the text of a tool error.
pub fn build_request(mut args: Value) -> Result<Request, String> {
    if args.is_null() {
        args = json!({});
    }
    let Some(fields) = args.as_object_mut() else {
        return Err("invalid arguments: expected an object".to_string());
    };
    let path = match fields.remove("profile") {
        None | Some(Value::Null) => None,
        Some(Value::String(path)) => Some(PathBuf::from(path)),
        Some(_) => return Err("invalid arguments: profile must be a path".to_string()),
    };
    let explicit: PartialRequest =
        serde_json::from_value(args).map_err(|err| format!("invalid arguments: {err}"))?;
    let mut problems = Vec::new();
    let merged = profile::merge(
        path.as_deref(),
        ErrorDetail::Position,
        &mut problems,
        |_| explicit,
    );
    if !problems.is_empty() {
        return Err(format!("invalid arguments: {}", join(&problems)));
    }
    merged
        .into_request_unvalidated()
        .map_err(|problems| format!("invalid arguments: {}", join(&problems)))
}
