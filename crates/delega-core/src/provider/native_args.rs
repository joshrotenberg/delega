//! The allowlist for `native_args`.
//!
//! `native_args` passes provider options through. Each provider has a short
//! list of options that cannot change permissions, sandboxing, the working
//! directory, the configuration location or the output format, and each option
//! has a rule for its value. Anything else is refused before the provider
//! starts. A message names the option, never the value.
//!
//! An option may be written `--flag value` or `--flag=value`.

use crate::contract::request::Provider;

use super::limits::{NATIVE_NAME_CHARS, NATIVE_NUMBER_CHARS};
use super::refusal::Refusal;

/// What a value must look like.
#[derive(Debug, Clone, Copy)]
enum Value {
    /// A whole number of at least 1.
    Positive,
    /// A decimal number such as `0.5`.
    Decimal,
    /// A model-like name: starts with a letter or digit, then letters, digits
    /// and `. _ : / -`.
    Name,
    /// `key=value` where the key and the value are both listed.
    Setting(&'static [(&'static str, &'static [&'static str])]),
}

struct Rule {
    flags: &'static [&'static str],
    value: Value,
}

const CLAUDE: &[Rule] = &[
    Rule {
        flags: &["--max-turns"],
        value: Value::Positive,
    },
    Rule {
        flags: &["--max-budget-usd"],
        value: Value::Decimal,
    },
    Rule {
        flags: &["--fallback-model"],
        value: Value::Name,
    },
];

const CODEX: &[Rule] = &[Rule {
    flags: &["-c", "--config"],
    value: Value::Setting(&[
        ("model_verbosity", &["low", "medium", "high"]),
        (
            "model_reasoning_summary",
            &["auto", "concise", "detailed", "none"],
        ),
    ]),
}];

/// Options that are named in refusals as unsafe to pass through, so the
/// message can say why. Not an exhaustive list: anything off the allowlist is
/// refused either way.
const SENSITIVE: &[&str] = &[
    "--permission-mode",
    "--permission-prompts",
    "--dangerously-skip-permissions",
    "--allow-dangerously-skip-permissions",
    "--allowedTools",
    "--allowed-tools",
    "--disallowedTools",
    "--disallowed-tools",
    "--tools",
    "--settings",
    "--setting-sources",
    "--add-dir",
    "--mcp-config",
    "--strict-mcp-config",
    "--plugin-dir",
    "--output-format",
    "--input-format",
    "--append-system-prompt",
    "--system-prompt",
    "--resume",
    "--continue",
    "--session-id",
    "--sandbox",
    "-s",
    "--ask-for-approval",
    "-a",
    "--full-auto",
    "--dangerously-bypass-approvals-and-sandbox",
    "--yolo",
    "--cd",
    "-C",
    "--json",
    "--output-last-message",
    "--output-schema",
    "--profile",
    "-p",
    "--ignore-user-config",
    "--model",
    "-m",
    "-i",
    "--image",
];

fn rules(provider: Provider) -> &'static [Rule] {
    match provider {
        Provider::Claude => CLAUDE,
        Provider::Codex => CODEX,
    }
}

/// The allowed options for a provider, as text for the documentation and for
/// `delega doctor`: one entry per option with a description of its value.
pub fn allowlist(provider: Provider) -> Vec<String> {
    rules(provider)
        .iter()
        .map(|rule| {
            let flags = rule.flags.join(" or ");
            let value = match rule.value {
                Value::Positive => "a whole number of at least 1".to_string(),
                Value::Decimal => "a decimal number".to_string(),
                Value::Name => "a model-like name".to_string(),
                Value::Setting(keys) => {
                    let list: Vec<String> = keys
                        .iter()
                        .map(|(key, values)| format!("{key}={{{}}}", values.join("|")))
                        .collect();
                    format!("one of {}", list.join(", "))
                }
            };
            format!("{flags} VALUE, where VALUE is {value}")
        })
        .collect()
}

fn valid(value: Value, text: &str) -> bool {
    match value {
        Value::Positive => {
            !text.is_empty()
                && text.len() <= NATIVE_NUMBER_CHARS
                && text.bytes().all(|b| b.is_ascii_digit())
                && text.bytes().any(|b| b != b'0')
        }
        Value::Decimal => {
            let mut parts = text.splitn(2, '.');
            let whole = parts.next().unwrap_or_default();
            let frac = parts.next();
            text.len() <= NATIVE_NUMBER_CHARS
                && !whole.is_empty()
                && whole.bytes().all(|b| b.is_ascii_digit())
                && frac.is_none_or(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()))
        }
        Value::Name => {
            let bytes = text.as_bytes();
            !bytes.is_empty()
                && bytes.len() <= NATIVE_NAME_CHARS
                && bytes[0].is_ascii_alphanumeric()
                && bytes[1..]
                    .iter()
                    .all(|b| b.is_ascii_alphanumeric() || b".:_/-".contains(b))
        }
        Value::Setting(keys) => text.split_once('=').is_some_and(|(key, val)| {
            keys.iter()
                .any(|(k, values)| *k == key && values.contains(&val))
        }),
    }
}

fn reject(index: usize, flag: &str, why: &str) -> Refusal {
    Refusal::new(
        "native_arg_rejected",
        format!("native_args[{index}] ({flag}): {why}"),
    )
}

/// Check `args` against the provider's allowlist and return them in the
/// normal `flag value` form, ready to append to the launch arguments.
pub fn check(provider: Provider, args: &[String]) -> Result<Vec<String>, Refusal> {
    let rules = rules(provider);
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if !arg.starts_with('-') {
            return Err(Refusal::new(
                "native_arg_rejected",
                format!(
                    "native_args[{i}]: a value without an option before it; an option and its \
                     value are given as `--flag value` or `--flag=value`"
                ),
            ));
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) => (flag, Some(value)),
            None => (arg.as_str(), None),
        };
        let Some(rule) = rules.iter().find(|r| r.flags.contains(&flag)) else {
            let why = if SENSITIVE.contains(&flag) {
                "this option can change permissions, sandboxing, the working directory, the \
                 configuration location, the session or the output format, so it is not \
                 passed through"
            } else {
                "this option is not in the allowlist for this provider"
            };
            return Err(reject(i, flag, why));
        };
        let (value, used) = match inline {
            Some(value) => (value, 1),
            None => match args.get(i + 1) {
                Some(next) => (next.as_str(), 2),
                None => return Err(reject(i, flag, "the option needs a value")),
            },
        };
        if !valid(rule.value, value) {
            return Err(reject(i, flag, "the value is not one this option allows"));
        }
        out.push(flag.to_string());
        out.push(value.to_string());
        i += used;
    }
    Ok(out)
}
