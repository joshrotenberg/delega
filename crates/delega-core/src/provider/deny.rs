//! The deny rules of the user's and the project's Claude Code settings.
//!
//! A Claude worker starts with settings sources off, so none of the user's
//! settings reach it. Deny rules are the one exception: a rule the user wrote
//! can only narrow a run, so delega reads `permissions.deny` from the settings
//! files itself and passes the rules on. Nothing else in those files is read
//! into any value this module returns: not hooks, `env`, `apiKeyHelper`,
//! plugins, skills, MCP servers or instructions.
//!
//! Files, in this order:
//!
//! 1. user settings: `settings.json` in the directory the worker would get as
//!    `CLAUDE_CONFIG_DIR`, or `.claude/settings.json` under the `HOME` the
//!    worker would get when that variable is absent;
//! 2. `.claude/settings.json` under the working directory;
//! 3. `.claude/settings.local.json` under the working directory.
//!
//! The directories come from the environment the worker will receive, not from
//! the calling process. Managed (enterprise) settings are not read.
//!
//! A missing file contributes nothing. A file that exists and cannot be read,
//! is not a regular file, is too large, is not JSON, or holds a `permissions`
//! or `permissions.deny` of the wrong type refuses the run, naming the file and
//! never its content: a rule the user set is not dropped silently.

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use super::limits::{
    MAX_DENY_RULE_CHARS, MAX_DENY_RULES, MAX_DENY_RULES_PER_FILE, SETTINGS_FILE_BYTES,
};
use super::refusal::Refusal;

/// Refusal kind: a settings file exists and cannot be used.
pub(super) const UNREADABLE: &str = "settings_unreadable";
/// Refusal kind: the deny rules are over a bound.
pub(super) const OVER_LIMIT: &str = "deny_rules_over_limit";

/// A settings file that was read, and how many deny rules it held.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DenyFile {
    /// The file's path.
    pub path: String,
    /// The rules in the file, before anchoring and deduplication.
    pub rules: usize,
}

/// The deny rules to pass to a worker, and where they came from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct DenyRules {
    /// The rules as they are passed: anchored, without duplicates.
    pub rules: Vec<String>,
    /// The files that existed and were read, in order.
    pub files: Vec<DenyFile>,
    /// Whether the worker would have neither a configuration directory nor a
    /// home directory, so no user settings file could be located.
    pub user_settings_unlocated: bool,
}

impl DenyRules {
    /// The `provider_notes` for these rules: paths and counts, never rules.
    pub(super) fn notes(&self) -> Vec<String> {
        let mut notes = Vec::new();
        if self.user_settings_unlocated {
            notes.push(
                "the worker's environment has neither CLAUDE_CONFIG_DIR nor HOME, so the user's \
                 Claude Code settings file was not looked for"
                    .to_string(),
            );
        }
        let from: Vec<String> = self
            .files
            .iter()
            .filter(|f| f.rules > 0)
            .map(|f| format!("{} ({})", f.path, f.rules))
            .collect();
        notes.push(if self.rules.is_empty() {
            "no deny rules were found in the user's or the project's Claude Code settings files, \
             so none were applied"
                .to_string()
        } else {
            format!(
                "{} deny rules from the user's and the project's Claude Code settings files were \
                 applied to the worker; they can only narrow the run. Files: {}",
                self.rules.len(),
                from.join(", ")
            )
        });
        notes
    }
}

