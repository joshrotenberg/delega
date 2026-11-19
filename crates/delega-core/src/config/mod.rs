//! The user-level configuration file.
//!
//! One optional TOML file holds what a person wants every `delega run`, `delega
//! mcp` and `delega doctor` to start from: request defaults, environment names
//! and values for workers, provider executables and the MCP server's limits.
//! `docs/api.md` ("Configuration file") is the specification; the committed
//! schema is `schemas/config.schema.json`, generated from the types in this
//! module.
//!
//! # Where it is read
//!
//! `DELEGA_CONFIG` (an absolute path) when set and not empty; otherwise
//! `$XDG_CONFIG_HOME/delega/config.toml` when `XDG_CONFIG_HOME` is set and
//! absolute; otherwise `$HOME/.config/delega/config.toml`. No file in a
//! repository or in the working directory is ever read, so a cloned project
//! cannot change what a worker may do. A missing file at either default place
//! means no configuration, which is exactly the behaviour without this module.
//! A missing file that `DELEGA_CONFIG` names is refused.
//!
//! # When it is refused
//!
//! The file is used only if it is a regular file owned by the current user and
//! not writable by group or others, judged on the open handle ([`read`]). A
//! problem of any kind refuses with a kind that names it, never skips the file:
//!
//! - `config_unreadable`: it cannot be used as a file (cannot be opened, is not
//!   a regular file, is owned by another user, is writable by group or others,
//!   is a symbolic link that fails the rule in [`file`], or is over
//!   [`MAX_CONFIG_BYTES`]);
//! - `config_invalid`: it is not UTF-8 or TOML, has an unknown key, a value of
//!   the wrong type or outside its bound, or a path that is not absolute;
//! - `invalid_env_name` or `forbidden_env_name`: a name in `[env]` that `--env`
//!   would refuse for the same reason.
//!
//! A message names the file and the key or the line, and never repeats a value
//! the file holds. Only key names, bounds and positions are quoted.
//!
//! # What it does
//!
//! [`Config`] is the parsed file. It does nothing by itself: [`Config::request_defaults`],
//! [`Config::apply_to_options`] and [`Config::limits`] give the layers below
//! what they need, and the command line layers pass them in. The precedence is
//! built-in defaults, then this file, then a profile, then explicit flags or
//! tool arguments.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use schemars::JsonSchema;
use serde::Deserialize;

use crate::contract::partial::{PartialPermissions, PartialRequest};
use crate::contract::permissions::Mode;
use crate::contract::request::{Effort, Provider};
use crate::provider::ProviderOptions;
use crate::supervisor::Limits;
use crate::supervisor::limits as supervisor;

mod check;
pub mod file;
mod schema;

#[cfg(test)]
mod tests;

pub use schema::generate_schema;

/// The version of the file format this build reads.
pub const SCHEMA_VERSION: u32 = 1;

/// Largest configuration file read, in bytes: the limit for a profile's extras.
pub const MAX_CONFIG_BYTES: usize = 1024 * 1024;

/// Refusal kind: the file cannot be used as a file.
pub const UNREADABLE: &str = "config_unreadable";
/// Refusal kind: the file's content is not acceptable.
pub const INVALID: &str = "config_invalid";

/// The file name under `delega/` in the configuration directory.
pub const FILE_NAME: &str = "config.toml";

/// Upper bound of `mcp.max_live_workers`: the supervisor's default, which is
/// the limit this build has always had.
pub const MAX_LIVE_WORKERS_BOUND: usize = supervisor::MAX_LIVE_WORKERS;
/// Upper bound of `mcp.max_running_runs`.
pub const MAX_RUNNING_RUNS_BOUND: usize = supervisor::MAX_RUNNING_RUNS;
/// Upper bound of `mcp.max_queued_per_worker`.
pub const MAX_QUEUED_PER_WORKER_BOUND: usize = supervisor::MAX_QUEUED_PER_WORKER;
/// Upper bound of `mcp.wait_ceiling_secs` and of `mcp.wait_default_secs`.
pub const WAIT_BOUND_SECS: u64 = supervisor::WAIT_CEILING.as_secs();

/// Where a configuration path came from, for reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The `DELEGA_CONFIG` variable.
    DelegaConfig,
    /// `$XDG_CONFIG_HOME/delega/config.toml`.
    XdgConfigHome,
    /// `$HOME/.config/delega/config.toml`.
    Home,
}

