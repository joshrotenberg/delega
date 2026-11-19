//! What the server offers to read besides tools: the embedded skills and the
//! identity of this build.

use delega_core::contract::SCHEMA_VERSION;
use delega_core::skills;
use serde_json::{Value, json};
use tower_mcp::protocol::ReadResourceResult;
use tower_mcp::skills::{SkillBuilder, SkillError, SkillSet};
use tower_mcp::{Resource, ResourceBuilder};

/// The URI of the identity resource.
pub const IDENTITY_URI: &str = "delega://identity";

/// The facts of the build that the library cannot know: the source revision and
/// the Cargo features of the executable. The command line fills them in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildInfo {
    /// The source revision, or `unknown`.
    pub revision: String,
    /// The Cargo features the executable was built with.
    pub features: Vec<String>,
}

impl Default for BuildInfo {
    fn default() -> Self {
        Self {
            revision: "unknown".to_string(),
            features: Vec::new(),
        }
    }
}

/// The identity of this build: the version, the contract schema version, the
/// revision and features, and the SHA-256 of each embedded skill. `delega
/// version --output json` prints this same document, and the identity resource
/// of the MCP server serves it, so a caller can tell which guidance it has.
pub fn identity(build: &BuildInfo) -> Value {
    let catalog: Vec<Value> = skills::catalog()
        .iter()
        .map(|skill| {
            json!({
                "name": skill.name(),
                "uri": skill.uri(),
                "digest": skill.digest(),
            })
        })
        .collect();
    json!({
        "name": "delega",
        "version": delega_core::VERSION,
        "schema_version": SCHEMA_VERSION,
        "revision": build.revision,
        "features": build.features,
        "skills": catalog,
    })
}

/// The embedded skills for the MCP Skills extension: each skill is listed and
/// readable at `skill://delega/<name>/SKILL.md`, with the bytes `delega skill
/// show NAME` prints. The catalog holds only each skill's `SKILL.md`, so there
/// are no supporting files to serve.
pub(crate) fn skill_set() -> Result<SkillSet, SkillError> {
    let mut set = SkillSet::new();
    for skill in skills::catalog() {
        set = set.skill(
            SkillBuilder::new(format!("delega/{}", skill.name())).skill_md(skill.content()),
        )?;
    }
    Ok(set)
}

/// The identity resource.
pub(crate) fn identity_resource(build: &BuildInfo) -> Resource {
    let document = identity(build);
    ResourceBuilder::new(IDENTITY_URI)
        .name("identity")
        .description("The version, contract schema version and skill digests of this build")
        .mime_type("application/json")
        .handler(move || {
            let document = document.clone();
            async move { Ok(ReadResourceResult::json(IDENTITY_URI, &document)) }
        })
        .build()
}
