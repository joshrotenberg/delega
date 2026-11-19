//! From the file's bytes to a [`Config`], or the problems that refuse it.
//!
//! A message names the file and the key or the line and never repeats a value
//! from the file. The parser's own messages and the type errors of `serde` can
//! quote what they read, so neither is shown. The structure is checked against
//! the generated JSON Schema instead ([`super::schema`]) and each error is
//! reported from its location and its keyword: the key path, the bounds the
//! schema states, the names of the keys that are not known, and the values the
//! schema accepts for an enumeration. Those come from the schema or from key
//! names in the file, never from a value.

use std::path::Path;
use std::sync::OnceLock;

use jsonschema::ValidationError;
use jsonschema::Validator;
use jsonschema::error::ValidationErrorKind;
use serde_json::Value;

use crate::contract::request::Request;
use crate::provider::Refusal;

use super::{Config, INVALID, MAX_CONFIG_BYTES, WAIT_BOUND_SECS, display_path, refusal, schema};

/// How many problems a message lists before it counts the rest.
const MAX_LISTED: usize = 10;
/// Longest key name repeated in a message, in characters.
const MAX_KEY_CHARS: usize = 64;

/// Text from the environment or the file made safe to print: control
/// characters shown as `?` and a long text cut.
pub fn clean(text: &str) -> String {
    let mut out: String = text
        .chars()
        .take(MAX_KEY_CHARS * 4)
        .map(|c| if c.is_control() { '?' } else { c })
        .collect();
    if text.chars().count() > MAX_KEY_CHARS * 4 {
        out.push_str("...");
    }
    out
}

fn key(text: &str) -> String {
    let mut out: String = text
        .chars()
        .take(MAX_KEY_CHARS)
        .map(|c| if c.is_control() { '?' } else { c })
        .collect();
    if text.chars().count() > MAX_KEY_CHARS {
        out.push_str("...");
    }
    out
}

fn validator() -> Option<&'static Validator> {
    static VALIDATOR: OnceLock<Option<Validator>> = OnceLock::new();
    VALIDATOR
        .get_or_init(|| jsonschema::validator_for(&schema::value()).ok())
        .as_ref()
}

fn invalid(path: &Path, problems: &[String]) -> Refusal {
    let mut shown: Vec<String> = problems.iter().take(MAX_LISTED).cloned().collect();
    if problems.len() > MAX_LISTED {
        shown.push(format!("and {} more", problems.len() - MAX_LISTED));
    }
    refusal(
        INVALID,
        format!(
            "the configuration file {} is not valid: {}",
            display_path(path),
            shown.join("; ")
        ),
    )
}

/// The line and column (from 1) of byte `offset` in `text`.
fn position(text: &str, offset: usize) -> (usize, usize) {
    let end = offset.min(text.len());
    let before = text.as_bytes().get(..end).unwrap_or_default();
    let line = before.iter().filter(|b| **b == b'\n').count() + 1;
    let start = before
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(0, |i| i + 1);
    let column = String::from_utf8_lossy(before.get(start..).unwrap_or_default())
        .chars()
        .count()
        + 1;
    (line, column)
}

/// Read the file's content: UTF-8, TOML, the schema, then the rules the schema
/// cannot state.
pub(super) fn parse(path: &Path, bytes: &[u8]) -> Result<Config, Refusal> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Err(invalid(path, &["it is not valid UTF-8".to_string()]));
    };
    debug_assert!(bytes.len() <= MAX_CONFIG_BYTES);
    let table: toml::Table = match text.parse() {
        Ok(table) => table,
        Err(err) => {
            let place = err.span().map_or(String::new(), |span| {
                let (line, column) = position(text, span.start);
                format!(" (line {line}, column {column})")
            });
            return Err(invalid(path, &[format!("it is not valid TOML{place}")]));
        }
    };
    let Ok(document) = serde_json::to_value(&table) else {
        return Err(invalid(
            path,
            &["it cannot be read as a document".to_string()],
        ));
    };
    let Some(validator) = validator() else {
        return Err(invalid(
            path,
            &["the format could not be checked (internal error)".to_string()],
        ));
    };
    let mut problems: Vec<String> = Vec::new();
    for error in validator.iter_errors(&document) {
        let at = location(&error.instance_path().to_string(), &document);
        problems.extend(describe(&at, error.kind(), &document));
    }
    problems.sort();
    problems.dedup();
    if !problems.is_empty() {
        return Err(invalid(path, &problems));
    }
    let config: Config = match serde_json::from_value(document) {
        Ok(config) => config,
        Err(_) => {
            return Err(invalid(
                path,
                &["it does not match the configuration format".to_string()],
            ));
        }
    };
    let mut problems = rules(&config);
    problems.sort();
    if !problems.is_empty() {
        return Err(invalid(path, &problems));
    }
    env_names(path, &config)?;
    Ok(config)
}

