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
//!    walking up to the nearest `.git` directory or file; `git` is not run. As
//!    Claude Code does, a root that is the home directory is not used. A root is
//!    also not used when the root, its `.git` entry, its `.claude` directory or
//!    the settings file is not owned by the current user, or when `.git`,
//!    `.claude` or the file is a symbolic link: links are never followed in a
//!    repository root, because another user can plant them in a tree the user
//!    owns but others can write. The file is then not read and is reported as
//!    ignored. The owner of the file is judged on the handle that is read, and
//!    the file is reached by opening each directory from the one above without
//!    following links, so it cannot be swapped after the check. A `gitdir:` line
//!    is followed only to a `.git/worktrees/<name>` directory of a `.git`
//!    directory whose `gitdir` file names this worktree's `.git` file; when it is
//!    not followed, a note says the main checkout could not be identified.
//!    The user's own file and the working directory's files may be symbolic
//!    links, as a dotfiles setup makes them.
//!
//! The directories come from the environment the worker will receive, not from
//! the calling process. Managed (enterprise) settings are not read.
//!
//! Rules are passed as written. A `Read` or `Edit` rule with exactly one leading
//! slash means a path from the directory of its settings file, which is lost
//! when the rule is passed inline, so absolute rules are added beside it: for the
//! user file under its directory, for a project or local file under the working
//! directory and, when the file is elsewhere, under its project directory.
//!
//! A missing file contributes nothing. A file that exists and cannot be read,
//! is not a regular file, is too large, is not JSON, or holds a `permissions`
//! or `permissions.deny` of the wrong type refuses the run, naming the file and
//! never its content: a rule the user set is not dropped silently.

use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use rustix::io::Errno;
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

/// A local settings file that was not read, and why.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SkippedFile {
    /// The file's path.
    pub path: String,
    /// Why it was ignored.
    pub reason: String,
}

/// The deny rules to pass to a worker, and where they came from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct DenyRules {
    /// The rules as they are passed: without duplicates, one-slash rules with their absolute form.
    pub rules: Vec<String>,
    /// The files that existed and were read, in order.
    pub files: Vec<DenyFile>,
    /// Existing files that were not read, and why.
    pub skipped: Vec<SkippedFile>,
    /// Other things the user should know, such as a main checkout that could
    /// not be identified.
    pub notices: Vec<String>,
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
        notes.extend(self.notices.iter().cloned());
        notes.extend(self.skipped.iter().map(|f| {
            format!(
                "the Claude Code settings file {} was ignored, so its deny rules were not \
                 applied: {}",
                f.path, f.reason
            )
        }));
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
    let owner = Owner {
        euid: rustix::process::geteuid().as_raw(),
        foreign: &[],
    };
    collect_as(home, config_dir, cwd, owner)
}

/// The user that must own what is read, with a way for a test to stand in for
/// another user.
#[derive(Clone, Copy)]
pub(super) struct Owner<'a> {
    /// The effective user id.
    pub euid: u32,
    /// Paths treated as owned by someone else whatever their real owner. Only
    /// a test sets this: the files it can make all belong to the test's user.
    pub foreign: &'a [PathBuf],
}

impl Owner<'_> {
    /// Whether the current user owns `path`, whose owner is `uid`.
    fn owns(&self, path: &Path, uid: u32) -> bool {
        if uid != self.euid {
            return false;
        }
        let real = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let here = real(path);
        !self.foreign.iter().any(|f| real(f) == here)
    }
}

/// A file to read, the directories a one-slash rule in it means, and, for a file
/// in a repository root, that root (real path), which makes the read refuse
/// links and judge owners.
struct Source {
    path: PathBuf,
    bases: Vec<PathBuf>,
    guarded_root: Option<PathBuf>,
}

