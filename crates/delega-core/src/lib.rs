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
    fn read_manifest(path: &std::path::Path) -> toml::Table {
        std::fs::read_to_string(path)
            .expect("read manifest")
            .parse()
            .expect("parse manifest")
    }

    #[test]
    fn version_matches_the_manifest() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let package = read_manifest(&root.join("Cargo.toml"));
        let version = &package["package"]["version"];
        if let Some(version) = version.as_str() {
            // Cargo writes a normalized manifest with a literal version in the archive.
            assert_eq!(super::VERSION, version);
        } else {
            assert_eq!(version["workspace"].as_bool(), Some(true));
            let workspace = read_manifest(&root.join("../../Cargo.toml"));
            assert_eq!(
                Some(super::VERSION),
                workspace["workspace"]["package"]["version"].as_str()
            );
        }
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
