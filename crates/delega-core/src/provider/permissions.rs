//! The provider-neutral side of the permission mapping.
//!
//! [`scope`] turns the request's permissions into resolved paths and a command
//! list, and [`Mapping`] is what a run reports about how the provider was set
//! up. The provider-specific settings are built in the `claude` and `codex`
//! modules.

use std::path::PathBuf;

use serde::Serialize;

use crate::contract::permissions::Mode;
use crate::contract::request::Request;

use super::refusal::Refusal;

/// The permissions of a request with every path resolved.
#[derive(Debug, Clone)]
pub struct Scope {
    /// The mode requested.
    pub mode: Mode,
    /// The working directory, absolute with links resolved.
    pub cwd: PathBuf,
    /// Extra readable directories, absolute with links resolved, without
    /// duplicates and without the working directory.
    pub directories: Vec<PathBuf>,
    /// The allowed commands, without duplicates, in request order.
    pub commands: Vec<String>,
}

/// How a run's permissions were applied to the provider, for `--dry-run` and
/// for diagnosis. It holds settings, never environment values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Mapping {
    /// The mode requested and applied.
    pub mode: Mode,
    /// The commands the provider is set up to allow, matched exactly. Empty
    /// when none were requested, when the provider cannot restrict commands,
    /// or when the request could not be honoured and a note says why.
    pub commands: Vec<String>,
    /// The extra readable directories as resolved.
    pub directories: Vec<String>,
    /// The provider settings that carry the mapping, one `name=value` per line.
    pub settings: Vec<String>,
    /// What happens to a request that was not granted. Always `deny`.
    pub on_request: String,
}

/// Resolve the request's working directory, directories and commands.
///
/// The working directory defaults to this process's. A relative `cwd`
/// resolves against it, and a relative directory entry resolves against the
/// working directory. Each must exist and be a directory. This process's
/// directory is read only when `cwd` is absent or relative, so an absolute
/// `cwd` resolves even if that directory is gone.
pub(super) fn scope(request: &Request) -> Result<Scope, Refusal> {
    scope_from(request, std::env::current_dir)
}

/// [`scope`] with the source of this process's directory given, so a test can
/// make it unreadable without changing the test process's own directory.
fn scope_from(
    request: &Request,
    current_dir: impl FnOnce() -> std::io::Result<PathBuf>,
) -> Result<Scope, Refusal> {
    let base = || {
        current_dir().map_err(|e| {
            Refusal::new(
                "cwd_unusable",
                format!("the current directory cannot be read: {e}"),
            )
        })
    };
    let requested = match &request.cwd {
        Some(cwd) if cwd.is_absolute() => cwd.clone(),
        Some(cwd) => base()?.join(cwd),
        None => base()?,
    };
    let cwd = std::fs::canonicalize(&requested)
        .ok()
        .filter(|p| p.is_dir())
        .ok_or_else(|| {
            Refusal::new(
                "cwd_unusable",
                format!(
                    "cwd {} does not exist or is not a directory",
                    requested.display()
                ),
            )
        })?;

    let mut directories: Vec<PathBuf> = Vec::new();
    for (i, dir) in request.permissions.directories.iter().enumerate() {
        let resolved = std::fs::canonicalize(cwd.join(dir))
            .ok()
            .filter(|p| p.is_dir())
            .ok_or_else(|| {
                Refusal::new(
                    "directory_unusable",
                    format!(
                        "permissions.directories[{i}] ({}) does not exist or is not a directory",
                        dir.display()
                    ),
                )
            })?;
        if resolved != cwd && !directories.contains(&resolved) {
            directories.push(resolved);
        }
    }

    let mut commands: Vec<String> = Vec::new();
    for command in &request.permissions.commands {
        if !commands.contains(command) {
            commands.push(command.clone());
        }
    }

    Ok(Scope {
        mode: request.permissions.mode,
        cwd,
        directories,
        commands,
    })
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::path::PathBuf;

    use crate::contract::request::{Provider, Request};

    use super::scope_from;

    fn unreadable() -> io::Result<PathBuf> {
        Err(io::Error::from(io::ErrorKind::NotFound))
    }

    #[test]
    fn an_absolute_cwd_resolves_when_the_current_directory_cannot_be_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut request = Request::new(Provider::Codex, "p");
        request.cwd = Some(dir.path().to_path_buf());
        let scope = scope_from(&request, unreadable).expect("scope");
        assert_eq!(scope.cwd, std::fs::canonicalize(dir.path()).unwrap());
    }

    #[test]
    fn a_missing_or_relative_cwd_is_refused_when_the_current_directory_cannot_be_read() {
        for cwd in [None, Some(PathBuf::from("work"))] {
            let mut request = Request::new(Provider::Codex, "p");
            request.cwd = cwd;
            let refusal = scope_from(&request, unreadable).expect_err("refused");
            assert_eq!(refusal.kind, "cwd_unusable");
            assert!(
                refusal.message.contains("current directory cannot be read"),
                "{}",
                refusal.message
            );
        }
    }
}
