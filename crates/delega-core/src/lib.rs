//! The delega library: the request and result contract and, later, the engine
//! that runs a provider as a worker.
//!
//! `docs/api.md` in the repository is the agreed design this crate follows.
//! Only the contract in [`contract`] exists so far.

pub mod canonical;

pub mod config;

pub mod contract;

pub mod doctor;

pub mod process;

pub mod profile;

pub mod provider;

pub mod skills;

pub mod supervisor;

#[cfg(test)]
mod test_spawn;

/// The version of this build, as set in the workspace manifest.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    /// The workspace version, read from the manifest at the repository root.
    fn workspace_manifest_version() -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
        let manifest = std::fs::read_to_string(path).expect("read workspace manifest");
        let mut in_package = false;
        for line in manifest.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_package = line == "[workspace.package]";
            } else if in_package && let Some(rest) = line.strip_prefix("version") {
                let value = rest.trim_start().trim_start_matches('=').trim();
                return value.trim_matches('"').to_string();
            }
        }
        panic!("no version under [workspace.package]");
    }

    #[test]
    fn version_matches_the_workspace_manifest() {
        assert_eq!(super::VERSION, workspace_manifest_version());
    }

    #[test]
    fn version_is_major_minor_patch_with_optional_suffix() {
        let core = super::VERSION
            .split(['-', '+'])
            .next()
            .expect("split yields one item");
        let parts: Vec<&str> = core.split('.').collect();
        assert_eq!(parts.len(), 3, "expected MAJOR.MINOR.PATCH in {core}");
        assert!(
            parts
                .iter()
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())),
            "non-numeric version part in {core}"
        );
    }
}
