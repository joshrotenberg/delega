//! Finding the provider executable.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::contract::request::Provider;

use super::limits::VERSION_CHARS;
use super::options::ProviderOptions;
use super::redact::Redactor;
use super::refusal::{Refusal, clip};

/// The environment variable that names the executable of a provider.
pub fn override_variable(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude => "DELEGA_CLAUDE_BIN",
        Provider::Codex => "DELEGA_CODEX_BIN",
    }
}

/// The name searched on `PATH`.
pub fn default_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude => "claude",
        Provider::Codex => "codex",
    }
}

fn label(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude => "Claude Code",
        Provider::Codex => "Codex CLI",
    }
}

fn is_executable(path: &Path) -> bool {
    // Ask the operating system whether this user may execute it; a mode bit
    // alone does not establish that.
    std::fs::metadata(path).is_ok_and(|m| m.is_file())
        && rustix::fs::access(path, rustix::fs::Access::EXEC_OK).is_ok()
}

fn not_found(provider: Provider, detail: String) -> Refusal {
    Refusal::new(
        "provider_not_found",
        format!(
            "{} is not available: {detail}. Install it, or set {} to its absolute path.",
            label(provider),
            override_variable(provider)
        ),
    )
}

fn explicit(provider: Provider, path: &Path, source: &str) -> Result<PathBuf, Refusal> {
    if !path.is_absolute() {
        return Err(not_found(
            provider,
            format!("{source} must be an absolute path, got {}", path.display()),
        ));
    }
    if !is_executable(path) {
        return Err(not_found(
            provider,
            format!(
                "{} (from {source}) is not an executable file",
                path.display()
            ),
        ));
    }
    Ok(path.to_path_buf())
}

/// Resolve the executable: the option, then the override variable, then the
/// one a configuration file names, then the provider's name on the absolute
/// entries of `PATH`. Relative and empty
/// `PATH` entries are skipped, so a program in the working directory is never
/// picked up by accident.
pub(super) fn resolve(
    provider: Provider,
    options: &ProviderOptions,
    parent: &[(OsString, OsString)],
) -> Result<PathBuf, Refusal> {
    let lookup = |name: &str| {
        parent
            .iter()
            .rev()
            .find(|(n, _)| n.to_str() == Some(name))
            .map(|(_, v)| v)
    };
    if let Some(path) = &options.executable {
        return explicit(provider, path, "the executable option");
    }
    let variable = override_variable(provider);
    if let Some(value) = lookup(variable).filter(|v| !v.is_empty()) {
        return explicit(provider, Path::new(value), variable);
    }
    if let Some(path) = options.configured_executable(provider) {
        return explicit(provider, path, "the configuration file");
    }
    let name = default_name(provider);
    if let Some(found) = find_on_path(name, lookup("PATH").map(OsString::as_os_str)) {
        return Ok(found);
    }
    Err(not_found(
        provider,
        format!("\"{name}\" was not found on the absolute entries of PATH"),
    ))
}

/// The first executable file called `name` on the absolute entries of `path`,
/// a `PATH` value. Relative and empty entries are skipped, as [`resolve`]
/// skips them.
pub(super) fn find_on_path(name: &str, path: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    std::env::split_paths(path?)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

/// The first executable file called `name` on `path`, a `PATH` value, the way
/// a shell or `which` running in `cwd` finds it: an empty entry is `cwd` and a
/// relative entry is taken from `cwd`. Used for tools a provider looks up
/// itself, where relative entries count.
pub(super) fn find_tool(name: &str, path: Option<&std::ffi::OsStr>, cwd: &Path) -> Option<PathBuf> {
    std::env::split_paths(path?)
        .map(|dir| {
            if dir.as_os_str().is_empty() {
                cwd.to_path_buf()
            } else {
                cwd.join(dir)
            }
        })
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

/// The first line of `--version` output as the provider printed it. The hook
/// version gate parses this line; what is stored or shown is
/// [`version_text`].
pub(super) fn version_line(stdout: &str) -> Option<String> {
    let line = stdout.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(line.to_string())
}

/// The version line as a result or report holds it: with credentials removed,
/// then cut to [`VERSION_CHARS`]. Redacting before the cut means the cut cannot
/// leave part of a credential the redactor no longer recognises.
pub(super) fn version_text(line: &str, redactor: &Redactor) -> String {
    clip(&redactor.apply(line), VERSION_CHARS)
}

/// `MAJOR.MINOR.PATCH` at the start of trimmed version output.
pub(super) fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let mut rest = text.trim_start();
    let mut parts = [0u64; 3];
    for (index, part) in parts.iter_mut().enumerate() {
        let digits = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        if digits == 0 {
            return None;
        }
        *part = rest[..digits].parse().ok()?;
        rest = &rest[digits..];
        if index < 2 {
            rest = rest.strip_prefix('.')?;
        }
    }
    Some((parts[0], parts[1], parts[2]))
}
