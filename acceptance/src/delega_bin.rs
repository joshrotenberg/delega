//! Finding the built `delega` binary.
//!
//! The harness drives the binary as a black box. Unless a path is given, it
//! builds the `delega` package with the profile the harness itself was built
//! with and reads the executable path from cargo's JSON messages, so a stale
//! binary is never measured.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::spawn::Guarded;

/// The workspace root, fixed at build time.
pub fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// Build `delega` and return its path.
pub fn build() -> Result<PathBuf, String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let release = std::env::current_exe()
        .map(|p| p.components().any(|c| c.as_os_str() == "release"))
        .unwrap_or(false);
    let mut command = Command::new(cargo);
    command
        .current_dir(workspace_root())
        .args([
            "build",
            "-p",
            "delega",
            "--message-format=json-render-diagnostics",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if release {
        command.arg("--release");
    }
    let out = command
        .guarded_output()
        .map_err(|e| format!("run cargo: {e}"))?;
    if !out.status.success() {
        return Err(format!("cargo build -p delega failed: {}", out.status));
    }
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if message["reason"] == "compiler-artifact"
            && message["target"]["name"] == "delega"
            && message["target"]["kind"]
                .as_array()
                .is_some_and(|k| k.iter().any(|k| k == "bin"))
            && let Some(path) = message["executable"].as_str()
        {
            return Ok(PathBuf::from(path));
        }
    }
    Err("cargo did not report the delega executable".to_string())
}
