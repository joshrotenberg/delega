//! The ownership marker, `.delega-install.json`, format version 1.
//!
//! It names every file `init` wrote (relative to the managed directory, the
//! marker itself excepted) and every host the plugin was registered with.
//! `init` reuses a directory only if it holds a valid marker, and `uninstall`
//! acts only on one.

use std::io::Read;
use std::path::Path;

use delega_core::canonical::sorted;
use serde_json::{Value, json};

use super::{EXECUTABLE, Host, MARKER, PLUGIN_DIR};

const MAX_BYTES: u64 = 1024 * 1024;

/// A validated marker.
#[derive(Debug, Clone)]
pub struct Marker {
    pub files: Vec<String>,
    pub hosts: Vec<Host>,
    raw: Value,
}

impl Marker {
    /// The same marker recording `hosts`, as the bytes to write.
    pub fn with_hosts(&self, hosts: &[Host]) -> Vec<u8> {
        let mut raw = self.raw.clone();
        raw["hosts"] = host_names(hosts);
        text(raw)
    }
}

fn host_names(hosts: &[Host]) -> Value {
    Value::Array(hosts.iter().map(|host| json!(host.name())).collect())
}

fn text(value: Value) -> Vec<u8> {
    format!(
        "{}\n",
        serde_json::to_string_pretty(&sorted(value)).expect("serialize the marker")
    )
    .into_bytes()
}

/// The marker for a new or updated install, as the bytes to write.
pub fn render(files: &[String], hosts: &[Host], plugin_version: &str) -> Vec<u8> {
    let build = crate::commands::version::build_info();
    text(json!({
        "format_version": 1,
        "managed_by": "delega",
        "version": delega_core::VERSION,
        "revision": build.revision,
        "plugin_version": plugin_version,
        "files": files,
        "hosts": host_names(hosts),
    }))
}

/// Whether `path` is a relative path of plain components inside a directory.
fn plain_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// Whether `init` of this build can write `path`: the marketplaces, the
/// executable and the plugin's manifests, identity and skills. Anything else a
/// marker names is not ours to delete.
pub fn writable(path: &str) -> bool {
    const FIXED: [&str; 3] = [
        ".claude-plugin/marketplace.json",
        ".agents/plugins/marketplace.json",
        EXECUTABLE,
    ];
    const PLUGIN_FILES: [&str; 5] = [
        "plugin.json",
        "mcp.json",
        ".mcp.json",
        ".claude-plugin/plugin.json",
        "IDENTITY.json",
    ];
    if FIXED.contains(&path) {
        return true;
    }
    let Some(inside) = path.strip_prefix(&format!("{PLUGIN_DIR}/")) else {
        return false;
    };
    if PLUGIN_FILES.contains(&inside) {
        return true;
    }
    // skills/<name>/SKILL.md
    let mut parts = inside.split('/');
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some("skills"), Some(name), Some("SKILL.md"), None)
            if !name.is_empty()
                && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    )
}

/// Read the marker in `dir`. `Ok(None)` when there is none; an error when
/// there is one that is not valid format 1.
pub fn read(dir: &Path) -> Result<Option<Marker>, String> {
    let path = dir.join(MARKER);
    let invalid = |reason: &str| {
        format!(
            "{} has no valid delega ownership marker: {reason}; host registrations and files were left unchanged",
            dir.display()
        )
    };
    let info = match std::fs::symlink_metadata(&path) {
        Ok(info) => info,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(invalid(&error.to_string())),
    };
    if !info.is_file() || info.len() > MAX_BYTES {
        return Err(invalid(
            "the marker must be a regular file of at most 1 MiB, not a symbolic link",
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&path)
        .map_err(|error| invalid(&error.to_string()))?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| invalid(&error.to_string()))?;
    let raw: Value = serde_json::from_slice(&bytes).map_err(|_| invalid("not valid JSON"))?;
    if !raw.is_object()
        || raw["managed_by"] != "delega"
        || raw["format_version"].as_u64() != Some(1)
    {
        return Err(invalid("unsupported managed_by or format_version"));
    }
    for field in ["version", "plugin_version"] {
        if raw[field].as_str().is_none_or(str::is_empty) {
            return Err(invalid(&format!("{field} must be a non-empty string")));
        }
    }
    let mut hosts = Vec::new();
    for host in raw["hosts"]
        .as_array()
        .ok_or_else(|| invalid("hosts must be an array"))?
    {
        let host = host
            .as_str()
            .and_then(Host::from_name)
            .filter(|host| !hosts.contains(host))
            .ok_or_else(|| invalid("hosts must hold each of claude and codex at most once"))?;
        hosts.push(host);
    }
    let mut files: Vec<String> = Vec::new();
    for file in raw["files"]
        .as_array()
        .ok_or_else(|| invalid("files must be an array"))?
    {
        let file = file
            .as_str()
            .filter(|file| plain_relative(file) && *file != MARKER)
            .ok_or_else(|| invalid("files must hold plain relative paths"))?;
        if !writable(file) {
            return Err(invalid(&format!(
                "{file} is not a path this delega writes, so it will not be deleted"
            )));
        }
        if files.iter().any(|seen| seen == file) {
            return Err(invalid("a file is listed twice"));
        }
        files.push(file.to_string());
    }
    Ok(Some(Marker { files, hosts, raw }))
}