impl Source {
    /// The name of the variable the path came from.
    pub fn as_str(self) -> &'static str {
        match self {
            Source::DelegaConfig => "DELEGA_CONFIG",
            Source::XdgConfigHome => "XDG_CONFIG_HOME",
            Source::Home => "HOME",
        }
    }
}

/// The place a configuration file is looked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    /// The absolute path.
    pub path: PathBuf,
    /// What chose it.
    pub source: Source,
}

/// The request defaults of `[defaults]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(transform = schema::refine_defaults)]
pub struct Defaults {
    /// The provider a request uses when it names none.
    #[serde(default)]
    pub provider: Option<Provider>,
    /// Provider model name or alias, as the request's `model`.
    #[serde(default)]
    pub model: Option<String>,
    /// Reasoning effort, as the request's `effort`.
    #[serde(default)]
    pub effort: Option<Effort>,
    /// Limit for a run in whole seconds, as the request's `timeout_secs`.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// What a worker may do, by default. `prompt` and `cwd` have no default:
    /// they belong to one request.
    #[serde(default)]
    pub permissions: Option<DefaultPermissions>,
}

/// `[defaults.permissions]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DefaultPermissions {
    /// The request's `permissions.mode`.
    #[serde(default)]
    pub mode: Option<Mode>,
    /// The request's `permissions.strict`.
    #[serde(default)]
    pub strict: Option<bool>,
}

/// `[env]`: variables every worker is given, with the rules and the refusals of
/// `--env`.
#[derive(Clone, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnvSection {
    /// Names of variables whose value is taken from the environment of the
    /// process that reads this file, as `--env NAME`.
    #[serde(default)]
    pub pass: Vec<String>,
    /// Variables set to a value, as `--env NAME=VALUE`. A value is never
    /// printed, in a result, a report or an error.
    #[serde(default)]
    pub set: BTreeMap<String, String>,
}

impl fmt::Debug for EnvSection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let set_names: Vec<&String> = self.set.keys().collect();
        f.debug_struct("EnvSection")
            .field("pass", &self.pass)
            .field("set_names", &set_names)
            .finish()
    }
}

/// One provider's entry in `[providers]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProviderEntry {
    /// Absolute path of the provider's executable. `DELEGA_CLAUDE_BIN` or
    /// `DELEGA_CODEX_BIN` overrides it.
    #[serde(default)]
    pub executable: Option<String>,
}

/// `[providers]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Providers {
    /// Claude Code.
    #[serde(default)]
    pub claude: Option<ProviderEntry>,
    /// Codex CLI.
    #[serde(default)]
    pub codex: Option<ProviderEntry>,
}

/// `[mcp]`: the limits of the server `delega mcp` runs. Each is bounded by the
/// constant that is the default today, so the file can lower a limit and cannot
/// raise one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(transform = schema::refine_mcp)]
pub struct Mcp {
    /// Most workers live at once, from 1 to 16.
    #[serde(default)]
    pub max_live_workers: Option<u64>,
    /// Most runs running at once, from 1 to 4.
    #[serde(default)]
    pub max_running_runs: Option<u64>,
    /// Most runs waiting behind one worker's running run, from 0 to 16.
    #[serde(default)]
    pub max_queued_per_worker: Option<u64>,
    /// The timeout `wait` uses when it gets none, in seconds, from 0 to 300 and
    /// at most the wait ceiling.
    #[serde(default)]
    pub wait_default_secs: Option<u64>,
    /// The longest one `wait` blocks, in seconds, from 1 to 300.
    #[serde(default)]
    pub wait_ceiling_secs: Option<u64>,
}

/// The user-level delega configuration file, a TOML document with the keys below.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(title = "delega configuration file", transform = schema::refine_config)]
pub struct Config {
    /// The version of this format: 1. Required.
    pub schema_version: u32,
    /// Request defaults.
    #[serde(default)]
    pub defaults: Defaults,
    /// Environment for workers.
    #[serde(default)]
    pub env: EnvSection,
    /// Provider executables.
    #[serde(default)]
    pub providers: Providers,
    /// Limits of the MCP server.
    #[serde(default)]
    pub mcp: Mcp,
}