/// Read the deny rules for a worker that will run in `cwd` with the given
/// `HOME` and `CLAUDE_CONFIG_DIR` (as the worker will see them, `None` or empty
/// when absent).
pub(super) fn collect(
    home: Option<&OsStr>,
    config_dir: Option<&OsStr>,
    cwd: &Path,
) -> Result<DenyRules, Refusal> {
    let absolute = |value: Option<&OsStr>| {
        value
            .filter(|v| !v.is_empty())
            .map(|v| cwd.join(Path::new(v)))
    };
    let home = absolute(home);
    let config_dir = absolute(config_dir);
    let mut out = DenyRules::default();

    let mut sources: Vec<(PathBuf, PathBuf)> = Vec::new();
    match (&config_dir, &home) {
        (Some(dir), base) => sources.push((
            dir.join("settings.json"),
            base.clone().unwrap_or_else(|| dir.clone()),
        )),
        (None, Some(home)) => {
            sources.push((home.join(".claude").join("settings.json"), home.clone()));
        }
        (None, None) => out.user_settings_unlocated = true,
    }
    for name in ["settings.json", "settings.local.json"] {
        sources.push((cwd.join(".claude").join(name), cwd.to_path_buf()));
    }

    for (path, base) in sources {
        let shown = path.display().to_string();
        let Some(bytes) = read(&path).map_err(|why| unreadable(&shown, &why))? else {
            continue;
        };
        let rules = parse(&bytes).map_err(|why| unreadable(&shown, &why))?;
        if rules.len() > MAX_DENY_RULES_PER_FILE {
            return Err(Refusal::new(
                OVER_LIMIT,
                format!(
                    "{shown} has {} deny rules, over the limit of {MAX_DENY_RULES_PER_FILE} per \
                     file; no rule is dropped to fit, so shorten the list",
                    rules.len()
                ),
            ));
        }
        for (i, rule) in rules.iter().enumerate() {
            if rule.chars().count() > MAX_DENY_RULE_CHARS {
                return Err(Refusal::new(
                    OVER_LIMIT,
                    format!(
                        "deny rule number {} of {shown} is longer than {MAX_DENY_RULE_CHARS} \
                         characters",
                        i + 1
                    ),
                ));
            }
        }
        out.files.push(DenyFile {
            path: shown,
            rules: rules.len(),
        });
        for rule in &rules {
            for written in anchored(rule, &base) {
                if !out.rules.contains(&written) {
                    out.rules.push(written);
                }
            }
        }
        if out.rules.len() > MAX_DENY_RULES {
            return Err(Refusal::new(
                OVER_LIMIT,
                format!(
                    "the deny rules from the settings files come to over {MAX_DENY_RULES} after \
                     anchoring and removing duplicates; no rule is dropped to fit, so shorten \
                     the lists"
                ),
            ));
        }
    }
    Ok(out)
}

fn unreadable(path: &str, why: &str) -> Refusal {
    Refusal::new(
        UNREADABLE,
        format!(
            "the Claude Code settings file {path} cannot be used for its deny rules: {why}. The \
             run is refused instead of starting without a rule you set"
        ),
    )
}

/// The content of a regular file of at most [`SETTINGS_FILE_BYTES`], `None`
/// when it does not exist. A device, pipe or directory is refused before it is
/// opened, so reading cannot block or run without end.
fn read(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(None);
        }
        Err(e) => return Err(format!("it cannot be inspected ({})", e.kind())),
    };
    if !meta.is_file() {
        return Err("it is not a regular file".to_string());
    }
    let file =
        std::fs::File::open(path).map_err(|e| format!("it cannot be opened ({})", e.kind()))?;
    // The file could have been replaced between the two calls.
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return Err("it is not a regular file".to_string());
    }
    let mut bytes = Vec::new();
    file.take(SETTINGS_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("it cannot be read ({})", e.kind()))?;
    if bytes.len() > SETTINGS_FILE_BYTES {
        return Err(format!(
            "it is larger than the limit of {SETTINGS_FILE_BYTES} bytes"
        ));
    }
    Ok(Some(bytes))
}

/// The `permissions.deny` strings of a settings file. Everything else in the
/// file is dropped here, and no message quotes any of it.
fn parse(bytes: &[u8]) -> Result<Vec<String>, String> {
    let value: Value = serde_json::from_slice(bytes).map_err(|e| {
        format!(
            "it is not valid JSON (line {}, column {})",
            e.line(),
            e.column()
        )
    })?;
    let Value::Object(top) = value else {
        return Err("its top level is not a JSON object".to_string());
    };
    let Some(permissions) = top.get("permissions") else {
        return Ok(Vec::new());
    };
    let Value::Object(permissions) = permissions else {
        return Err("`permissions` is not an object".to_string());
    };
    let Some(deny) = permissions.get("deny") else {
        return Ok(Vec::new());
    };
    let Value::Array(list) = deny else {
        return Err("`permissions.deny` is not a list".to_string());
    };
    let mut rules = Vec::with_capacity(list.len().min(MAX_DENY_RULES_PER_FILE + 1));
    for (i, item) in list.iter().enumerate() {
        let Value::String(rule) = item else {
            return Err(format!(
                "`permissions.deny` entry {} is not a string",
                i + 1
            ));
        };
        if rule.trim().is_empty() {
            return Err(format!("`permissions.deny` entry {} is empty", i + 1));
        }
        if rule.chars().any(char::is_control) {
            return Err(format!(
                "`permissions.deny` entry {} contains a control character",
                i + 1
            ));
        }
        rules.push(rule.clone());
    }
    Ok(rules)
}

