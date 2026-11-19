//! Reading a profile file and merging it with explicit values.
//!
//! `delega run` builds a request from a profile file and command line flags;
//! the MCP `start` tool builds one from a profile file and tool arguments. Both
//! go through [`merge`], so a profile and its overrides combine the same way on
//! every surface: the profile is read first, every explicit value replaces the
//! matching field, and every relative path then resolves against the working
//! directory of the process (the caller's for the command line, the server's for
//! MCP).

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::contract::limits::{MAX_INSTRUCTIONS_BYTES, MAX_PROMPT_BYTES};
use crate::contract::partial::PartialRequest;
use crate::contract::validate::Problem;

/// Largest profile file read: a full prompt and instructions with room for the
/// other fields.
pub const MAX_PROFILE_BYTES: usize = MAX_PROMPT_BYTES + MAX_INSTRUCTIONS_BYTES + 1024 * 1024;

/// A problem with the given field and message.
pub fn problem(field: &str, message: impl Into<String>) -> Problem {
    Problem {
        field: field.to_string(),
        message: message.into(),
    }
}

/// Read a regular file of at most `limit` bytes as UTF-8. A device, a pipe or a
/// directory is refused before it is opened, so reading cannot block or run
/// without end, and a file over the limit is refused naming the limit.
pub fn read_limited(path: &Path, limit: usize) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|err| err.to_string())?;
    if !meta.is_file() {
        return Err("it is not a regular file".to_string());
    }
    let file = std::fs::File::open(path).map_err(|err| err.to_string())?;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| err.to_string())?;
    if bytes.len() > limit {
        return Err(format!("it is larger than the limit of {limit} bytes"));
    }
    String::from_utf8(bytes).map_err(|_| "it is not valid UTF-8".to_string())
}

/// Make every relative path in the request relative to the working directory
/// of this process: `cwd` and `permissions.directories`, whether they came from
/// a profile or from explicit values. Without this a relative extra directory
/// would be resolved against the worker's own working directory. A path that
/// cannot be made absolute, such as an empty one, is left for validation to
/// report.
pub fn resolve_paths(request: &mut PartialRequest) {
    let absolute = |path: &PathBuf| std::path::absolute(path).unwrap_or_else(|_| path.clone());
    if let Some(cwd) = &mut request.cwd {
        *cwd = absolute(cwd);
    }
    if let Some(dirs) = request
        .permissions
        .as_mut()
        .and_then(|p| p.directories.as_mut())
    {
        for dir in dirs {
            *dir = absolute(dir);
        }
    }
}

/// Read the profile file at `path`. A problem in reading it is added to
/// `problems` and gives `None`.
pub fn read_profile(path: &Path, problems: &mut Vec<Problem>) -> Option<PartialRequest> {
    let text = match read_limited(path, MAX_PROFILE_BYTES) {
        Ok(text) => text,
        Err(err) => {
            problems.push(problem(
                "request",
                format!("cannot read {}: {err}", path.display()),
            ));
            return None;
        }
    };
    match PartialRequest::from_profile_json(&text) {
        Ok(profile) => Some(profile),
        Err(err) => {
            problems.push(problem(
                "request",
                format!("{} is not a valid profile: {err}", path.display()),
            ));
            None
        }
    }
}

/// The profile at `path`, if one is named, merged with the explicit values
/// `explicit` returns, with relative paths made absolute.
///
/// The profile is read first, then `explicit` is called, so a problem reading
/// the profile comes before the problems `explicit` finds. Problems are added
/// to `problems`; when there are any, the merged request is still returned
/// but is not complete.
pub fn merge(
    path: Option<&Path>,
    problems: &mut Vec<Problem>,
    explicit: impl FnOnce(&mut Vec<Problem>) -> PartialRequest,
) -> PartialRequest {
    let profile = path
        .and_then(|path| read_profile(path, problems))
        .unwrap_or_default();
    let explicit = explicit(problems);
    let mut merged = profile.merge(explicit);
    resolve_paths(&mut merged);
    merged
}