/// A JSON pointer as a dotted key path. A key under `env.set` is a variable
/// name the file chose, and a mistaken `NAME=value` there would put a value in
/// the message, so it is shown as its position among the keys of `env.set` in
/// alphabetical order, as `<entry 2>`. Entries of `env.pass` are already shown
/// by index.
fn location(pointer: &str, document: &Value) -> String {
    let parts: Vec<String> = pointer
        .split('/')
        .skip(1)
        .map(|part| part.replace("~1", "/").replace("~0", "~"))
        .collect();
    let under_set = parts.len() > 2 && parts[0] == "env" && parts[1] == "set";
    parts
        .iter()
        .enumerate()
        .map(|(i, part)| {
            if under_set && i == 2 {
                let mut names: Vec<&String> = document
                    .pointer("/env/set")
                    .and_then(Value::as_object)
                    .map(|m| m.keys().collect())
                    .unwrap_or_default();
                names.sort();
                let at = names.iter().position(|n| *n == part).map_or(0, |p| p + 1);
                format!("<entry {at}>")
            } else {
                key(part)
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

fn join(at: &str, name: &str) -> String {
    if at.is_empty() {
        key(name)
    } else {
        format!("{at}.{}", key(name))
    }
}

/// The text for one schema error, with the key it is about. Never a value.
fn describe(at: &str, kind: &ValidationErrorKind, document: &Value) -> Vec<String> {
    let at_or_file = if at.is_empty() { "the file" } else { at };
    match kind {
        ValidationErrorKind::AdditionalProperties { unexpected } => unexpected
            .iter()
            .map(|name| {
                let full = join(at, name);
                if at == "defaults" && (name == "prompt" || name == "cwd") {
                    format!(
                        "{full} is not accepted: a prompt or a working directory belongs to one \
                         request, not to a default"
                    )
                } else {
                    format!("{full} is not a known key")
                }
            })
            .collect(),
        ValidationErrorKind::Required { property } => {
            let name = property.as_str().unwrap_or("a key");
            vec![format!("{} is required", join(at, name))]
        }
        ValidationErrorKind::Constant { expected_value } => {
            vec![format!("{at_or_file} must be {expected_value}")]
        }
        ValidationErrorKind::Minimum { limit } => {
            vec![format!("{at_or_file} must be at least {limit}")]
        }
        ValidationErrorKind::Maximum { limit } => {
            vec![format!("{at_or_file} must be at most {limit}")]
        }
        ValidationErrorKind::Type { kind } => {
            let expected = expected_type(&format!("{kind:?}"));
            vec![format!("{at_or_file} must be {expected}")]
        }
        ValidationErrorKind::AnyOf { context } | ValidationErrorKind::OneOfNotValid { context } => {
            let mut accepted: Vec<(String, String)> = Vec::new();
            let mut others: Vec<String> = Vec::new();
            for errors in context {
                for error in errors {
                    collect(error, &mut accepted, &mut others, document);
                }
            }
            others.sort();
            others.dedup();
            if !others.is_empty() {
                return others;
            }
            accepted.sort();
            accepted.dedup();
            match accepted.first() {
                None => vec![format!("{at_or_file} has a value that is not accepted")],
                Some((path, _)) => {
                    let path = if path.is_empty() { at_or_file } else { path };
                    let values: Vec<&str> = accepted.iter().map(|(_, v)| v.as_str()).collect();
                    vec![format!("{path} must be one of {}", values.join(", "))]
                }
            }
        }
        ValidationErrorKind::Enum { options } => {
            vec![format!("{at_or_file} must be one of {options}")]
        }
        other => vec![format!(
            "{at_or_file} has a value that is not accepted ({})",
            other.keyword()
        )],
    }
}

/// Sort what a branch of an `anyOf` or `oneOf` found: the values an
/// enumeration accepts, and the other problems. A branch that only expects
/// `null` is the "or absent" of an optional key, which a TOML file cannot
/// write, so it says nothing.
fn collect(
    error: &ValidationError<'_>,
    accepted: &mut Vec<(String, String)>,
    others: &mut Vec<String>,
    document: &Value,
) {
    let at = location(&error.instance_path().to_string(), document);
    match error.kind() {
        ValidationErrorKind::Constant { expected_value } => {
            accepted.push((at, expected_value.to_string()));
        }
        ValidationErrorKind::Enum { options } => {
            if let Value::Array(options) = options.as_ref() {
                accepted.extend(options.iter().map(|o| (at.clone(), o.to_string())));
            }
        }
        ValidationErrorKind::AnyOf { context } | ValidationErrorKind::OneOfNotValid { context } => {
            for errors in context {
                for error in errors {
                    collect(error, accepted, others, document);
                }
            }
        }
        ValidationErrorKind::Type { kind }
            if expected_type(&format!("{kind:?}")).starts_with("of another") => {}
        other => others.extend(describe(&at, other, document)),
    }
}

/// The type a `Type` error expects, from its debug text, in words.
fn expected_type(debug: &str) -> String {
    let words: Vec<&str> = ["string", "integer", "number", "boolean", "array", "object"]
        .into_iter()
        .filter(|w| debug.to_ascii_lowercase().contains(w))
        .collect();
    match words.as_slice() {
        [] => "of another type".to_string(),
        [one] => format!("{} {one}", article(one)),
        many => format!("one of {}", many.join(", ")),
    }
}

fn article(word: &str) -> &'static str {
    if word.starts_with(['a', 'e', 'i', 'o', 'u']) {
        "an"
    } else {
        "a"
    }
}

/// Refuse a name in `[env]` that `--env` would refuse, with the kind `--env`
/// gives. The message says where in the file it is: a position counts the
/// entries of `pass` in the file's order and the keys of `set` in alphabetical
/// order. It never repeats an invalid name.
fn env_names(path: &Path, config: &Config) -> Result<(), Refusal> {
    let set: Vec<(String, String)> = config
        .env
        .set
        .keys()
        .map(|name| (name.clone(), String::new()))
        .collect();
    let at = |section: &str, r: Refusal| Refusal {
        kind: r.kind,
        message: format!(
            "the configuration file {}, {section}: {}",
            display_path(path),
            r.message
        ),
    };
    crate::provider::env::check_caller_names(&config.env.pass, &[])
        .map_err(|r| at("[env] pass", r))?;
    crate::provider::env::check_caller_names(&[], &set).map_err(|r| at("[env.set]", r))
}

/// The rules the schema cannot state.
fn rules(config: &Config) -> Vec<String> {
    let mut problems = Vec::new();
    if let Some(model) = &config.defaults.model {
        let mut request = Request::new(crate::contract::request::Provider::Claude, "x");
        request.model = Some(model.clone());
        if let Err(found) = request.validate() {
            problems.extend(
                found
                    .into_iter()
                    .filter(|p| p.field == "model")
                    .map(|p| format!("defaults.model {}", p.message)),
            );
        }
    }
    for (name, entry) in [
        ("providers.claude.executable", &config.providers.claude),
        ("providers.codex.executable", &config.providers.codex),
    ] {
        let Some(path) = entry.as_ref().and_then(|e| e.executable.as_deref()) else {
            continue;
        };
        if path.contains('\0') {
            problems.push(format!("{name} must not contain NUL bytes"));
        } else if !Path::new(path).is_absolute() {
            problems.push(format!("{name} must be an absolute path"));
        } else if path.len() > crate::contract::limits::MAX_PATH_BYTES {
            problems.push(format!(
                "{name} is longer than {} bytes",
                crate::contract::limits::MAX_PATH_BYTES
            ));
        }
    }
    for (index, value) in config.env.set.values().enumerate() {
        if value.contains('\0') {
            problems.push(format!(
                "env.set.<entry {}> must not contain NUL bytes",
                index + 1
            ));
        }
    }
    let ceiling = config.mcp.wait_ceiling_secs.unwrap_or(WAIT_BOUND_SECS);
    if let Some(default) = config.mcp.wait_default_secs
        && default > ceiling
    {
        problems.push(format!(
            "mcp.wait_default_secs must not be more than the wait ceiling ({ceiling} seconds)"
        ));
    }
    problems
}
