//! The environment a provider process receives.
//!
//! The policy starts from the parent's environment and removes three groups of
//! variables:
//!
//! 1. Variables that belong to the other provider, so a Claude Code worker does
//!    not see Codex credentials and the reverse. The session markers a Claude
//!    Code host sets for its own children are removed from a Claude worker too.
//! 2. Variables named like secrets (see [`is_secret_name`]), except those that
//!    belong to the provider being run, which it needs to authenticate.
//! 3. Variables that configure delega itself (`DELEGA_*`).
//!
//! The caller can pass a stripped variable through by name
//! (`ProviderOptions::env_pass`) and can set variables outright
//! (`ProviderOptions::env_set`). The adapter's own variables are applied last
//! and cannot be overridden.
//!
//! Only names are ever reported. Nothing here returns or logs a value except
//! to the child process.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;

use crate::contract::request::Provider;
use crate::process::EnvPolicy;

/// The environment resolved for one provider process.
#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedEnv {
    vars: BTreeMap<OsString, OsString>,
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

/// Apply the policy.
pub(super) fn resolve(inputs: &EnvInputs<'_>) -> ResolvedEnv {
    let mut vars: BTreeMap<OsString, OsString> = BTreeMap::new();
    for (name, value) in inputs.parent {
        let Some(text) = name.to_str() else {
            continue;
        };
        if inherited(inputs.provider, text, inputs.pass) {
            vars.insert(name.clone(), value.clone());
        }
    }
    for (name, value) in inputs.set.iter().chain(inputs.adapter) {
        vars.insert(OsString::from(name), OsString::from(value));
    }
    ResolvedEnv { vars }
}

fn inherited(provider: Provider, name: &str, pass: &[String]) -> bool {
    if pass.iter().any(|p| p == name) {
        return true;
    }
    if is_other_providers(provider, name) || name.starts_with("DELEGA_") {
        return false;
    }
    owned_by(provider, name) || !is_secret_name(name)
}

/// Whether a variable belongs to the provider being run.
pub fn owned_by(provider: Provider, name: &str) -> bool {
    match provider {
        Provider::Claude => {
            name == "CLAUDECODE" || name.starts_with("CLAUDE_") || name.starts_with("ANTHROPIC_")
        }
        Provider::Codex => name.starts_with("CODEX_") || name.starts_with("OPENAI_"),
    }
}

/// Whether a variable belongs to the provider that is not being run, or is a
/// session marker of the host that must not reach a child.
pub fn is_other_providers(provider: Provider, name: &str) -> bool {
    match provider {
        Provider::Codex => {
            name == "CLAUDECODE" || name.starts_with("CLAUDE_") || name.starts_with("ANTHROPIC_")
        }
        Provider::Claude => {
            name.starts_with("CODEX_")
                || name.starts_with("OPENAI_")
                || matches!(
                    name,
                    "CLAUDE_CODE_MESSAGING_SOCKET"
                        | "CLAUDE_CODE_MESSAGING_TOKEN"
                        | "CLAUDE_CODE_BRIDGE_SESSION_ID"
                )
        }
    }
}

/// Whether a name looks like it holds a secret or grants access to one.
///
/// A name matches when, in upper case, it contains `SECRET`, `TOKEN`,
/// `PASSWORD`, `PASSWD`, `CREDENTIAL`, `PRIVATE_KEY` or `ACCESS_KEY`, ends in
/// `_KEY`, `_PAT` or `APIKEY`, or is `SSH_AUTH_SOCK`. It is a heuristic: a
/// secret under an unremarkable name is passed.
pub fn is_secret_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    const CONTAINS: [&str; 7] = [
        "SECRET",
        "TOKEN",
        "PASSWORD",
        "PASSWD",
        "CREDENTIAL",
        "PRIVATE_KEY",
        "ACCESS_KEY",
    ];
    const ENDS: [&str; 3] = ["_KEY", "_PAT", "APIKEY"];
    upper == "SSH_AUTH_SOCK"
        || CONTAINS.iter().any(|c| upper.contains(c))
        || ENDS.iter().any(|e| upper.ends_with(e))
}
