//! From a request to everything needed to start the provider.
//!
//! [`build`] does all the work that needs neither a running provider nor a
//! runtime: validation, path resolution, the executable lookup, the permission
//! mapping, the arguments and the environment. `run` starts from it and
//! `dry_run` reports it.

use std::path::PathBuf;
use std::time::Duration;

use jsonschema::Validator;

use crate::contract::applied::{Applied, AppliedContext, AppliedDenyRules};
use crate::contract::request::{Provider, Request};

use super::deny::{self, DenyRules};
use super::env::{self, EnvInputs, ResolvedEnv};
use super::launch::{Built, Elide, Inputs};
use super::limits::{MAX_OUTPUT_BYTES, VALIDATION_MESSAGE_CHARS};
use super::managed;
use super::options::ProviderOptions;
use super::permissions::{Mapping, Scope};
use super::redact::Redactor;
use super::refusal::{Refusal, clip};
use super::{claude, codex, native_args, permissions, program, prompt};

/// A request resolved into a launch.
pub(super) struct Plan {
    pub provider: Provider,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: ResolvedEnv,
    pub scope: Scope,
    pub stdin: Vec<u8>,
    pub mapping: Mapping,
    pub notes: Vec<String>,
    /// What this plan applies, for the result and the dry run.
    pub applied: Applied,
    pub deny: DenyRules,
    pub elide: Vec<Elide>,
    pub mcp_index: Option<usize>,
    pub needs_hook_version: bool,
    pub validator: Option<Validator>,
    pub redactor: Redactor,
    pub time_limit: Duration,
    pub max_output_bytes: usize,
    pub stop_grace: Option<Duration>,
    #[cfg(test)]
    pub tap_queue_chunks: Option<usize>,
}

/// Resolve a request into a launch, or say why it is refused.
pub(super) fn build(request: &Request, options: &ProviderOptions) -> Result<Plan, Refusal> {
    build_with(request, options, None)
}

/// [`build`] for a caller that wants the launch even where a run would be
/// refused for a missing executable, unusable deny rules or exclusive managed
/// MCP. Those refusals are pushed on `problems` and the plan goes on with
/// defaults for anything unavailable. `doctor` uses it to report the environment
/// a worker would get next to the problems.
pub(super) fn build_lenient(
    request: &Request,
    options: &ProviderOptions,
    problems: &mut Vec<Refusal>,
) -> Result<Plan, Refusal> {
    build_with(request, options, Some(problems))
}

/// Keep a refusal for later when `problems` is given, and use the default value
/// in its place; otherwise return it.
fn tolerate<T: Default>(
    result: Result<T, Refusal>,
    problems: &mut Option<&mut Vec<Refusal>>,
) -> Result<T, Refusal> {
    match (result, problems.as_deref_mut()) {
        (Ok(value), _) => Ok(value),
        (Err(refusal), Some(list)) => {
            list.push(refusal);
            Ok(T::default())
        }
        (Err(refusal), None) => Err(refusal),
    }
}

