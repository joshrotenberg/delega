//! The arguments of the tools other than `start`.
//!
//! Each type rejects unknown fields, and its schema says so, so a misspelled
//! argument is an error and not a silently ignored setting. `start` takes the
//! request fields and is read in [`crate::start`].

use delega_core::contract::limits::{MAX_TIMEOUT_SECS, MIN_TIMEOUT_SECS};
use delega_core::supervisor::Detail;
use schemars::{JsonSchema, Schema};
use serde::Deserialize;
use serde_json::{Value, json};

/// The range of `timeout_secs`, the contract's, so `follow_up` states what
/// `start` does.
fn timeout_range(schema: &mut Schema) {
    if let Some(Value::Object(target)) = schema.pointer_mut("/properties/timeout_secs") {
        target.insert("minimum".to_string(), json!(MIN_TIMEOUT_SECS));
        target.insert("maximum".to_string(), json!(MAX_TIMEOUT_SECS));
    }
}

/// Arguments of `follow_up`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(transform = timeout_range)]
pub struct FollowUpArgs {
    /// The worker to give another assignment.
    pub worker: String,
    /// The assignment. It continues the worker's provider session, so it can
    /// refer to earlier work.
    pub prompt: String,
    /// Time limit for this run in whole seconds, replacing the worker's.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// Arguments of `wait`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WaitArgs {
    /// Run ids. The wait ends when any of them finishes.
    #[serde(default)]
    pub runs: Vec<String>,
    /// Worker ids. The wait ends when the worker's current run finishes.
    #[serde(default)]
    pub workers: Vec<String>,
    /// How long to wait, in seconds. Default 30 unless the server's configuration file sets another, capped by the server's wait
    /// ceiling. A timeout is not an error.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// The `cursor` of an earlier `wait` or `read`. With it, the call returns
    /// at once when something named finished after that cursor.
    #[serde(default)]
    pub after: Option<u64>,
}

/// Arguments of `read`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadArgs {
    /// The run to read. Give this or `worker`, not both.
    #[serde(default)]
    pub run: Option<String>,
    /// The worker to read: its current run, or its latest. Give this or `run`.
    #[serde(default)]
    pub worker: Option<String>,
    /// `compact` (the default) or `full`.
    #[serde(default)]
    pub detail: Option<Detail>,
    /// The `cursor` of an earlier call. Only activity after it is returned.
    #[serde(default)]
    pub after: Option<u64>,
    /// Characters to skip in the answer text, to read the next window of a long
    /// answer. Use the run's id to page.
    #[serde(default)]
    pub text_offset: Option<usize>,
}

/// Arguments of `interrupt`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InterruptArgs {
    /// The run to stop. Give this or `worker`, not both.
    #[serde(default)]
    pub run: Option<String>,
    /// The worker whose current run to stop. Give this or `run`.
    #[serde(default)]
    pub worker: Option<String>,
}

/// Arguments of `close`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloseArgs {
    /// The worker to stop and free.
    pub worker: String,
}

/// `list` takes no arguments.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListArgs {}
