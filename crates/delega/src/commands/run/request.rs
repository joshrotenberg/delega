//! From a profile file and the command line flags to one validated request.

use std::io::Read;
use std::path::{Path, PathBuf};

use delega_core::contract::limits::{
    DEFAULT_TIMEOUT_SECS, MAX_ANSWER_SCHEMA_BYTES, MAX_INSTRUCTIONS_BYTES, MAX_PROMPT_BYTES,
};
use delega_core::contract::partial::{PartialPermissions, PartialRequest, PartialSession};
use delega_core::contract::permissions::Mode;
use delega_core::contract::request::{Provider, Request};
use delega_core::contract::validate::Problem;
use serde_json::Value;

use super::{Args, Effort, Provider as ProviderArg};

/// Largest profile file read: a full prompt and instructions with room for the
/// other fields.
const MAX_PROFILE_BYTES: usize = MAX_PROMPT_BYTES + MAX_INSTRUCTIONS_BYTES + 1024 * 1024;
/// Largest schema file read. The contract bounds the schema as compact JSON; a
/// file may carry whitespace, so it gets a multiple.
const MAX_SCHEMA_FILE_BYTES: usize = MAX_ANSWER_SCHEMA_BYTES * 4;

/// What a refused result still reports about a request that never became one.
#[derive(Debug, Clone, Copy)]
pub struct Facts {
    /// The provider, when the request named one.
    pub provider: Option<Provider>,
    /// The permission mode that would have applied.
    pub mode: Mode,
    /// The time limit that would have applied, in seconds.
    pub timeout_secs: u64,
}

impl Facts {
    /// The facts of a request that was built.
    pub fn of(request: &Request) -> Self {
        Self {
            provider: Some(request.provider),
            mode: request.permissions.mode,
            timeout_secs: request.timeout_secs,
        }
    }
}

/// Why no request could be built: every problem found, and what is known.
#[derive(Debug)]
pub struct Failure {
    pub problems: Vec<Problem>,
    pub facts: Facts,
}

fn problem(field: &str, message: impl Into<String>) -> Problem {
    Problem {
        field: field.to_string(),
        message: message.into(),
    }
}

/// Build the request: the profile first, explicit flags over it, then the
/// contract's own validation. Problems in reading the inputs are reported
/// together and stop before validation, which would only repeat them as a
/// missing field.
pub fn build(args: &Args) -> Result<Request, Failure> {
    let mut problems = Vec::new();
    let profile = args
        .request
        .as_deref()
        .and_then(|path| read_profile(path, &mut problems))
        .unwrap_or_default();
    let flags = flags(args, &mut problems);
    let mut merged = profile.merge(flags);
    resolve_paths(&mut merged);
    let facts = Facts {
        provider: merged.provider,
        mode: merged
            .permissions
            .as_ref()
            .and_then(|p| p.mode)
            .unwrap_or_default(),
        timeout_secs: merged.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS),
    };
    if !problems.is_empty() {
        return Err(Failure { problems, facts });
    }
    merged
        .into_request()
        .map_err(|problems| Failure { problems, facts })
}

/// Read a regular file of at most `limit` bytes as UTF-8. A device, a pipe or a
/// directory is refused before it is opened, so reading cannot block or run
/// without end, and a file over the limit is refused naming the limit.
fn read_limited(path: &Path, limit: usize) -> Result<String, String> {
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

/// Make every relative path in the request relative to the caller's working
/// directory: `cwd` and `permissions.directories`, whether they came from the
/// flags or from the profile. Without this a relative extra directory would be
/// resolved against the worker's own working directory. A path that cannot be
/// made absolute, such as an empty one, is left for validation to report.
fn resolve_paths(request: &mut PartialRequest) {
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

fn read_profile(path: &Path, problems: &mut Vec<Problem>) -> Option<PartialRequest> {
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

/// The request fields the flags set explicitly. A flag that was not given sets
/// nothing, so it never overrides the profile.
fn flags(args: &Args, problems: &mut Vec<Problem>) -> PartialRequest {
    PartialRequest {
        provider: args.provider.map(|p| match p {
            ProviderArg::Claude => Provider::Claude,
            ProviderArg::Codex => Provider::Codex,
        }),
        prompt: prompt(args, problems),
        cwd: args.cwd.clone(),
        name: args.name.clone(),
        model: args.model.clone(),
        effort: args.effort.map(|e| match e {
            Effort::Low => delega_core::contract::request::Effort::Low,
            Effort::Medium => delega_core::contract::request::Effort::Medium,
            Effort::High => delega_core::contract::request::Effort::High,
        }),
        timeout_secs: args.timeout,
        permissions: permissions(args),
        answer_schema: args
            .schema
            .as_deref()
            .and_then(|path| read_schema(path, problems)),
        session: session(args),
        native_args: (!args.native_args.is_empty()).then(|| args.native_args.clone()),
        ..PartialRequest::default()
    }
}

fn prompt(args: &Args, problems: &mut Vec<Problem>) -> Option<String> {
    match (&args.prompt, args.stdin) {
        (Some(_), true) => {
            problems.push(problem(
                "prompt",
                "--prompt and --stdin cannot be used together; give the assignment one way",
            ));
            None
        }
        (Some(text), false) => Some(text.clone()),
        (None, true) => read_stdin(problems),
        (None, false) => None,
    }
}

/// Read the assignment from standard input, up to one byte past the contract's
/// limit so that validation reports an oversize prompt instead of this reading
/// without bound.
fn read_stdin(problems: &mut Vec<Problem>) -> Option<String> {
    let mut bytes = Vec::new();
    let limit = MAX_PROMPT_BYTES as u64 + 1;
    if let Err(err) = std::io::stdin().lock().take(limit).read_to_end(&mut bytes) {
        problems.push(problem(
            "prompt",
            format!("cannot read standard input: {err}"),
        ));
        return None;
    }
    match String::from_utf8(bytes) {
        Ok(text) => Some(text),
        Err(_) => {
            problems.push(problem("prompt", "standard input is not valid UTF-8"));
            None
        }
    }
}

fn read_schema(path: &Path, problems: &mut Vec<Problem>) -> Option<Value> {
    let text = match read_limited(path, MAX_SCHEMA_FILE_BYTES) {
        Ok(text) => text,
        Err(err) => {
            problems.push(problem(
                "answer_schema",
                format!("cannot read {}: {err}", path.display()),
            ));
            return None;
        }
    };
    match serde_json::from_str(&text) {
        Ok(schema) => Some(schema),
        Err(err) => {
            problems.push(problem(
                "answer_schema",
                format!("{} is not valid JSON: {err}", path.display()),
            ));
            None
        }
    }
}

fn permissions(args: &Args) -> Option<PartialPermissions> {
    let mode = if args.full {
        Some(Mode::Full)
    } else if args.write {
        Some(Mode::Write)
    } else if args.read {
        Some(Mode::Read)
    } else {
        None
    };
    let commands = (!args.allow_command.is_empty()).then(|| args.allow_command.clone());
    let directories = (!args.add_dir.is_empty()).then(|| args.add_dir.clone());
    if mode.is_none() && commands.is_none() && directories.is_none() {
        return None;
    }
    Some(PartialPermissions {
        mode,
        commands,
        directories,
        on_request: None,
    })
}

fn session(args: &Args) -> Option<PartialSession> {
    if let Some(id) = &args.resume {
        return Some(PartialSession {
            ephemeral: None,
            resume: Some(id.clone()),
        });
    }
    args.ephemeral.then_some(PartialSession {
        ephemeral: Some(true),
        resume: None,
    })
}
