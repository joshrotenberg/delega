//! From a profile file and the command line flags to one validated request.

use std::io::Read;
use std::path::Path;

use delega_core::config::Config;
use delega_core::contract::applied::{Applied, AppliedConfig, Layer};
use delega_core::contract::limits::{
    DEFAULT_TIMEOUT_SECS, MAX_ANSWER_SCHEMA_BYTES, MAX_PROMPT_BYTES, MAX_TIMEOUT_SECS,
    MIN_TIMEOUT_SECS,
};
use delega_core::contract::partial::{PartialPermissions, PartialRequest, PartialSession};
use delega_core::contract::permissions::Mode;
use delega_core::contract::request::{Provider, Request};
use delega_core::contract::validate::Problem;
use delega_core::profile::{self, ErrorDetail, problem, read_limited};
use serde_json::Value;

use super::{Args, Effort, Provider as ProviderArg};

/// Largest schema file read. The contract bounds the schema as compact JSON; a
/// file may carry whitespace, so it gets a multiple.
const MAX_SCHEMA_FILE_BYTES: usize = MAX_ANSWER_SCHEMA_BYTES * 4;

/// What a refused result still reports about a request that never became one.
#[derive(Debug, Clone)]
pub struct Facts {
    /// The provider, when the request named one.
    pub provider: Option<Provider>,
    /// The permission mode that would have applied.
    pub mode: Mode,
    /// The time limit that would have applied, in seconds.
    pub timeout_secs: u64,
    /// What is known of `applied`: the configuration file and the layer of
    /// each field, once the request was merged; nothing before.
    pub origin: Box<Applied>,
}

impl Facts {
    /// The facts of a request that was built.
    pub fn of(request: &Request, origin: Applied) -> Self {
        Self {
            provider: Some(request.provider),
            mode: request.permissions.mode,
            timeout_secs: request.timeout_secs,
            origin: Box::new(origin),
        }
    }
}

/// A request and what is known of how it was built.
#[derive(Debug)]
pub struct Built {
    pub request: Request,
    /// The configuration file and the layer of each field, as `applied`.
    pub origin: Applied,
}

/// Why no request could be built: every problem found, and what is known.
#[derive(Debug)]
pub struct Failure {
    pub problems: Vec<Problem>,
    pub facts: Facts,
}

/// What a refusal before any request exists still reports: the provider and the
/// mode the flags name, and the time limit they give or the default. The
/// configuration file is not consulted, because the refusal may be about it.
pub fn flag_facts(args: &Args) -> Facts {
    Facts {
        provider: provider(args),
        mode: mode(args).unwrap_or_default(),
        // A raw `--timeout` outside the contract's range is not carried into a
        // result: it would state a limit that no run could have had.
        timeout_secs: args
            .timeout
            .filter(|t| (MIN_TIMEOUT_SECS..=MAX_TIMEOUT_SECS).contains(t))
            .unwrap_or(DEFAULT_TIMEOUT_SECS),
        origin: Box::default(),
    }
}

/// Build the request: the configuration file's defaults first, then the
/// profile, then explicit flags over both, then the contract's own validation. Problems in reading the inputs are reported
/// together and stop before validation, which would only repeat them as a
/// missing field.
pub fn build(args: &Args, config: &Config, file: Option<AppliedConfig>) -> Result<Built, Failure> {
    let mut problems = Vec::new();
    let profile::Merged {
        request: merged,
        sources,
    } = profile::merge_tracked(
        config.request_defaults(),
        args.request.as_deref(),
        ErrorDetail::Full,
        Layer::Flag,
        &mut problems,
        |problems| flags(args, problems),
    );
    let origin = Applied {
        config: file,
        sources,
        ..Applied::default()
    };
    let facts = Facts {
        provider: merged.provider,
        mode: merged
            .permissions
            .as_ref()
            .and_then(|p| p.mode)
            .unwrap_or_default(),
        timeout_secs: merged.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS),
        origin: Box::new(origin.clone()),
    };
    if !problems.is_empty() {
        return Err(Failure { problems, facts });
    }
    match merged.into_request() {
        Ok(request) => Ok(Built { request, origin }),
        Err(problems) => Err(Failure { problems, facts }),
    }
}

/// The request fields the flags set explicitly. A flag that was not given sets
/// nothing, so it never overrides the profile.
fn flags(args: &Args, problems: &mut Vec<Problem>) -> PartialRequest {
    PartialRequest {
        provider: provider(args),
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

fn provider(args: &Args) -> Option<Provider> {
    args.provider.map(|p| match p {
        ProviderArg::Claude => Provider::Claude,
        ProviderArg::Codex => Provider::Codex,
    })
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

/// The permission mode the flags name, if any.
fn mode(args: &Args) -> Option<Mode> {
    if args.full {
        Some(Mode::Full)
    } else if args.build {
        Some(Mode::Build)
    } else if args.write {
        Some(Mode::Write)
    } else if args.read {
        Some(Mode::Read)
    } else {
        None
    }
}

fn permissions(args: &Args) -> Option<PartialPermissions> {
    let mode = mode(args);
    let commands = (!args.allow_command.is_empty()).then(|| args.allow_command.clone());
    let directories = (!args.add_dir.is_empty()).then(|| args.add_dir.clone());
    let strict = args.strict.then_some(true);
    if mode.is_none() && commands.is_none() && directories.is_none() && strict.is_none() {
        return None;
    }
    Some(PartialPermissions {
        mode,
        commands,
        directories,
        on_request: None,
        ask_timeout: None,
        strict,
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