impl Config {
    /// The configuration of no file: it changes nothing.
    pub fn empty() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            defaults: Defaults::default(),
            env: EnvSection::default(),
            providers: Providers::default(),
            mcp: Mcp::default(),
        }
    }

    /// The request fields `[defaults]` sets, as the lowest layer under a profile
    /// and explicit values.
    pub fn request_defaults(&self) -> PartialRequest {
        let d = &self.defaults;
        let permissions = d.permissions.as_ref().and_then(|p| {
            (p.mode.is_some() || p.strict.is_some()).then(|| PartialPermissions {
                mode: p.mode,
                strict: p.strict,
                ..PartialPermissions::default()
            })
        });
        PartialRequest {
            provider: d.provider,
            model: d.model.clone(),
            effort: d.effort,
            timeout_secs: d.timeout_secs,
            permissions,
            ..PartialRequest::default()
        }
    }

    /// Add the environment names and values of `[env]` and the executables of
    /// `[providers]` to `options`.
    ///
    /// The file's names go before whatever `options` already holds, so a name
    /// or a value given on the command line, which is applied after the file,
    /// wins where both set the same variable. Both lists apply otherwise.
    pub fn apply_to_options(&self, options: &mut ProviderOptions) {
        options.env_pass.splice(0..0, self.env.pass.iter().cloned());
        options.env_set.splice(
            0..0,
            self.env.set.iter().map(|(n, v)| (n.clone(), v.clone())),
        );
        let path = |entry: &Option<ProviderEntry>| {
            entry
                .as_ref()
                .and_then(|e| e.executable.as_deref())
                .map(PathBuf::from)
        };
        if let Some(path) = path(&self.providers.claude) {
            options.claude_executable = Some(path);
        }
        if let Some(path) = path(&self.providers.codex) {
            options.codex_executable = Some(path);
        }
    }

    /// The supervisor limits: the defaults, with the ones `[mcp]` sets.
    pub fn limits(&self) -> Limits {
        let m = &self.mcp;
        let mut limits = Limits::default();
        // The bounds were checked when the file was read, so a value fits.
        let count = |value: u64| usize::try_from(value).unwrap_or(usize::MAX);
        if let Some(v) = m.max_live_workers {
            limits.max_live_workers = count(v);
        }
        if let Some(v) = m.max_running_runs {
            limits.max_running_runs = count(v);
        }
        if let Some(v) = m.max_queued_per_worker {
            limits.max_queued_per_worker = count(v);
        }
        if let Some(v) = m.wait_ceiling_secs {
            limits.wait_ceiling = Duration::from_secs(v);
        }
        if let Some(v) = m.wait_default_secs {
            limits.wait_default = Duration::from_secs(v);
        }
        limits
    }

    /// The dotted names of the keys the file sets, sorted. No value.
    pub fn settings(&self) -> Vec<String> {
        let mut keys = Vec::new();
        let mut add = |set: bool, name: &str| {
            if set {
                keys.push(name.to_string());
            }
        };
        let d = &self.defaults;
        add(d.provider.is_some(), "defaults.provider");
        add(d.model.is_some(), "defaults.model");
        add(d.effort.is_some(), "defaults.effort");
        add(d.timeout_secs.is_some(), "defaults.timeout_secs");
        let p = d.permissions.as_ref();
        add(
            p.is_some_and(|p| p.mode.is_some()),
            "defaults.permissions.mode",
        );
        add(
            p.is_some_and(|p| p.strict.is_some()),
            "defaults.permissions.strict",
        );
        add(!self.env.pass.is_empty(), "env.pass");
        add(!self.env.set.is_empty(), "env.set");
        let exe =
            |entry: &Option<ProviderEntry>| entry.as_ref().is_some_and(|e| e.executable.is_some());
        add(exe(&self.providers.claude), "providers.claude.executable");
        add(exe(&self.providers.codex), "providers.codex.executable");
        let m = &self.mcp;
        add(m.max_live_workers.is_some(), "mcp.max_live_workers");
        add(m.max_running_runs.is_some(), "mcp.max_running_runs");
        add(
            m.max_queued_per_worker.is_some(),
            "mcp.max_queued_per_worker",
        );
        add(m.wait_default_secs.is_some(), "mcp.wait_default_secs");
        add(m.wait_ceiling_secs.is_some(), "mcp.wait_ceiling_secs");
        keys.sort();
        keys
    }
}