/// [`collect`] for a given owner, so a test can stand in for a user who does
/// not own what a temporary directory holds.
pub(super) fn collect_as(
    home: Option<&OsStr>,
    config_dir: Option<&OsStr>,
    cwd: &Path,
    owner: Owner<'_>,
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

    // Each source is a file and the directories a one-slash rule in it means.
    let plain = |path: PathBuf, bases: Vec<PathBuf>| Source {
        path,
        bases,
        guarded_root: None,
    };
    let mut sources: Vec<Source> = Vec::new();
    match (&config_dir, &home) {
        (Some(dir), _) => sources.push(plain(dir.join("settings.json"), vec![dir.clone()])),
        (None, Some(home)) => {
            let dir = home.join(".claude");
            sources.push(plain(dir.join("settings.json"), vec![dir]));
        }
        (None, None) => out.user_settings_unlocated = true,
    }
    // A project or local file's one-slash rules mean the working directory and,
    // for a file elsewhere, its own project directory. Both only narrow.
    let project = |dir: &Path, name: &str| {
        let mut bases = vec![cwd.to_path_buf()];
        if dir != cwd {
            bases.push(dir.to_path_buf());
        }
        (dir.join(".claude").join(name), bases)
    };
    for name in ["settings.json", "settings.local.json"] {
        let (path, bases) = project(cwd, name);
        sources.push(plain(path, bases));
    }
    let (roots, notices) = repository_roots(cwd, owner);
    out.notices = notices;
    for root in roots {
        if root == cwd {
            continue;
        }
        let (path, bases) = project(&root, "settings.local.json");
        if sources.iter().any(|s| s.path == path) {
            continue;
        }
        let real = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        if let Some(reason) = untrusted_root(&real, home.as_deref(), owner) {
            if std::fs::symlink_metadata(&path).is_ok() {
                out.skipped.push(SkippedFile {
                    path: path.display().to_string(),
                    reason,
                });
            }
            continue;
        }
        sources.push(Source {
            path,
            bases,
            guarded_root: Some(real),
        });
    }

    for Source {
        path,
        bases,
        guarded_root,
    } in sources
    {
        let shown = path.display().to_string();
        let outcome = match &guarded_root {
            None => read(&path),
            Some(root) => read_guarded(root, &path, owner),
        }
        .map_err(|why| unreadable(&shown, &why))?;
        let bytes = match outcome {
            Got::Missing => continue,
            Got::Skipped(reason) => {
                out.skipped.push(SkippedFile {
                    path: shown,
                    reason,
                });
                continue;
            }
            Got::Content(bytes) => bytes,
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
            for written in expanded(rule, &bases) {
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

/// What reading a settings file gave.
enum Got {
    /// The file does not exist.
    Missing,
    /// The file's content.
    Content(Vec<u8>),
    /// The file exists and must not be used, for this reason.
    Skipped(String),
}

/// The content of a regular file of at most [`SETTINGS_FILE_BYTES`]; a missing
/// file is [`Got::Missing`]. A symbolic link is followed. A device, pipe or
/// directory is refused before it is opened, so reading cannot block or run
/// without end.
fn read(path: &Path) -> Result<Got, String> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(Got::Missing);
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
    read_limited(file).map(Got::Content)
}

/// The bytes of an open file, at most [`SETTINGS_FILE_BYTES`].
fn read_limited(file: std::fs::File) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    file.take(SETTINGS_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("it cannot be read ({})", e.kind()))?;
    if bytes.len() > SETTINGS_FILE_BYTES {
        return Err(format!(
            "it is larger than the limit of {SETTINGS_FILE_BYTES} bytes"
        ));
    }
    Ok(bytes)
}

const SYMLINK_REASON: &str = "a symbolic link is not followed in a repository root";

/// Open `name` in the directory `dir` (a handle) without following a link.
/// `Ok(None)` when it does not exist or is not a directory; `Err` with the reason
/// when it is a symbolic link.
fn open_below(
    dir: &std::fs::File,
    name: &str,
    flags: OFlags,
) -> Result<Option<std::fs::File>, String> {
    match rustix::fs::openat(
        dir,
        name,
        flags | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => Ok(Some(std::fs::File::from(fd))),
        Err(Errno::NOENT) => Ok(None),
        Err(e @ (Errno::LOOP | Errno::NOTDIR)) => {
            // A link, or a plain file where a directory was wanted.
            match rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(st) if FileType::from_raw_mode(st.st_mode) == FileType::Symlink => {
                    Err(format!("`{name}` is a symbolic link: {SYMLINK_REASON}"))
                }
                Ok(_) if e == Errno::NOTDIR => Ok(None),
                _ => Err(format!(
                    "`{name}` cannot be opened ({})",
                    std::io::Error::from(e).kind()
                )),
            }
        }
        Err(e) => Err(format!(
            "`{name}` cannot be opened ({})",
            std::io::Error::from(e).kind()
        )),
    }
}

/// Read the settings file `file` (`<root>/.claude/<name>`) of a repository
/// `root` (a real path) without following a link and only when the current user
/// owns the root, `.claude` and the file, judged on the open handles: each
/// directory is opened from the one above, so nothing swapped in after a check
/// can be read. A missing file is [`Got::Missing`] whoever owns the directories.
fn read_guarded(root: &Path, file: &Path, owner: Owner<'_>) -> Result<Got, String> {
    let Some(name) = file.file_name().and_then(OsStr::to_str) else {
        return Ok(Got::Missing);
    };
    let skipped = |why: String| Ok(Got::Skipped(why));
    let root_dir = match rustix::fs::open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => std::fs::File::from(fd),
        Err(Errno::NOENT | Errno::NOTDIR) => return Ok(Got::Missing),
        Err(e) => {
            return Err(format!(
                "its repository root cannot be opened ({})",
                std::io::Error::from(e).kind()
            ));
        }
    };
    let claude = match open_below(&root_dir, ".claude", OFlags::RDONLY | OFlags::DIRECTORY) {
        Ok(Some(dir)) => dir,
        Ok(None) => return Ok(Got::Missing),
        Err(why) => return skipped(why),
    };
    let handle = match open_below(&claude, name, OFlags::RDONLY | OFlags::NONBLOCK) {
        Ok(Some(handle)) => handle,
        Ok(None) => return Ok(Got::Missing),
        Err(why) => return skipped(why),
    };
    let uid_of = |f: &std::fs::File| {
        f.metadata()
            .map(|m| m.uid())
            .map_err(|e| format!("it cannot be inspected ({})", e.kind()))
    };
    let dot_claude = root.join(".claude");
    if !owner.owns(root, uid_of(&root_dir)?) {
        return skipped("the repository root is not owned by the current user".to_string());
    }
    if !owner.owns(&dot_claude, uid_of(&claude)?) {
        return skipped("the .claude directory is not owned by the current user".to_string());
    }
    let meta = handle
        .metadata()
        .map_err(|e| format!("it cannot be inspected ({})", e.kind()))?;
    if !meta.is_file() {
        return Err("it is not a regular file".to_string());
    }
    if !owner.owns(&dot_claude.join(name), meta.uid()) {
        return skipped("the settings file is not owned by the current user".to_string());
    }
    read_limited(handle).map(Got::Content)
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

/// The rules to pass for a rule written in a settings file, whose one-slash
/// rules mean each directory in `bases`.
///
/// The rule is always passed as written: Claude Code resolves `path` and
/// `./path` against the working directory in every settings source. The one
/// form that loses its meaning inline is a `Read` or `Edit` rule with exactly one
/// leading slash, which names a path from a settings directory, so an absolute
/// rule `//base/path` is added beside it for each base. Adding a rule can only
/// narrow a run. Rules for other tools and rules with `//` or `~/` paths are
/// unchanged.
fn expanded(rule: &str, bases: &[PathBuf]) -> Vec<String> {
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
    if relative.is_empty() {
        return rules;
    }
    for base in bases {
        let Some(base_text) = base.to_str() else {
            continue;
        };
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
        let added = format!("{tool}(//{directory}{relative})");
        if !rules.contains(&added) {
            rules.push(added);
        }
    }
    rules
}

/// The roots of the git repository the working directory is inside: the nearest
/// ancestor holding `.git`, and the main checkout when that `.git` is a file
/// of a linked worktree, with notes for a linked worktree whose main checkout
/// could not be identified. Nothing is run.
fn repository_roots(cwd: &Path, owner: Owner<'_>) -> (Vec<PathBuf>, Vec<String>) {
    let Some(root) = cwd.ancestors().find(|d| d.join(".git").exists()) else {
        return (Vec::new(), Vec::new());
    };
    let mut roots = vec![root.to_path_buf()];
    let mut notices = Vec::new();
    match main_checkout(root, owner) {
        Ok(Some(main)) if main != root => roots.push(main),
        Ok(_) => {}
        Err(why) => notices.push(format!(
            "the main checkout of the git worktree at {} could not be identified, so its local \
             Claude Code settings were not read: {why}",
            root.display()
        )),
    }
    (roots, notices)
}

/// For a linked worktree, whose `.git` is a file reading `gitdir: <main>/.git/
/// worktrees/<name>`, the main checkout `<main>`; `Ok(None)` when `.git` is not a
/// file or does not point into a `worktrees` directory (a submodule, say), and
/// `Err` with the reason when a linked worktree's main checkout cannot be
/// identified.
///
/// The line is followed only when it names a directory directly under a
/// `worktrees` directory of a directory called `.git` (a worktree may itself be
/// named `worktrees`), and when that directory's `gitdir` file names this
/// worktree's `.git` file, so a `.git` file planted in an unpacked archive cannot
/// borrow another checkout's rules. A `.git` that is a symbolic link or a git
/// directory kept elsewhere is not followed. Whether the main checkout may be
/// read is decided where its files are read.
fn main_checkout(root: &Path, owner: Owner<'_>) -> Result<Option<PathBuf>, String> {
    let git = root.join(".git");
    let Ok(meta) = std::fs::symlink_metadata(&git) else {
        return Ok(None);
    };
    if !meta.is_file() {
        return Ok(None);
    }
    if !owner.owns(&git, meta.uid()) {
        return Err("its .git file is not owned by the current user".to_string());
    }
    let text = match read(&git) {
        Ok(Got::Content(text)) => text,
        _ => return Err("its .git file cannot be read".to_string()),
    };
    let text = String::from_utf8(text).map_err(|_| "its .git file is not text".to_string())?;
    let target = text
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("gitdir:"))
        .map(str::trim)
        .ok_or("its .git file has no gitdir line")?;
    let target = std::fs::canonicalize(root.join(target))
        .ok()
        .filter(|t| t.is_dir())
        .ok_or("the directory its .git file names does not exist")?;
    let Some(worktrees) = target.parent() else {
        return Ok(None);
    };
    if worktrees.file_name() != Some(OsStr::new("worktrees")) {
        return Ok(None);
    }
    let common = worktrees.parent().filter(|c| c.is_dir());
    let Some(main) = common
        .filter(|c| c.file_name() == Some(OsStr::new(".git")))
        .and_then(Path::parent)
    else {
        return Err(
            "the repository it belongs to is not a .git directory directly under a checkout (a \
             .git that is a symbolic link, or a git directory kept elsewhere, is not followed)"
                .to_string(),
        );
    };
    // The worktree's directory must name this worktree back.
    let back = match read(&target.join("gitdir")) {
        Ok(Got::Content(text)) => String::from_utf8(text).ok(),
        _ => None,
    };
    let names_this_worktree = back
        .as_deref()
        .and_then(|t| t.lines().next())
        .map(|line| target.join(line.trim()))
        .and_then(|p| std::fs::canonicalize(p).ok())
        .is_some_and(|p| Some(p) == std::fs::canonicalize(&git).ok());
    if !names_this_worktree {
        return Err(
            "the gitdir file in the worktree directory its .git file names does not name this \
             worktree, so it is not a linked worktree of that checkout"
                .to_string(),
        );
    }
    Ok(Some(main.to_path_buf()))
}

