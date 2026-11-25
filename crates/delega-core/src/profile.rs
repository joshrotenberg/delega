//! Reading a profile file and merging it with explicit values.
//!
//! `delega run` builds a request from a profile file and command line flags;
//! the MCP `start` tool builds one from a profile file and tool arguments. Both
//! go through [`merge`], so a profile and its overrides combine the same way on
//! every surface: the profile is read first, every explicit value replaces the
//! matching field, and every relative path then resolves against the working
//! directory of the process (the caller's for the command line, the server's for
//! MCP).

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::contract::applied::Layer;
use crate::contract::limits::{MAX_INSTRUCTIONS_BYTES, MAX_PROMPT_BYTES};
use crate::contract::partial::{DEFAULTED_KEYS, PartialRequest};
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
/// directory at the path is refused before it is opened. The path is then
/// opened without blocking and what was opened is checked again before it is
/// read, so a pipe put in its place does not wait for a writer. A file over the
/// limit is refused naming the limit.
pub fn read_limited(path: &Path, limit: usize) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|err| err.to_string())?;
    if !meta.is_file() {
        return Err(NOT_A_REGULAR_FILE.to_string());
    }
    read_opened(path, limit)
}

const NOT_A_REGULAR_FILE: &str = "it is not a regular file";

/// Open `path` without blocking and read it only if what was opened is a
/// regular file. The check before opening keeps a device at the path from being
/// opened in the ordinary case; this one covers a path replaced between that
/// check and the open, such as by a pipe, whose blocking open would wait for a
/// writer.
fn read_opened(path: &Path, limit: usize) -> Result<String, String> {
    use std::os::unix::fs::OpenOptionsExt;
    let flags = rustix::fs::OFlags::NONBLOCK | rustix::fs::OFlags::NOCTTY;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags.bits() as i32)
        .open(path)
        .map_err(|err| err.to_string())?;
    let meta = file.metadata().map_err(|err| err.to_string())?;
    if !meta.is_file() {
        return Err(NOT_A_REGULAR_FILE.to_string());
    }
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

/// How much of a profile's parse error a problem repeats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorDetail {
    /// The parser's own message, which can quote values and field names from the
    /// file. Right for a person reading their own file.
    Full,
    /// The path, the kind of error and its line and column, and nothing the file
    /// said. For a caller that does not own the file, such as an MCP client that
    /// names a path.
    Position,
}

fn parse_problem(path: &Path, err: &serde_json::Error, detail: ErrorDetail) -> String {
    match detail {
        ErrorDetail::Full => format!("{} is not a valid profile: {err}", path.display()),
        ErrorDetail::Position => {
            let kind = match err.classify() {
                serde_json::error::Category::Io => "could not be read",
                serde_json::error::Category::Syntax => "is not valid JSON",
                serde_json::error::Category::Eof => "ends before the JSON document does",
                serde_json::error::Category::Data => {
                    "has a field or value that a profile does not accept"
                }
            };
            // serde gives no position for some errors, such as a missing field,
            // and reports line 0 then.
            let position = if err.line() == 0 {
                String::new()
            } else {
                format!(" (line {}, column {})", err.line(), err.column())
            };
            format!(
                "{} is not a valid profile: it {kind}{position}",
                path.display()
            )
        }
    }
}

/// Read the profile file at `path`. A problem in reading it is added to
/// `problems` and gives `None`.
pub fn read_profile(
    path: &Path,
    problems: &mut Vec<Problem>,
    detail: ErrorDetail,
) -> Option<PartialRequest> {
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
            problems.push(problem("request", parse_problem(path, &err, detail)));
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
    detail: ErrorDetail,
    problems: &mut Vec<Problem>,
    explicit: impl FnOnce(&mut Vec<Problem>) -> PartialRequest,
) -> PartialRequest {
    merge_over(PartialRequest::default(), path, detail, problems, explicit)
}

/// For each request field that ended up set, the layer that set it, by the
/// dotted names of [`PartialRequest::set_keys`]. A field in
/// [`DEFAULTED_KEYS`] that no layer set is `default`.
pub type Sources = BTreeMap<String, Layer>;

