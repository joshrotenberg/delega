//! The files `delega init` writes besides the executable copy and the marker.

use delega_core::canonical::sorted;
use delega_core::skills;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{MARKETPLACE, PLUGIN, PLUGIN_DIR};

const DESCRIPTION: &str =
    "Run Claude Code and Codex workers from an agent, with the delega skills and MCP tools.";

/// The plugin version the hosts see: the delega version, a dot, and the full
/// SHA-256 of the executable that is copied. A host caches a plugin by
/// version, so a changed executable, even a different build of the same
/// source revision, must change it.
pub fn plugin_version(executable: &[u8]) -> String {
    let digest = Sha256::digest(executable);
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{}.{hex}", delega_core::VERSION)
}

/// A manifest as the bytes written: sorted keys, two-space indent, a newline.
fn document(value: Value) -> Vec<u8> {
    format!(
        "{}\n",
        serde_json::to_string_pretty(&sorted(value)).expect("serialize a manifest")
    )
    .into_bytes()
}

/// The MCP server entry: the copied executable with the argument `mcp`.
fn mcp_server(command: &str, root_macro: &str) -> Value {
    json!({
        "mcpServers": {
            PLUGIN: {
                "type": "stdio",
                "command": command,
                "args": ["mcp"],
                "cwd": root_macro,
            }
        }
    })
}

/// Every file written under the managed directory except the executable copy
/// and the marker, as (relative path, bytes). The skills have the bytes of
/// `delega skill show`, and `IDENTITY.json` the content of `delega version
/// --output json`.
pub fn payload(version: &str) -> Vec<(String, Vec<u8>)> {
    let mut codex_mcp = mcp_server("./bin/delega", "${PLUGIN_ROOT}");
    codex_mcp["$schema"] = json!("https://agent-plugins.org/schemas/1.0.0/mcp.schema.json");
    let mut files = vec![
        (
            ".claude-plugin/marketplace.json".to_string(),
            document(json!({
                "name": MARKETPLACE,
                "owner": {"name": PLUGIN},
                "description": "The delega plugin installed by delega init",
                "plugins": [{
                    "name": PLUGIN,
                    "source": format!("./{PLUGIN_DIR}"),
                    "version": version,
                    "description": DESCRIPTION,
                }],
            })),
        ),
        (
            ".agents/plugins/marketplace.json".to_string(),
            document(json!({
                "name": MARKETPLACE,
                "interface": {"displayName": "delega local"},
                "plugins": [{
                    "name": PLUGIN,
                    "source": {"source": "local", "path": format!("./{PLUGIN_DIR}")},
                    "policy": {"installation": "AVAILABLE", "authentication": "ON_INSTALL"},
                    "category": "Developer Tools",
                }],
            })),
        ),
        (
            format!("{PLUGIN_DIR}/plugin.json"),
            document(json!({
                "$schema": "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json",
                "name": PLUGIN,
                "version": version,
                "description": DESCRIPTION,
                "extensions": {
                    "com.openai": {
                        "requires_local_executor": true,
                        "interface": {
                            "displayName": "delega",
                            "shortDescription": "Claude and Codex CLI workers",
                            "category": "Developer Tools",
                            "defaultPrompt": "Use the delega tools to run independent workers and read their results.",
                        },
                    },
                },
            })),
        ),
        (
            format!("{PLUGIN_DIR}/.claude-plugin/plugin.json"),
            document(json!({
                "name": PLUGIN,
                "version": version,
                "description": DESCRIPTION,
            })),
        ),
        (format!("{PLUGIN_DIR}/mcp.json"), document(codex_mcp)),
        (
            format!("{PLUGIN_DIR}/.mcp.json"),
            document(mcp_server(
                "${CLAUDE_PLUGIN_ROOT}/bin/delega",
                "${CLAUDE_PLUGIN_ROOT}",
            )),
        ),
        (
            format!("{PLUGIN_DIR}/IDENTITY.json"),
            format!(
                "{}\n",
                delega_mcp::identity(&crate::commands::version::build_info())
            )
            .into_bytes(),
        ),
    ];
    for skill in skills::catalog() {
        files.push((
            format!("{PLUGIN_DIR}/skills/{}/SKILL.md", skill.name()),
            skill.content().as_bytes().to_vec(),
        ));
    }
    files
}