/// Why a local settings file in the repository root `root` (a real path) must
/// not be read, before it is opened: the root is the home directory, or its `.git`
/// entry is a symbolic link or not owned by the current user. The root, `.claude`
/// and the file are judged when the file is opened ([`read_guarded`]).
fn untrusted_root(root: &Path, home: Option<&Path>, owner: Owner<'_>) -> Option<String> {
    if home == Some(root) {
        return Some("the repository root is the user's home directory".to_string());
    }
    let git = root.join(".git");
    let meta = std::fs::symlink_metadata(&git).ok()?;
    if meta.file_type().is_symlink() {
        Some(format!(
            "the .git entry of the repository root is a symbolic link: {SYMLINK_REASON}"
        ))
    } else if !owner.owns(&git, meta.uid()) {
        Some("the .git entry of the repository root is not owned by the current user".to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_are_passed_as_written_and_a_one_slash_rule_gains_an_absolute_one() {
        let base = [PathBuf::from("/home/u/.claude")];
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
            assert_eq!(expanded(rule, &base), [rule], "{rule}");
        }
        assert_eq!(
            expanded("Edit(/out/x)", &base),
            ["Edit(/out/x)", "Edit(//home/u/.claude/out/x)"]
        );
        assert_eq!(
            expanded("Read(/a[b]/c)", &[PathBuf::from("/p[q]")]),
            ["Read(/a[b]/c)", "Read(//p\\[q\\]/a[b]/c)"]
        );
        assert_eq!(
            expanded("Read(/x)", &[PathBuf::from("/")]),
            ["Read(/x)", "Read(//x)"]
        );
    }

    #[test]
    fn a_one_slash_rule_gains_one_absolute_rule_per_base() {
        let bases = [PathBuf::from("/work/sub"), PathBuf::from("/work")];
        assert_eq!(
            expanded("Read(/x)", &bases),
            ["Read(/x)", "Read(//work/sub/x)", "Read(//work/x)"]
        );
        let same = [PathBuf::from("/work"), PathBuf::from("/work")];
        assert_eq!(expanded("Read(/x)", &same).len(), 2);
    }
}