/// A merged request and where each of its fields came from.
#[derive(Debug, Clone)]
pub struct Merged {
    /// The request, as [`merge_over`] returns it.
    pub request: PartialRequest,
    /// The layer of each field that is set.
    pub sources: Sources,
}

/// [`merge`] with a lowest layer under the profile: the defaults of the
/// configuration file ([`crate::config::Config::request_defaults`]). The order
/// is `base`, then the profile, then `explicit`, each replacing the fields the
/// one before set.
pub fn merge_over(
    base: PartialRequest,
    path: Option<&Path>,
    detail: ErrorDetail,
    problems: &mut Vec<Problem>,
    explicit: impl FnOnce(&mut Vec<Problem>) -> PartialRequest,
) -> PartialRequest {
    merge_tracked(base, path, detail, Layer::Flag, problems, explicit).request
}

/// [`merge_over`] that also records which layer set each field. `base` is the
/// `config` layer, the profile is `profile`, and `explicit` is `explicit_layer`:
/// `flag` for the command line, `argument` for an MCP call. The layers are
/// recorded here, as they are merged, not worked out from the values afterwards.
pub fn merge_tracked(
    base: PartialRequest,
    path: Option<&Path>,
    detail: ErrorDetail,
    explicit_layer: Layer,
    problems: &mut Vec<Problem>,
    explicit: impl FnOnce(&mut Vec<Problem>) -> PartialRequest,
) -> Merged {
    let mut merged =
        merge_unresolved_tracked(base, path, detail, explicit_layer, problems, explicit);
    resolve_paths(&mut merged.request);
    merged
}

/// [`merge`] without [`resolve_paths`]: the merged request keeps `cwd` and
/// `permissions.directories` as the profile or the caller wrote them. For a
/// caller that must see whether `cwd` was absolute before this process's
/// working directory is applied to it.
pub fn merge_unresolved(
    path: Option<&Path>,
    detail: ErrorDetail,
    problems: &mut Vec<Problem>,
    explicit: impl FnOnce(&mut Vec<Problem>) -> PartialRequest,
) -> PartialRequest {
    merge_unresolved_over(PartialRequest::default(), path, detail, problems, explicit)
}

/// [`merge_over`] without [`resolve_paths`].
pub fn merge_unresolved_over(
    base: PartialRequest,
    path: Option<&Path>,
    detail: ErrorDetail,
    problems: &mut Vec<Problem>,
    explicit: impl FnOnce(&mut Vec<Problem>) -> PartialRequest,
) -> PartialRequest {
    merge_unresolved_tracked(base, path, detail, Layer::Flag, problems, explicit).request
}

