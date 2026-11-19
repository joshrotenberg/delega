//! The environment a provider process receives.
//!
//! The policy is an allowlist. A worker receives only:
//!
//! 1. The base variables ([`BASE_NAMES`]): the user, the shell, the locale,
//!    temporary and base directories, and two switches.
//! 2. Network plumbing ([`NETWORK_NAMES`]): proxy and certificate settings.
//! 3. The variables that belong to the provider being run (see [`owned_by`]),
//!    except those that describe the calling session (see
//!    [`is_calling_session`]).
//! 4. Variables the caller names (`ProviderOptions::env_pass`), when they are
//!    set in the parent environment.
//! 5. Variables the caller sets to a value (`ProviderOptions::env_set`) and the
//!    adapter's own variables, applied last. An adapter variable cannot be
//!    overridden.
//!
//! Everything else is withheld, and so is every `DELEGA_*` variable and every
//! calling-session variable, even when named. [`is_secret_name`] is not part
//! of the allowlist. It marks values for redaction and flags a secret-looking
//! name the caller passed explicitly.
//!
//! Only names and counts are ever reported. Nothing here returns or logs a
//! value except to the child process.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fmt;

use serde::Serialize;

use crate::contract::request::Provider;
use crate::process::EnvPolicy;

use super::refusal::Refusal;

/// Base variables every worker receives, when the caller has them: who and
/// where the user is (`HOME`, `USER`, `LOGNAME`, `SHELL`), how to find programs
/// (`PATH`), locale and terminal (`LANG`, `LANGUAGE`, the `LC_*` categories
/// listed by name, `TERM`, `TZ`), a temporary directory
/// (`TMPDIR`), the base directories tools use to find their own configuration,
/// data, cache and state (`XDG_*_HOME`), and two switches, `NO_COLOR` and `CI`.
/// These are locations and switches; none carries a secret.
pub const BASE_NAMES: [&str; 29] = [
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_COLLATE",
    "LC_CTYPE",
    "LC_MESSAGES",
    "LC_MONETARY",
    "LC_NUMERIC",
    "LC_TIME",
    "LC_ADDRESS",
    "LC_IDENTIFICATION",
    "LC_MEASUREMENT",
    "LC_NAME",
    "LC_PAPER",
    "LC_TELEPHONE",
    "TERM",
    "TMPDIR",
    "TZ",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    "NO_COLOR",
    "CI",
];

/// Network plumbing: proxy settings in upper and lower case, and the
/// certificate settings that let a worker behind a TLS-inspecting proxy
/// connect. A proxy address can carry credentials, which is why these are
/// named here and not passed by a pattern.
pub const NETWORK_NAMES: [&str; 11] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NODE_EXTRA_CA_CERTS",
];

/// The environment resolved for one provider process.
#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedEnv {
    vars: BTreeMap<OsString, OsString>,
    report: EnvReport,
    named_secret_looking: Vec<String>,
    set_secret_looking: Vec<String>,
}

/// Which variables a worker would receive and why, as names. It holds no
/// values and, for what was withheld, only a count.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct EnvReport {
    /// Passed because they are in [`BASE_NAMES`].
    pub base: Vec<String>,
    /// Passed because they are in [`NETWORK_NAMES`].
    pub network: Vec<String>,
    /// Passed because they belong to the provider being run.
    pub provider: Vec<String>,
    /// Passed because the caller named them and nothing above covers them.
    pub named: Vec<String>,
    /// Set by the caller to a value (`env_set`), whatever the parent
    /// environment holds.
    pub set_by_caller: Vec<String>,
    /// Set by delega's adapter. These cannot be overridden.
    pub set_by_delega: Vec<String>,
    /// Named by the caller but not set in the parent environment, so not
    /// passed.
    pub named_not_set: Vec<String>,
    /// How many variables of the parent environment were not passed. Their
    /// names are not reported.
    pub withheld_count: usize,
}

