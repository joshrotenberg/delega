//! The delega library: the request and result contract and, later, the engine
//! that runs a provider as a worker.
//!
//! Nothing is implemented yet. `docs/api.md` in the repository is the agreed
//! design this crate will follow.

/// The version of this build, as set in the workspace manifest.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    #[test]
    fn the_build_version_is_the_workspace_version() {
        assert!(!super::VERSION.is_empty());
        assert_eq!(super::VERSION, env!("CARGO_PKG_VERSION"));
    }
}