/// [`merge_tracked`] without [`resolve_paths`].
pub fn merge_unresolved_tracked(
    base: PartialRequest,
    path: Option<&Path>,
    detail: ErrorDetail,
    explicit_layer: Layer,
    problems: &mut Vec<Problem>,
    explicit: impl FnOnce(&mut Vec<Problem>) -> PartialRequest,
) -> Merged {
    let profile = path
        .and_then(|path| read_profile(path, problems, detail))
        .unwrap_or_default();
    let explicit = explicit(problems);
    let mut sources = Sources::new();
    for (layer, request) in [
        (Layer::Config, &base),
        (Layer::Profile, &profile),
        (explicit_layer, &explicit),
    ] {
        for key in request.set_keys() {
            sources.insert(key.to_string(), layer);
        }
    }
    let request = base.merge(profile).merge(explicit);
    // A field the merge cleared (an explicit `resume` drops an `ephemeral` from
    // a lower layer, and the other way round) has no source.
    let set = request.set_keys();
    sources.retain(|key, _| set.contains(&key.as_str()));
    for key in DEFAULTED_KEYS {
        sources.entry(key.to_string()).or_insert(Layer::Default);
    }
    Merged { request, sources }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::partial::{PartialPermissions, PartialSession};
    use crate::contract::permissions::Mode;
    use crate::contract::request::{Effort, Provider};
    use crate::test_spawn::Guarded;

    fn config_layer() -> PartialRequest {
        PartialRequest {
            provider: Some(Provider::Claude),
            model: Some("config-model".to_string()),
            effort: Some(Effort::Low),
            permissions: Some(PartialPermissions {
                mode: Some(Mode::Read),
                strict: Some(true),
                ..PartialPermissions::default()
            }),
            ..PartialRequest::default()
        }
    }

    fn profile_file(dir: &tempfile::TempDir, body: &str) -> PathBuf {
        let path = dir.path().join("profile.json");
        std::fs::write(&path, body).expect("write profile");
        path
    }

    fn merged(
        base: PartialRequest,
        path: Option<&Path>,
        layer: Layer,
        explicit: PartialRequest,
    ) -> Merged {
        let mut problems = Vec::new();
        let merged = merge_tracked(base, path, ErrorDetail::Full, layer, &mut problems, |_| {
            explicit
        });
        assert!(problems.is_empty(), "{problems:?}");
        merged
    }

    fn source<'a>(merged: &'a Merged, key: &str) -> Option<&'a Layer> {
        merged.sources.get(key)
    }

    #[test]
    fn a_profile_accepts_build_mode() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = profile_file(
            &dir,
            r#"{"schema_version": 1, "permissions": {"mode": "build"}}"#,
        );
        let mut problems = Vec::new();
        let profile = read_profile(&path, &mut problems, ErrorDetail::Full);
        let profile = profile.unwrap_or_else(|| panic!("{problems:?}"));
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(profile.permissions.and_then(|p| p.mode), Some(Mode::Build));
    }

    #[test]
    fn a_profile_error_without_a_position_does_not_report_line_zero() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = profile_file(&dir, r#"{"model": "m"}"#);
        let mut problems = Vec::new();
        assert!(read_profile(&path, &mut problems, ErrorDetail::Position).is_none());
        let message = &problems[0].message;
        assert!(!message.contains("line 0"), "{message}");
        assert!(!message.contains("column"), "{message}");
        assert!(message.contains("a profile does not accept"), "{message}");

        let path = profile_file(&dir, "{ not json");
        let mut problems = Vec::new();
        assert!(read_profile(&path, &mut problems, ErrorDetail::Position).is_none());
        let message = &problems[0].message;
        assert!(message.contains("(line 1, column"), "{message}");
    }

    #[test]
    fn every_layer_is_named_for_the_fields_it_set() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = profile_file(
            &dir,
            r#"{"schema_version": 1, "model": "profile-model",
                "permissions": {"mode": "write", "commands": ["cargo test"]}}"#,
        );
        let flags = PartialRequest {
            prompt: Some("go".to_string()),
            effort: Some(Effort::High),
            permissions: Some(PartialPermissions {
                directories: Some(vec![PathBuf::from("/x")]),
                ..PartialPermissions::default()
            }),
            ..PartialRequest::default()
        };
        let m = merged(config_layer(), Some(&path), Layer::Flag, flags);
        // Config only.
        assert_eq!(source(&m, "provider"), Some(&Layer::Config));
        assert_eq!(source(&m, "permissions.strict"), Some(&Layer::Config));
        // Config, replaced by the profile.
        assert_eq!(source(&m, "model"), Some(&Layer::Profile));
        assert_eq!(source(&m, "permissions.mode"), Some(&Layer::Profile));
        assert_eq!(source(&m, "permissions.commands"), Some(&Layer::Profile));
        // Config, replaced by a flag.
        assert_eq!(source(&m, "effort"), Some(&Layer::Flag));
        // Flags only.
        assert_eq!(source(&m, "prompt"), Some(&Layer::Flag));
        assert_eq!(source(&m, "permissions.directories"), Some(&Layer::Flag));
        // Fields nothing set have a built-in default or are not listed.
        assert_eq!(source(&m, "timeout_secs"), Some(&Layer::Default));
        assert_eq!(source(&m, "permissions.on_request"), Some(&Layer::Default));
        assert_eq!(source(&m, "session.ephemeral"), Some(&Layer::Default));
        assert_eq!(source(&m, "name"), None);
        assert_eq!(source(&m, "session.resume"), None);
        assert_eq!(source(&m, "cwd"), None);
        assert_eq!(source(&m, "schema_version"), None);
        // The values are what the layers' order says.
        assert_eq!(m.request.model.as_deref(), Some("profile-model"));
        assert_eq!(m.request.effort, Some(Effort::High));
    }

    #[test]
    fn the_explicit_layer_is_argument_for_a_tool_call() {
        let args = PartialRequest {
            provider: Some(Provider::Codex),
            prompt: Some("go".to_string()),
            timeout_secs: Some(90),
            ..PartialRequest::default()
        };
        let m = merged(config_layer(), None, Layer::Argument, args);
        assert_eq!(source(&m, "provider"), Some(&Layer::Argument));
        assert_eq!(source(&m, "timeout_secs"), Some(&Layer::Argument));
        assert_eq!(source(&m, "model"), Some(&Layer::Config));
        assert_eq!(source(&m, "permissions.mode"), Some(&Layer::Config));
    }

    #[test]
    fn nothing_set_is_all_defaults() {
        let m = merged(
            PartialRequest::default(),
            None,
            Layer::Flag,
            PartialRequest::default(),
        );
        assert_eq!(m.sources.len(), DEFAULTED_KEYS.len());
        assert!(m.sources.values().all(|l| *l == Layer::Default));
    }

    #[test]
    fn a_field_the_merge_clears_has_no_source() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = profile_file(
            &dir,
            r#"{"schema_version": 1, "session": {"ephemeral": true}}"#,
        );
        let flags = PartialRequest {
            session: Some(PartialSession {
                ephemeral: None,
                resume: Some("sess-1".to_string()),
            }),
            ..PartialRequest::default()
        };
        let m = merged(PartialRequest::default(), Some(&path), Layer::Flag, flags);
        assert_eq!(source(&m, "session.resume"), Some(&Layer::Flag));
        // The profile's `ephemeral` was dropped for the explicit `resume`, so the
        // field is the default again, not the profile's.
        assert_eq!(m.request.session.as_ref().and_then(|s| s.ephemeral), None);
        assert_eq!(source(&m, "session.ephemeral"), Some(&Layer::Default));
    }

    #[test]
    fn the_untracked_merges_give_the_same_request() {
        let flags = || PartialRequest {
            prompt: Some("go".to_string()),
            ..PartialRequest::default()
        };
        let mut problems = Vec::new();
        let plain = merge_unresolved_over(
            config_layer(),
            None,
            ErrorDetail::Full,
            &mut problems,
            |_| flags(),
        );
        let tracked = merged(config_layer(), None, Layer::Flag, flags());
        assert_eq!(plain, tracked.request);
    }

    /// The open itself must not block and the opened file must be checked:
    /// without that, a path replaced by a pipe after the check before opening
    /// waits for a writer.
    #[test]
    fn a_pipe_in_place_of_the_checked_file_is_refused_without_blocking() {
        let dir = tempfile::tempdir().expect("temp dir");
        let fifo = dir.path().join("profile.json");
        let made = std::process::Command::new("/usr/bin/mkfifo")
            .arg(&fifo)
            .guarded_status()
            .expect("run mkfifo");
        assert!(made.success(), "mkfifo failed: {made}");
        let (tx, rx) = std::sync::mpsc::channel();
        let path = fifo.clone();
        std::thread::spawn(move || {
            let _ = tx.send(read_opened(&path, MAX_PROFILE_BYTES));
        });
        let answer = rx.recv_timeout(std::time::Duration::from_secs(30));
        if matches!(answer, Err(std::sync::mpsc::RecvTimeoutError::Timeout)) {
            // Release an open blocked on the pipe so the thread ends. Opened
            // without blocking, so this cannot hang if nothing is reading.
            use std::os::unix::fs::OpenOptionsExt;
            let _ = std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
                .open(&fifo);
        }
        let refusal = answer
            .expect("read_opened must answer within 30 s with no writer on the pipe")
            .expect_err("refused");
        assert_eq!(refusal, NOT_A_REGULAR_FILE);
    }

    #[test]
    fn a_directory_in_place_of_the_checked_file_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let refusal = read_opened(dir.path(), MAX_PROFILE_BYTES).expect_err("refused");
        assert_eq!(refusal, NOT_A_REGULAR_FILE);
    }

    #[test]
    fn a_regular_file_is_read_through_the_non_blocking_open() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = profile_file(&dir, "{\"schema_version\": 1}");
        assert_eq!(
            read_limited(&path, MAX_PROFILE_BYTES).expect("read"),
            "{\"schema_version\": 1}"
        );
    }
}
