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
//! 2. `.claude/settings.json` under the working directory (only there, as
//!    Claude Code reads it);
//! 3. `.claude/settings.local.json` under the working directory, then under the
//!    root of the git repository the working directory is inside, then under the
//!    main checkout when that root is a linked worktree. The root is found by
//!    walking up to the nearest `.git` directory or file; `git` is not run.
//!
//! The directories come from the environment the worker will receive, not from
//! the calling process. Managed (enterprise) settings are not read.
//!
//! Rules are passed as written. A `Read` or `Edit` rule with exactly one leading
//! slash means a path from the directory of its settings file, which is lost
//! when the rule is passed inline, so an absolute rule is added beside it.
//!
//! A missing file contributes nothing. A file that exists and cannot be read,
//! is not a regular file, is too large, is not JSON, or holds a `permissions`
//! or `permissions.deny` of the wrong type refuses the run, naming the file and
//! never its content: a rule the user set is not dropped silently.

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};

use rustix::fs::{Mode, OFlags};
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
    /// The rules in the file, before deduplication.
    pub rules: usize,
}

/// The deny rules to pass to a worker, and where they came from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct DenyRules {
    /// The rules as they are passed: without duplicates, one-slash rules with their absolute form.
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
        value.filter(|v| !v.is_empty()).map(|v| {
            let joined = cwd.join(Path::new(v));
            std::fs::canonicalize(&joined).unwrap_or(joined)
        })
    };
    let home = absolute(home);
    let config_dir = absolute(config_dir);
    let mut out = DenyRules::default();

    // Each source is a file and the directory a one-slash rule in it means.
    let mut sources: Vec<(PathBuf, PathBuf)> = Vec::new();
    match (&config_dir, &home) {
        (Some(dir), _) => sources.push((dir.join("settings.json"), dir.clone())),
        (None, Some(home)) => {
            let dir = home.join(".claude");
            sources.push((dir.join("settings.json"), dir));
        }
        (None, None) => out.user_settings_unlocated = true,
    }
    let project = |dir: &Path, name: &str| (dir.join(".claude").join(name), dir.to_path_buf());
    sources.push(project(cwd, "settings.json"));
    sources.push(project(cwd, "settings.local.json"));
    for root in repository_roots(cwd) {
        let source = project(&root, "settings.local.json");
        if !sources.iter().any(|(path, _)| *path == source.0) {
            sources.push(source);
        }
    }

    for (path, base) in sources {
        let base = base.as_path();
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
            for written in expanded(rule, base) {
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
    // Opened without blocking, so a pipe swapped in after the check above cannot
    // hold the run; the handle is checked again below.
    let file = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(std::fs::File::from)
    .map_err(|e| format!("it cannot be opened ({})", std::io::Error::from(e).kind()))?;
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
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(Vec::new());
    }
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

/// The rules to pass for a rule written in a settings file whose directory is
/// `base`.
///
/// The rule is always passed as written: Claude Code resolves `path` and
/// `./path` against the working directory in every settings source. The one
/// form that loses its meaning inline is a `Read` or `Edit` rule with exactly one
/// leading slash, which names a path from the settings file's directory, so an
/// absolute rule `//base/path` is added beside it. Adding a rule can only narrow
/// a run. Rules for other tools and rules with `//` or `~/` paths are unchanged.
fn expanded(rule: &str, base: &Path) -> Vec<String> {
    let mut rules = vec![rule.to_string()];
    let Some((tool, rest)) = rule.split_once('(') else {
        return rules;
    };
    if !matches!(tool, "Read" | "Edit") {
        return rules;
    }
    let Some(path) = rest.strip_suffix(')') else {
        return rules;
    };
    if !path.starts_with('/') || path.starts_with("//") {
        return rules;
    }
    let relative = path.trim_start_matches('/');
    let Some(base_text) = base.to_str() else {
        return rules;
    };
    if relative.is_empty() {
        return rules;
    }
    let mut literal = String::new();
    for c in base_text.trim_matches('/').chars() {
        if GLOB_CHARS.contains(&c) || c == '\\' {
            literal.push('\\');
        }
        literal.push(c);
    }
    let directory = if literal.is_empty() {
        String::new()
    } else {
        format!("{literal}/")
    };
    rules.push(format!("{tool}(//{directory}{relative})"));
    rules
}

/// The roots of the git repository the working directory is inside: the nearest
/// ancestor holding `.git`, and the main checkout when that `.git` is a file
/// of a linked worktree. Nothing is run, and a `.git` file that cannot be
/// followed leaves only the nearest root.
fn repository_roots(cwd: &Path) -> Vec<PathBuf> {
    let Some(root) = cwd.ancestors().find(|d| d.join(".git").exists()) else {
        return Vec::new();
    };
    let mut roots = vec![root.to_path_buf()];
    if let Some(main) = main_checkout(root)
        && main != root
    {
        roots.push(main);
    }
    roots
}

/// For a linked worktree, whose `.git` is a file reading `gitdir: <main>/.git/
/// worktrees/<name>`, the main checkout `<main>`.
fn main_checkout(root: &Path) -> Option<PathBuf> {
    let git = root.join(".git");
    if !std::fs::metadata(&git).ok()?.is_file() {
        return None;
    }
    let text = read(&git).ok()??;
    let text = String::from_utf8(text).ok()?;
    let target = text.lines().next()?.strip_prefix("gitdir:")?.trim();
    let target = root.join(target);
    let common = target
        .ancestors()
        .find(|a| a.file_name().is_some_and(|n| n == "worktrees"))?
        .parent()?;
    let main = common.parent()?;
    let main = std::fs::canonicalize(main).ok()?;
    main.join(".git").is_dir().then_some(main)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_are_passed_as_written_and_a_one_slash_rule_gains_an_absolute_one() {
        let base = Path::new("/home/u/.claude");
        for rule in [
            "Read(secret.txt)",
            "Read(./a/b)",
            "Read(**/x)",
            "Edit(~/x)",
            "Read(//abs/x)",
            "Bash(rm -rf /)",
            "Bash(/usr/bin/x)",
            "Read",
            "Read()",
            "Read(/)",
            "Read(x",
        ] {
            assert_eq!(expanded(rule, base), [rule], "{rule}");
        }
        assert_eq!(
            expanded("Edit(/out/x)", base),
            ["Edit(/out/x)", "Edit(//home/u/.claude/out/x)"]
        );
        assert_eq!(
            expanded("Read(/a[b]/c)", Path::new("/p[q]")),
            ["Read(/a[b]/c)", "Read(//p\\[q\\]/a[b]/c)"]
        );
        assert_eq!(
            expanded("Read(/x)", Path::new("/")),
            ["Read(/x)", "Read(//x)"]
        );
    }
}