fn build_with(
    request: &Request,
    options: &ProviderOptions,
    mut problems: Option<&mut Vec<Refusal>>,
) -> Result<Plan, Refusal> {
    if let Err(problems) = request.validate() {
        let message = problems
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        return Err(Refusal::new("invalid_request", message));
    }
    let native = native_args::check(request.provider, &request.native_args)?;
    let validator = match &request.answer_schema {
        Some(schema) => Some(jsonschema::validator_for(schema).map_err(|e| {
            Refusal::new(
                "invalid_answer_schema",
                format!(
                    "answer_schema is not a usable JSON Schema: {}",
                    clip(&e.to_string(), VALIDATION_MESSAGE_CHARS)
                ),
            )
        })?),
        None => None,
    };
    let scope = permissions::scope(request)?;
    let parent = options.parent_env();
    let program = tolerate(
        program::resolve(request.provider, options, &parent),
        &mut problems,
    )?;
    let assembled = prompt::assemble(request);
    let deny = match request.provider {
        Provider::Claude => {
            let deny = tolerate(claude_deny_rules(&scope, &parent, options), &mut problems)?;
            tolerate(
                managed::check_claude_mcp(&options.system_paths),
                &mut problems,
            )?;
            deny
        }
        Provider::Codex => DenyRules::default(),
    };

    let inputs = Inputs {
        request,
        scope: &scope,
        hook: options.hook.as_ref(),
        system: &assembled.system,
        native: &native,
        deny: &deny,
    };
    let built: Built = match request.provider {
        Provider::Claude => claude::build(&inputs)?,
        Provider::Codex => codex::build(&inputs)?,
    };
    if request.permissions.strict && !built.unenforceable.is_empty() {
        return Err(Refusal::new(
            "permissions_not_enforceable",
            format!(
                "permissions.strict is set and {} cannot enforce these permissions as stated: \
                 {}",
                match request.provider {
                    Provider::Claude => "Claude Code",
                    Provider::Codex => "Codex",
                },
                built.unenforceable.join("; ")
            ),
        ));
    }
    env::check_names(&options.env_pass, &options.env_set, &built.env)?;
    let env = env::resolve(&EnvInputs {
        provider: request.provider,
        parent: &parent,
        pass: &options.env_pass,
        set: &options.env_set,
        adapter: &built.env,
    });
    let redactor = Redactor::new(env.secret_values(request.provider));
    let mut notes = built.notes;
    notes.extend(wide_scope_note(&scope, &parent));
    notes.extend(assembled.notes);
    notes.extend(env.notes());
    let applied = applied(request.provider, options, &env, &deny);
    let mapping = Mapping {
        mode: scope.mode,
        commands: built.commands,
        directories: scope
            .directories
            .iter()
            .map(|d| d.to_string_lossy().into_owned())
            .collect(),
        settings: built.settings,
        on_request: "deny".to_string(),
    };
    Ok(Plan {
        provider: request.provider,
        program,
        args: built.args,
        env,
        scope,
        stdin: assembled.stdin,
        mapping,
        notes,
        applied,
        deny,
        elide: built.elide,
        mcp_index: built.mcp_index,
        needs_hook_version: built.needs_hook_version,
        validator,
        redactor,
        time_limit: Duration::from_secs(request.timeout_secs),
        max_output_bytes: options.max_output_bytes.unwrap_or(MAX_OUTPUT_BYTES),
        stop_grace: options.stop_grace,
        #[cfg(test)]
        tap_queue_chunks: options.tap_queue_chunks,
    })
}

/// The `applied` object of a prepared run: what the caller knew
/// (`options.origin`) and what preparing the run decided.
fn applied(
    provider: Provider,
    options: &ProviderOptions,
    env: &ResolvedEnv,
    deny: &DenyRules,
) -> Applied {
    let mut named: Vec<String> = options
        .env_pass
        .iter()
        .cloned()
        .chain(options.env_set.iter().map(|(name, _)| name.clone()))
        .collect();
    named.sort();
    named.dedup();
    let claude = provider == Provider::Claude;
    Applied {
        config: options.origin.config.clone(),
        sources: options.origin.sources.clone(),
        env: Some(env.applied(named)),
        deny_rules: claude.then(|| AppliedDenyRules {
            files: deny.files.clone(),
            skipped: deny.skipped.clone(),
            applied: deny.rules.len(),
        }),
        // Claude Code runs with settings sources empty, so only the deny rules
        // reach it and delega loads no instruction files. Codex has no such
        // switch to report.
        context: claude.then(|| AppliedContext {
            settings: "deny_rules".to_string(),
            instructions: false,
        }),
    }
}

/// The deny rules of the user's and the project's Claude Code settings, found
/// from the home and configuration directory the worker will receive.
fn claude_deny_rules(
    scope: &Scope,
    parent: &[(std::ffi::OsString, std::ffi::OsString)],
    options: &ProviderOptions,
) -> Result<DenyRules, Refusal> {
    // The adapter's own variables never name a home or configuration directory,
    // so resolving without them gives the same two values the worker gets.
    let worker_env = env::resolve(&EnvInputs {
        provider: Provider::Claude,
        parent,
        pass: &options.env_pass,
        set: &options.env_set,
        adapter: &[],
    });
    deny::collect(
        worker_env.value("HOME"),
        worker_env.value("CLAUDE_CONFIG_DIR"),
        &scope.cwd,
    )
}

/// A warning when the working directory is the filesystem root or the home
/// directory, where rules scoped to the working directory cover everything
/// below it.
fn wide_scope_note(
    scope: &Scope,
    parent: &[(std::ffi::OsString, std::ffi::OsString)],
) -> Option<String> {
    let home = parent
        .iter()
        .rev()
        .find(|(n, _)| n.to_str() == Some("HOME"))
        .and_then(|(_, v)| std::fs::canonicalize(v).ok());
    let what = if scope.cwd == std::path::Path::new("/") {
        "the filesystem root"
    } else if home.as_deref() == Some(scope.cwd.as_path()) {
        "the home directory"
    } else {
        return None;
    };
    Some(format!(
        "the working directory is {what}, so the working-directory scope of this mode covers \
         everything below it"
    ))
}