impl ResolvedEnv {
    /// The variable names, sorted. Values are not available through this type's
    /// public interface.
    pub fn names(&self) -> Vec<String> {
        self.vars
            .keys()
            .map(|n| n.to_string_lossy().into_owned())
            .collect()
    }

    /// The value the worker will receive for `name`, when it gets one.
    pub(super) fn value(&self, name: &str) -> Option<&std::ffi::OsStr> {
        self.vars
            .get(std::ffi::OsStr::new(name))
            .map(OsString::as_os_str)
    }

    /// The names by reason, and the count of withheld variables.
    pub fn report(&self) -> &EnvReport {
        &self.report
    }

    /// The `provider_notes` that describe this environment: how many variables
    /// were withheld and passed, and each explicitly named variable whose name
    /// looks like a secret. Counts and names only.
    pub fn notes(&self) -> Vec<String> {
        let mut notes: Vec<String> = self
            .named_secret_looking
            .iter()
            .map(|name| {
                format!(
                    "environment variable {name} was passed because the caller named it; its \
                     name looks like a secret"
                )
            })
            .collect();
        notes.extend(self.set_secret_looking.iter().map(|name| {
            format!(
                "environment variable {name} was set to a value the caller gave; its name \
                 looks like a secret"
            )
        }));
        notes.push(format!(
            "{} environment variables of the caller were withheld; {} were passed",
            self.report.withheld_count,
            self.vars.len()
        ));
        notes
    }

    /// The values of the variables whose text must not appear in results: those
    /// with a secret-looking name or that belong to the provider.
    pub(super) fn secret_values(&self, provider: Provider) -> Vec<String> {
        self.vars
            .iter()
            .filter_map(|(name, value)| {
                let name = name.to_str()?;
                // A value that is not valid UTF-8 is kept in its lossy form,
                // which is how provider text is decoded.
                (is_secret_name(name) || owned_by(provider, name))
                    .then(|| value.to_string_lossy().into_owned())
            })
            .collect()
    }

    /// The process-layer policy that passes exactly these variables.
    pub(super) fn to_policy(&self) -> EnvPolicy {
        self.vars
            .iter()
            .fold(EnvPolicy::empty(), |policy, (name, value)| {
                policy.set(name.clone(), value.clone())
            })
    }
}

impl fmt::Debug for ResolvedEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedEnv")
            .field("names", &self.names())
            .finish()
    }
}

/// Everything the policy reads.
pub(super) struct EnvInputs<'a> {
    pub provider: Provider,
    pub parent: &'a [(OsString, OsString)],
    pub pass: &'a [String],
    pub set: &'a [(String, String)],
    pub adapter: &'a [(String, String)],
}

/// Whether a text is usable as an environment variable name: letters, digits
/// and underscore, not starting with a digit.
pub fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether a variable can never be passed, even by name: it describes the
/// calling session or configures delega itself.
pub fn is_never_passed(name: &str) -> bool {
    is_calling_session(name) || name.starts_with("DELEGA_")
}