/// Characters that are glob syntax in a rule path.
const GLOB_CHARS: [char; 6] = ['*', '?', '[', ']', '{', '}'];

/// Rules for a rule written in a file whose relative paths mean `base`.
///
/// A `Read` or `Edit` rule with a relative path (`secret.txt`, `./secret.txt`,
/// `docs/**`) is rewritten to an absolute path under `base`, so it keeps its
/// meaning whatever the worker's working directory is: `docs/**` becomes
/// `//base/**/docs/**`, and a rule written with a single leading slash, which
/// names a path from the root of its settings, becomes `//base/path`. Rules for
/// other tools, rules with `//` or `~/` paths and rules with a `:` are returned
/// unchanged. When `base` has glob or parenthesis characters, which this
/// rewrite would turn into syntax, they are escaped and the original rule is
/// kept as well. Keeping a rule is never wider than the user's text.
fn anchored(rule: &str, base: &Path) -> Vec<String> {
    let unchanged = || vec![rule.to_string()];
    let Some((tool, rest)) = rule.split_once('(') else {
        return unchanged();
    };
    if !matches!(tool, "Read" | "Edit") {
        return unchanged();
    }
    let Some(path) = rest.strip_suffix(')') else {
        return unchanged();
    };
    if path.is_empty() || path.starts_with("//") || path.starts_with("~/") {
        return unchanged();
    }
    let rooted = path.starts_with('/');
    let trimmed = path.trim_start_matches('/');
    let relative = trimmed.strip_prefix("./").unwrap_or(trimmed);
    if relative.is_empty() || relative.contains(':') {
        return unchanged();
    }
    let Some(base_text) = base.to_str() else {
        return unchanged();
    };
    let mut literal = String::new();
    let mut needs_fallback = false;
    for c in base_text.trim_matches('/').chars() {
        if GLOB_CHARS.contains(&c) || c == '\\' {
            literal.push('\\');
            needs_fallback = true;
        }
        if c == '(' || c == ')' {
            needs_fallback = true;
        }
        literal.push(c);
    }
    let directory = if literal.is_empty() {
        String::new()
    } else {
        format!("{literal}/")
    };
    let prefix = if rooted { "" } else { "**/" };
    let mut rules = vec![format!("{tool}(//{directory}{prefix}{relative})")];
    if needs_fallback {
        rules.push(rule.to_string());
    }
    rules
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchoring_writes_an_absolute_path_for_a_relative_file_rule() {
        let base = Path::new("/home/u");
        assert_eq!(
            anchored("Read(secret.txt)", base),
            ["Read(//home/u/**/secret.txt)"]
        );
        assert_eq!(anchored("Read(./a/b)", base), ["Read(//home/u/**/a/b)"]);
        assert_eq!(
            anchored("Edit(/rooted/x)", base),
            ["Edit(//home/u/rooted/x)"]
        );
        assert_eq!(
            anchored("Read(docs/**)", Path::new("/")),
            ["Read(//**/docs/**)"]
        );
    }

    #[test]
    fn anchoring_leaves_other_rules_alone() {
        let base = Path::new("/home/u");
        for rule in [
            "Bash(rm -rf /)",
            "Bash",
            "Read",
            "Read(//abs/x)",
            "Read(~/x)",
            "Read()",
            "Read(C:x)",
            "WebFetch(domain:example.com)",
            "Write(x)",
            "mcp__server__tool",
            "Read(x",
        ] {
            assert_eq!(anchored(rule, base), [rule], "{rule}");
        }
    }

    #[test]
    fn a_base_with_glob_or_parenthesis_characters_is_escaped_and_the_original_kept() {
        let rules = anchored("Read(x)", Path::new("/a[b]/c(d)"));
        assert_eq!(rules, ["Read(//a\\[b\\]/c(d)/**/x)", "Read(x)"]);
    }
}
