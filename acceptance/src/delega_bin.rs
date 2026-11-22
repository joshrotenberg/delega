//! Finding the built `delega` binary.
//!
//! The harness drives the binary as a black box. Unless a path is given, it
//! builds the `delega` package with the profile the harness itself was built
//! with and reads the executable path from cargo's JSON messages, so a stale
//! binary is never measured.

use std::path::{Path, PathBuf};
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
pub fn build(release: bool) -> Result<PathBuf, String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
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

/// Check for a managed MCP entry without opening it. A release binary resolves
/// the real system path during planning, so the harness needs it to be absent.
pub fn require_absent_managed_mcp(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Err(format!(
            "Claude cases cannot run on this machine with {} present. \
             Use the debug harness (omit --release and --delega) to redirect the system root.",
            path.display()
        )),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(format!(
            "Claude cases cannot run on this machine because {} could not be checked: {err}. \
             Use the debug harness (omit --release and --delega) to redirect the system root.",
            path.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::require_absent_managed_mcp;

    #[test]
    fn release_preflight_accepts_absence_but_rejects_even_a_dangling_managed_mcp_link() {
        let root = tempfile::tempdir().expect("temporary system root");
        let path = root.path().join("managed-mcp.json");
        assert_eq!(require_absent_managed_mcp(&path), Ok(()));
        std::os::unix::fs::symlink(root.path().join("missing"), &path).expect("create link");
        let error = require_absent_managed_mcp(&path).expect_err("reject the deployed link");
        assert!(error.contains(&path.display().to_string()), "{error}");
        assert!(error.contains("debug harness"), "{error}");
    }
}