/// What reading the configuration gave: where it looked, whether a file was
/// there, and the configuration or the reason it is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// Where the file is looked for; `None` when no place can be named (no
    /// `DELEGA_CONFIG`, no absolute `XDG_CONFIG_HOME`, no absolute `HOME`).
    pub location: Option<Location>,
    /// Whether something exists at the path, usable or not.
    pub found: bool,
    /// The configuration (`None` when there is no file) or the refusal.
    pub result: Result<Option<Config>, crate::provider::Refusal>,
}

impl Status {
    /// The configuration, when a file was read.
    pub fn config(&self) -> Option<&Config> {
        self.result.as_ref().ok().and_then(Option::as_ref)
    }

    /// The refusal, when the file cannot be used.
    pub fn refusal(&self) -> Option<&crate::provider::Refusal> {
        self.result.as_ref().err()
    }

    /// The configuration to apply: the file's, [`Config::empty`] when there is
    /// no file, or the refusal.
    pub fn into_config(self) -> Result<Config, crate::provider::Refusal> {
        self.result.map(|c| c.unwrap_or_else(Config::empty))
    }
}

/// Where the configuration file is, from a lookup of environment variables.
/// The error is a refusal for a `DELEGA_CONFIG` that is not an absolute path.
pub fn locate(
    lookup: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Result<Option<Location>, crate::provider::Refusal> {
    let value = |name: &str| lookup(name).filter(|v| !v.is_empty());
    if let Some(explicit) = value("DELEGA_CONFIG") {
        let path = PathBuf::from(explicit);
        if !path.is_absolute() {
            return Err(refusal(
                UNREADABLE,
                "DELEGA_CONFIG must be an absolute path to the configuration file".to_string(),
            ));
        }
        return Ok(Some(Location {
            path,
            source: Source::DelegaConfig,
        }));
    }
    let absolute = |name: &str| value(name).map(PathBuf::from).filter(|p| p.is_absolute());
    if let Some(base) = absolute("XDG_CONFIG_HOME") {
        return Ok(Some(Location {
            path: base.join("delega").join(FILE_NAME),
            source: Source::XdgConfigHome,
        }));
    }
    Ok(absolute("HOME").map(|home| Location {
        path: home.join(".config").join("delega").join(FILE_NAME),
        source: Source::Home,
    }))
}

pub(crate) fn refusal(kind: &str, message: String) -> crate::provider::Refusal {
    crate::provider::Refusal {
        kind: kind.to_string(),
        message,
    }
}

/// Read the configuration file named by a lookup of environment variables, as
/// the current user.
pub fn read(lookup: impl Fn(&str) -> Option<std::ffi::OsString>) -> Status {
    let euid = rustix::process::geteuid().as_raw();
    read_as(lookup, euid)
}

/// [`read`] for a given user id, so a test can stand in for a user who does not
/// own what a temporary directory holds.
pub fn read_as(lookup: impl Fn(&str) -> Option<std::ffi::OsString>, euid: u32) -> Status {
    let location = match locate(lookup) {
        Ok(location) => location,
        Err(refusal) => {
            return Status {
                location: None,
                found: false,
                result: Err(refusal),
            };
        }
    };
    let Some(place) = location else {
        return Status {
            location: None,
            found: false,
            result: Ok(None),
        };
    };
    let outcome = file::read(&place.path, euid);
    let (found, result) = match outcome {
        Ok(file::Read::Missing) if place.source == Source::DelegaConfig => (
            false,
            Err(refusal(
                UNREADABLE,
                format!(
                    "the configuration file {} (named by DELEGA_CONFIG) does not exist",
                    display_path(&place.path)
                ),
            )),
        ),
        Ok(file::Read::Missing) => (false, Ok(None)),
        Ok(file::Read::Bytes(bytes)) => (true, check::parse(&place.path, &bytes).map(Some)),
        Err(why) => (
            true,
            Err(refusal(
                UNREADABLE,
                format!(
                    "the configuration file {} cannot be used: {why}",
                    display_path(&place.path)
                ),
            )),
        ),
    };
    Status {
        location: Some(place),
        found,
        result,
    }
}

/// The path as text for a message: control characters shown as `?`.
pub fn display_path(path: &Path) -> String {
    check::clean(&path.display().to_string())
}