/// Refuse names the caller asked to pass or set that cannot be used. An invalid
/// name is not echoed, because a mistaken `NAME=value` handed over as a name
/// would put a value in the message. A name delega itself sets for the provider
/// is refused too, instead of being dropped silently.
pub(super) fn check_names(
    pass: &[String],
    set: &[(String, String)],
    adapter: &[(String, String)],
) -> Result<(), Refusal> {
    let passed = pass.iter().map(|n| ("to pass", n.as_str()));
    let given = set.iter().map(|(n, _)| ("to set", n.as_str()));
    let mut counts = (0usize, 0usize);
    for (what, name) in passed.chain(given) {
        let number = if what == "to pass" {
            counts.0 += 1;
            counts.0
        } else {
            counts.1 += 1;
            counts.1
        };
        if !is_valid_name(name) {
            return Err(Refusal::new(
                "invalid_env_name",
                format!(
                    "environment variable name number {number} {what} is not a valid name \
                     (letters, digits and underscore, not starting with a digit)"
                ),
            ));
        }
        if is_calling_session(name) {
            return Err(Refusal::new(
                "forbidden_env_name",
                format!("{name} describes the calling session and cannot be given to a worker"),
            ));
        }
        if name.starts_with("DELEGA_") {
            return Err(Refusal::new(
                "forbidden_env_name",
                format!("{name} configures delega itself and cannot be given to a worker"),
            ));
        }
        if adapter.iter().any(|(n, _)| n == name) {
            return Err(Refusal::new(
                "forbidden_env_name",
                format!(
                    "{name} is set by delega for this provider and cannot be given to a worker"
                ),
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Reason {
    Base,
    Network,
    Provider,
    Named,
}

fn reason(provider: Provider, name: &str, pass: &BTreeSet<&str>) -> Option<Reason> {
    if is_never_passed(name) {
        return None;
    }
    if BASE_NAMES.contains(&name) {
        Some(Reason::Base)
    } else if NETWORK_NAMES.contains(&name) {
        Some(Reason::Network)
    } else if owned_by(provider, name) {
        Some(Reason::Provider)
    } else if pass.contains(name) {
        Some(Reason::Named)
    } else {
        None
    }
}

/// Apply the policy.
pub(super) fn resolve(inputs: &EnvInputs<'_>) -> ResolvedEnv {
    let pass: BTreeSet<&str> = inputs.pass.iter().map(String::as_str).collect();
    let set_names: BTreeSet<&str> = inputs
        .set
        .iter()
        .chain(inputs.adapter)
        .map(|(n, _)| n.as_str())
        .collect();
    let set_caller: BTreeSet<&str> = inputs.set.iter().map(|(n, _)| n.as_str()).collect();
    let mut vars: BTreeMap<OsString, OsString> = BTreeMap::new();
    let mut report = EnvReport::default();
    let mut parent_names: BTreeSet<&OsString> = BTreeSet::new();
    let mut passed_from_parent: BTreeSet<&OsString> = BTreeSet::new();
    let mut explicit_present: BTreeSet<&str> = BTreeSet::new();
    let mut groups: BTreeMap<&str, Reason> = BTreeMap::new();
    for (name, value) in inputs.parent {
        parent_names.insert(name);
        let Some(text) = name.to_str() else {
            continue;
        };
        let Some(why) = reason(inputs.provider, text, &pass) else {
            continue;
        };
        passed_from_parent.insert(name);
        vars.insert(name.clone(), value.clone());
        groups.insert(text, why);
        if matches!(why, Reason::Named) && !set_caller.contains(text) {
            explicit_present.insert(text);
        }
    }
    for (text, why) in groups {
        if set_names.contains(text) {
            continue;
        }
        let list = match why {
            Reason::Base => &mut report.base,
            Reason::Network => &mut report.network,
            Reason::Provider => &mut report.provider,
            Reason::Named => &mut report.named,
        };
        list.push(text.to_string());
    }
    for (name, value) in inputs.set.iter().chain(inputs.adapter) {
        vars.insert(OsString::from(name), OsString::from(value));
    }
    let adapter_names: BTreeSet<&str> = inputs.adapter.iter().map(|(n, _)| n.as_str()).collect();
    report.set_by_delega = adapter_names.iter().map(ToString::to_string).collect();
    report.set_by_caller = inputs
        .set
        .iter()
        .map(|(n, _)| n.as_str())
        .filter(|n| !adapter_names.contains(n))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(ToString::to_string)
        .collect();
    let set_secret_looking = report
        .set_by_caller
        .iter()
        .filter(|n| is_secret_name(n))
        .cloned()
        .collect();
    report.named_not_set = pass
        .iter()
        .filter(|n| {
            !is_never_passed(n)
                && !set_caller.contains(**n)
                && !inputs
                    .parent
                    .iter()
                    .any(|(name, _)| name.to_str() == Some(**n))
        })
        .map(ToString::to_string)
        .collect();
    report.withheld_count = parent_names.len() - passed_from_parent.len();
    let named_secret_looking = explicit_present
        .iter()
        .filter(|n| is_secret_name(n))
        .map(ToString::to_string)
        .collect();
    ResolvedEnv {
        vars,
        report,
        named_secret_looking,
        set_secret_looking,
    }
}

/// Whether a variable belongs to the provider being run.
pub fn owned_by(provider: Provider, name: &str) -> bool {
    match provider {
        Provider::Claude => name.starts_with("CLAUDE_") || name.starts_with("ANTHROPIC_"),
        Provider::Codex => name.starts_with("CODEX_") || name.starts_with("OPENAI_"),
    }
}

/// Names that describe the session or tool call that called delega, not how to
/// reach a provider account. They are withheld from both providers. The Codex
/// names are the ones Codex sets in the environment of the commands it runs
/// (observed on codex-cli 0.170.0).
const SESSION_NAMES: [&str; 18] = [
    "AI_AGENT",
    "CLAUDECODE",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_EFFORT",
    "CLAUDE_PID",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CODEX_SANDBOX",
    "CODEX_SANDBOX_NETWORK_DISABLED",
    "CODEX_SESSION_ID",
    "CODEX_THREAD_ID",
    "CODEX_TOOL_CALL_ID",
    "CODEX_CI",
    "CODEX_VERSION",
    "CODEX_APPLY_PATCH_PRESERVE_LINE_ENDINGS",
];

/// Whether a variable describes the calling session.
///
/// A name matches when it is in [`SESSION_NAMES`], or starts with
/// `CLAUDE_CODE_` and either contains `SESSION`, `MESSAGING` or `BRIDGE`, has
/// `SSE` as a whole word between underscores (`CLAUDE_CODE_SSE_PORT`), or ends
/// in `_PORT`, `_SOCKET` or `_PID`. Other `CLAUDE_*` names are configuration
/// (`CLAUDE_CONFIG_DIR`, `CLAUDE_CODE_OAUTH_TOKEN`) and are not matched, and
/// neither are Codex account and configuration names (`CODEX_HOME`,
/// `OPENAI_API_KEY`, `OPENAI_BASE_URL`).
pub fn is_calling_session(name: &str) -> bool {
    SESSION_NAMES.contains(&name)
        || (name.starts_with("CLAUDE_CODE_")
            && (["SESSION", "MESSAGING", "BRIDGE"]
                .iter()
                .any(|part| name.contains(part))
                || name.split('_').any(|word| word == "SSE")
                || ["_PORT", "_SOCKET", "_PID"]
                    .iter()
                    .any(|end| name.ends_with(end))))
}

/// Whether a name looks like it holds a secret or grants access to one.
///
/// A name matches when, in upper case, it contains `SECRET`, `TOKEN`,
/// `PASSWORD`, `PASSWD`, `CREDENTIAL`, `PRIVATE_KEY`, `ACCESS_KEY` or
/// `CONNECTION_STRING`, ends in `_KEY`, `_PAT`, `APIKEY`, `_URL`, `_URI` or
/// `_DSN`, or is `SSH_AUTH_SOCK`. Connection addresses count because they
/// often carry a user name and password. It is a heuristic: a secret under an
/// unremarkable name is passed.
pub fn is_secret_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    const CONTAINS: [&str; 8] = [
        "SECRET",
        "TOKEN",
        "PASSWORD",
        "PASSWD",
        "CREDENTIAL",
        "PRIVATE_KEY",
        "ACCESS_KEY",
        "CONNECTION_STRING",
    ];
    const ENDS: [&str; 6] = ["_KEY", "_PAT", "APIKEY", "_URL", "_URI", "_DSN"];
    upper == "SSH_AUTH_SOCK"
        || CONTAINS.iter().any(|c| upper.contains(c))
        || ENDS.iter().any(|e| upper.ends_with(e))
}
