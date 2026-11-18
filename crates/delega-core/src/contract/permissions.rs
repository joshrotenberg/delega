//! What a worker may do, in provider-neutral terms.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// How much a worker may do in its working directory.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// May read the working directory; no writes and no commands.
    #[default]
    Read,
    /// May edit files in the working directory.
    Write,
    /// No restriction from delega.
    Full,
}

/// What happens when the provider asks for something that was not granted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OnRequest {
    /// Answer no at once and record the request in the result. The only
    /// choice in this version; asking the caller comes later.
    #[default]
    Deny,
}

/// The permissions part of a request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(transform = super::schema::refine_permissions)]
pub struct Permissions {
    /// How much the worker may do. Defaults to `read`.
    #[serde(default)]
    pub mode: Mode,
    /// Shell commands the worker may run, matched exactly.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<String>,
    /// Extra directories the worker may read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub directories: Vec<PathBuf>,
    /// What happens to a request that was not granted. Defaults to `deny`.
    #[serde(default)]
    pub on_request: OnRequest,
}
