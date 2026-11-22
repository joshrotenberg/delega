//! Managed (administrator) settings of the providers, for `delega doctor`.
//!
//! delega does not apply managed settings and cannot switch them off: each
//! provider reads them itself, above everything delega passes on the command
//! line. They can make a grant ignored, an MCP configuration refused or
//! delega's command hook not run, with nothing in a result saying why.
//! [`scan`] looks for the local sources of one provider and names the keys in
//! them that change how a delega launch is honoured.
//!
//! Sources, as each provider documents them:
//!
//! - Claude Code: `managed-settings.json`, the `*.json` files that are not
//!   hidden in `managed-settings.d/`, and `managed-mcp.json`, under
//!   `/Library/Application Support/ClaudeCode` on macOS and `/etc/claude-code`
//!   on Linux; on macOS the managed preferences domain
//!   `com.anthropic.claudecode`.
//! - Codex: `/etc/codex/requirements.toml` and the legacy
//!   `/etc/codex/managed_config.toml` on macOS and Linux; on macOS the managed
//!   preferences domain `com.openai.codex`.
//!
//! **What a report holds.** Fixed paths, counts and key names from the tables
//! below, with fixed text for each. Every string in a report is a constant of
//! this module or a fixed path: nothing is copied from a file or a directory.
//! It never holds a value. It never holds a name read from a directory: a
//! drop-in file or a user's preferences directory can be named anything,
//! including a credential or text with a line break, so the drop-in directory
//! and the per-user preferences are each one source with the directory's fixed
//! path and a count. It never holds a key name read from a file either, for the
//! same reason: a key that is in a table is reported under the table's name,
//! and a top-level key of a Codex file that is in no table is only counted. A
//! file's text is parsed, the names of the keys in the tables are looked up
//! and the text is dropped. No value is read, not even to tell whether a key
//! is switched on: a key is reported because it is present, with what it
//! controls.
//! A file that cannot be read or parsed is reported as present and unreadable,
//! without the error, which could quote the file. Managed preferences are
//! reported as present and are not opened. Settings a provider fetches from its
//! server are not on disk and cannot be seen here.
//!
//! **Reading.** A file is opened without blocking, the opened handle must be a
//! regular file, and at most [`MAX_MANAGED_FILE_BYTES`] are read, so a pipe or
//! a device put where a file belongs cannot hang doctor. A drop-in directory is
//! read up to [`MAX_DROP_IN_FILES`] files, [`MAX_DIRECTORY_ENTRIES`] entries
//! and [`MAX_MANAGED_FILE_BYTES`] in all, counted as the bytes read from disk
//! whether or not they turn out to be text, and the report says when a limit
//! stopped it.
//!
//! **Symbolic links.** Under the real root `/` links are followed, because the
//! providers follow them: a managed file may be a link an administrator's
//! tooling made. Under any other root, which is a test's, nothing outside the
//! root is opened, listed or reported: every path is resolved first and must
//! stay under the resolved root (see [`SystemPaths::inside`]). A source that is
//! a link out of the root is reported as present and unreadable; a source whose
//! directory is outside the root is not reported at all. The check and the
//! open are two steps, not one: a link swapped in between them would be
//! followed. That is accepted, because an injected root exists only in builds
//! with debug assertions, for tests, and no test writes its fixtures while a
//! scan runs.

use std::io::Read;
use std::path::{Path, PathBuf};

use rustix::fs::{Mode, OFlags};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

use crate::contract::request::Provider;

/// Most bytes of one managed file that are read, and most bytes read from one
/// drop-in directory in all. A larger file is reported as unreadable.
pub const MAX_MANAGED_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// Most files of a drop-in directory that are read.
pub const MAX_DROP_IN_FILES: usize = 64;

/// Most entries of one directory that are looked at, whatever they are.
pub const MAX_DIRECTORY_ENTRIES: usize = 4096;

/// The bounds of a scan. [`Limits::default`] is what doctor uses; a test passes
/// smaller ones so that reaching a bound does not take thousands of files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Most bytes of one file that are read, and most bytes read from one
    /// drop-in directory in all.
    pub file_bytes: u64,
    /// Most files of a drop-in directory that are read.
    pub drop_in_files: usize,
    /// Most entries of one directory that are looked at.
    pub directory_entries: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            file_bytes: MAX_MANAGED_FILE_BYTES,
            drop_in_files: MAX_DROP_IN_FILES,
            directory_entries: MAX_DIRECTORY_ENTRIES,
        }
    }
}

/// The layout of system locations to look in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// `/Library/Application Support`, `/Library/Managed Preferences` and
    /// `/etc/codex`.
    Macos,
    /// `/etc/claude-code` and `/etc/codex`.
    Linux,
}

impl Platform {
    /// The layout of the system this was built for. Anything that is not macOS
    /// gets the Linux layout.
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Macos
        } else {
            Self::Linux
        }
    }
}

/// Where the system locations are looked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemPaths {
    /// The directory the absolute system paths are joined under: `/`, or a
    /// temporary directory in a test.
    pub root: PathBuf,
    /// Which layout to look for under it.
    pub platform: Platform,
}

impl Default for SystemPaths {
    fn default() -> Self {
        Self {
            root: PathBuf::from("/"),
            platform: Platform::current(),
        }
    }
}

impl SystemPaths {
    /// The real layout of this system under `root`.
    pub fn under(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            platform: Platform::current(),
        }
    }

    fn join(&self, absolute: &str) -> PathBuf {
        self.root.join(absolute.trim_start_matches('/'))
    }

    /// Whether the root is one a test injected.
    fn injected(&self) -> bool {
        self.root != Path::new("/")
    }

    /// Whether `path` may be opened or listed. Always under the real root.
    /// Under an injected root, only when the path resolves, links followed, to
    /// somewhere under the resolved root, so a link in a fixture cannot lead
    /// the scan to a real system file. A path that does not resolve is not
    /// inside.
    fn inside(&self, path: &Path) -> bool {
        if !self.injected() {
            return true;
        }
        match (
            std::fs::canonicalize(&self.root),
            std::fs::canonicalize(path),
        ) {
            (Ok(root), Ok(target)) => target.starts_with(root),
            _ => false,
        }
    }

    /// What is at `path`, a fixed location: nothing, something that may be
    /// opened, or something that may not (a link out of an injected root, or
    /// one that points nowhere). Under an injected root the directory holding
    /// the path must itself be inside, or the answer is nothing.
    fn presence(&self, path: &Path) -> Presence {
        if self.injected() && !path.parent().is_some_and(|dir| self.inside(dir)) {
            return Presence::Absent;
        }
        if std::fs::symlink_metadata(path).is_err() {
            Presence::Absent
        } else if self.inside(path) {
            Presence::Usable
        } else {
            Presence::Unusable
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Presence {
    Absent,
    Usable,
    Unusable,
}

/// What kind of managed source a path is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ManagedSourceKind {
    /// Claude Code's `managed-settings.json`.
    SettingsFile,
    /// Claude Code's `managed-settings.d/` directory, as a whole: `path` is the
    /// directory and `files` counts the `*.json` files in it.
    SettingsDropIns,
    /// Claude Code's `managed-mcp.json`.
    McpFile,
    /// Codex's `requirements.toml`.
    RequirementsFile,
    /// Codex's legacy `managed_config.toml`.
    ManagedConfigFile,
    /// The macOS managed preferences file of the provider's domain for the
    /// whole system.
    ManagedPreferences,
    /// The macOS managed preferences files of the provider's domain in users'
    /// directories, as a whole: `path` is the directory that holds the users'
    /// directories and `files` counts the files.
    UserManagedPreferences,
}

/// What doctor could do with a managed source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ManagedSourceState {
    /// It was read and parsed, and `keys` lists what was found in it.
    Read,
    /// A directory that was not gone through completely: some of its files
    /// could not be read, or a limit stopped the scan (`files.limit_reached`),
    /// in which case there may be files that were never looked at. `keys`
    /// lists what was found in the files read.
    PartlyRead,
    /// It is there and could not be read or parsed, so nothing is known about
    /// what it sets.
    Unreadable,
    /// It is there and doctor does not open it (managed preferences).
    NotInspected,
}

/// A key in a managed source that can change how a delega launch is honoured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ManagedKey {
    /// The key's name, dotted when it is nested, such as `permissions.deny`.
    /// Never its value.
    pub name: String,
    /// What the key controls that bears on a delega launch. Fixed text for the
    /// key: it does not say whether the key is on, since no value is read.
    pub effect: String,
}

/// The files of a source that is a directory, as counts. No file is named.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ManagedFileCounts {
    /// Files found: `*.json` files that are not hidden in a drop-in directory,
    /// or users' preferences files. Counting stops at a limit.
    pub found: usize,
    /// Of those, how many could not be read or parsed. Always 0 for
    /// preferences, which are not opened.
    pub unreadable: usize,
    /// Whether a limit on files, directory entries or bytes stopped the scan
    /// before the directory was exhausted, so there may be more than `found`
    /// and keys may be missing.
    pub limit_reached: bool,
}

/// A managed source that exists on this system.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ManagedSource {
    /// The path of the file or directory. Always one of the fixed locations,
    /// never a name read from a directory.
    pub path: String,
    /// What it is.
    pub kind: ManagedSourceKind,
    /// Whether it was read.
    pub state: ManagedSourceState,
    /// For a source that is a directory, how many files it holds. Null for a
    /// single file.
    pub files: Option<ManagedFileCounts>,
    /// What the presence of the source means for a delega launch, when that
    /// does not depend on a key in it. Fixed text for the kind.
    pub note: Option<String>,
    /// The keys in it that can affect a delega launch, by name, in the order of
    /// the provider's table (Claude Code) or of the names (Codex). Every name
    /// is one this build knows, never text from the file. For a directory, the
    /// keys of all its files together. Empty when the source was not read or
    /// sets none of them.
    pub keys: Vec<ManagedKey>,
    /// For a Codex file, how many of its top-level keys are in no table of
    /// this build. They are counted and not named: such a key may be a typing
    /// mistake Codex ignores, and its name is whatever the file says. 0 for
    /// every other kind of source.
    pub other_keys: usize,
}

/// A Claude Code settings key, as the path to it, and what it controls.
type ClaudeKey = (&'static [&'static str], &'static str);

/// The managed Claude Code keys that bear on how what delega passes is
/// honoured, each with what it controls. A key is reported when it is present,
/// whatever its value: doctor does not reproduce how Claude Code reads a value
/// (a string where a Boolean belongs, `null`, an empty list, a key that fails
/// closed), so no reason says that a restriction is in force, only what the
/// key decides. What each key controls is from the Claude Code settings
/// reference and its sandboxing page. `permissions.defaultMode` is not here:
/// delega passes `--permission-mode`, which takes precedence over it. Neither
/// is `permissions.disableBypassPermissionsMode`: delega never asks for that
/// mode.
const CLAUDE_KEYS: &[ClaudeKey] = &[
    (
        &["allowManagedPermissionRulesOnly"],
        "controls whether only managed permission rules count; when enabled, the tools granted \
         with --allowedTools and the deny rules passed in --settings are ignored",
    ),
    (
        &["allowManagedHooksOnly"],
        concat!(
            "controls whether only managed hooks run, and so whether ",
            "delega's command hook, passed with --settings, runs; when it does not, exact command ",
            "grants are not checked by it"
        ),
    ),
    (
        &["disableAllHooks"],
        concat!(
            "controls whether hooks are turned off, and so whether ",
            "delega's command hook, passed with --settings, runs; when it does not, exact command ",
            "grants are not checked by it"
        ),
    ),
    (
        &["strictPluginOnlyCustomization"],
        "controls whether hooks and MCP servers that do not come from a plugin are blocked, \
         which would cover the hook in --settings and the servers in --mcp-config",
    ),
    (
        &["disableSideloadFlags"],
        "controls whether Claude Code rejects --mcp-config at startup, which delega passes on \
         every run",
    ),
    (
        &["allowManagedMcpServersOnly"],
        "controls whether only the managed allowlist of MCP servers counts; when enabled, \
         servers passed with --mcp-config can be refused",
    ),
    (
        &["allowedMcpServers"],
        "holds the managed allowlist of MCP servers, which servers passed with --mcp-config are \
         checked against",
    ),
    (
        &["deniedMcpServers"],
        "holds the managed list of blocked MCP servers, which servers passed with --mcp-config \
         are checked against",
    ),
    (
        &["permissions", "allow"],
        "holds managed allow rules, which apply beside the tools granted with --allowedTools, so \
         a worker can be allowed more than the request grants",
    ),
    (
        &["permissions", "deny"],
        "holds managed deny rules, which apply over the tools granted with --allowedTools",
    ),
    (
        &["permissions", "ask"],
        "holds managed ask rules, which apply over the tools granted with --allowedTools; a call \
         that would ask is denied under --permission-mode dontAsk",
    ),
    (
        &["permissions", "additionalDirectories"],
        "holds managed working directories, which are added beside the ones passed with \
         --add-dir, so a worker can reach directories the request did not name",
    ),
    (
        &["permissions", "blockReadsOutsideWorkingDirectories"],
        "controls whether file tools refuse reads outside the working directories (the ones \
         passed with --add-dir and any managed permissions.additionalDirectories), whatever the \
         request grants",
    ),
    (
        &["permissions", "blockWritesOutsideWorkingDirectories"],
        "controls whether file tools refuse writes outside the working directories (the ones \
         passed with --add-dir and any managed permissions.additionalDirectories), whatever the \
         request grants",
    ),
    (
        &["sandbox", "enabled"],
        "controls whether Bash commands run in Claude Code's sandbox, whatever the request grants",
    ),
    (
        &["sandbox", "filesystem", "allowManagedReadPathsOnly"],
        "controls whether only managed sandbox read paths count; when enabled, a sandboxed \
         command may not read a directory passed with --add-dir",
    ),
    (
        &["sandbox", "network", "allowManagedDomainsOnly"],
        "controls whether sandboxed commands reach only the network domains managed settings \
         allow; when enabled, any other domain is blocked without asking",
    ),
    (
        &["forceLoginMethod"],
        "controls which kind of account the worker may be logged in with",
    ),
    (
        &["forceLoginOrgUUID"],
        "controls which organization the worker's login may belong to",
    ),
    (
        &["enforceAnthropicSignIn"],
        "controls whether a claude.ai login is required; when enabled, a worker authenticated \
         another way is refused",
    ),
    (
        &["allowedProviders"],
        "lists the services a worker may reach Claude through; one that uses another is refused \
         at startup",
    ),
    (
        &["apiKeyHelper"],
        "names a command that can supply the worker's credential; environment credentials and \
         cloud provider settings come first, as Claude Code's authentication precedence \
         describes",
    ),
    (
        &["env"],
        "holds environment variables Claude Code sets in the worker, after delega's environment \
         policy has run",
    ),
    (
        &["availableModels"],
        "lists the models a --model passed by delega may name",
    ),
    (
        &["deniedModels"],
        "lists models a --model passed by delega may not name",
    ),
    (
        &["modelOverrides"],
        "maps model IDs to provider model IDs, so a model named with --model can be sent to a \
         different one",
    ),
    (
        &["requiredMinimumVersion"],
        "sets the oldest Claude Code version that may start",
    ),
    (
        &["requiredMaximumVersion"],
        "sets the newest Claude Code version that may start",
    ),
    (
        &["policyHelper"],
        "names a command that computes managed settings at startup; doctor cannot see what it \
         returns",
    ),
];

/// What the keys of `requirements.toml` that bear on something delega passes
/// do to a launch.
const CODEX_REQUIREMENT_KEYS: &[(&str, &str)] = &[
    (
        "allowed_approval_policies",
        "limits the approval policies Codex accepts, where delega passes -c \
         approval_policy=\"never\"",
    ),
    (
        "allowed_sandbox_modes",
        "limits the sandbox modes Codex accepts, where delega passes -c sandbox_mode for the \
         request's permissions",
    ),
    (
        "allowed_permission_profiles",
        "limits the permission profiles Codex accepts, which constrains the sandbox delega asks \
         for with -c sandbox_mode",
    ),
    (
        "default_permissions",
        "sets the permission profile Codex starts from, beside the -c sandbox_mode delega passes",
    ),
    (
        "mcp_servers",
        "limits the MCP servers Codex starts, beside the -c mcp_servers setting delega passes \
         to disable the configured ones",
    ),
    (
        "allow_login_shell",
        "constrains login shells, where delega passes -c allow_login_shell=false",
    ),
];

/// The other top-level keys of `requirements.toml` Codex reads, from
/// `ConfigRequirementsToml` in `codex-rs/config/src/config_requirements.rs`,
/// with the aliases it accepts.
const CODEX_REQUIREMENT_NAMES: &[&str] = &[
    "config_server",
    "allowed_login_methods",
    "allowed_chatgpt_workspaces",
    "cli_auth_credentials_store",
    "chatgpt_base_url",
    "sqlite_home",
    "unsafe_enforce_sqlite_journal_mode",
    "log_dir",
    "model_catalog_json",
    "model_provider",
    "model_providers",
    "check_for_update_on_startup",
    "feedback",
    "allowed_approvals_reviewers",
    "remote_sandbox_config",
    "allowed_web_search_modes",
    "allow_managed_hooks_only",
    "allow_browser_and_computer_use",
    "allow_appshots",
    "allow_remote_control",
    "computer_use",
    "browser_use",
    "in_app_browser",
    "windows",
    "features",
    "feature_requirements",
    "hooks",
    "plugins",
    "marketplaces",
    "otel",
    "apps",
    "rules",
    "enforce_residency",
    "network_proxy",
    "experimental_network",
    "application",
    "permissions",
    "auto_review",
    "models",
    "additional_developer_instructions",
    "guardian_policy_config",
    "guardian_extra_policy",
];

const CODEX_REQUIREMENT_KNOWN: &str =
    "a requirement Codex enforces whatever settings delega passes";

/// What the keys of Codex's legacy managed configuration that delega also
/// passes, or that Codex turns into requirements, do to a launch.
const CODEX_MANAGED_CONFIG_KEYS: &[(&str, &str)] = &[
    (
        "approval_policy",
        "replaces the -c approval_policy=\"never\" delega passes and is enforced as a requirement",
    ),
    (
        "sandbox_mode",
        "replaces the -c sandbox_mode delega passes and is enforced as a requirement",
    ),
    (
        "approvals_reviewer",
        "is enforced as a requirement on who reviews approvals",
    ),
    (
        "allow_login_shell",
        "replaces the -c allow_login_shell=false delega passes",
    ),
    (
        "sandbox_workspace_write",
        "replaces keys of the -c sandbox_workspace_write settings delega passes for a write run",
    ),
    (
        "developer_instructions",
        "replaces the -c developer_instructions delega passes",
    ),
    (
        "model_reasoning_effort",
        "replaces the -c model_reasoning_effort delega passes for a request's effort",
    ),
    (
        "mcp_servers",
        "is merged over the -c mcp_servers setting delega passes to disable the configured servers",
    ),
];

const MCP_FILE_NOTE: &str = "with this file in place Claude Code exits at startup when it is \
                             given --strict-mcp-config, which delega passes on every run";
const CLAUDE_PREFERENCES_NOTE: &str = "can set any managed Claude Code key; doctor does not read \
                                       it";
const CODEX_PREFERENCES_NOTE: &str = "can carry a managed configuration (config_toml_base64) or \
                                      requirements (requirements_toml_base64); doctor does not \
                                      read it";
const USER_PREFERENCES_NOTE: &str = "managed preferences of the provider in users' directories; \
                                     only the current user's apply, and doctor does not read them";

/// The managed sources of `provider` that exist under `system`, in a fixed
/// order: files, the drop-in directory, then managed preferences. Nothing is
/// returned for a source that is absent.
pub fn scan(provider: Provider, system: &SystemPaths) -> Vec<ManagedSource> {
    scan_with(provider, system, &Limits::default())
}

/// [`scan`] with the bounds given.
pub fn scan_with(provider: Provider, system: &SystemPaths, limits: &Limits) -> Vec<ManagedSource> {
    let mut found = Vec::new();
    match provider {
        Provider::Claude => {
            let dir = system.join(match system.platform {
                Platform::Macos => "/Library/Application Support/ClaudeCode",
                Platform::Linux => "/etc/claude-code",
            });
            file_source(
                &mut found,
                system,
                limits,
                &dir.join("managed-settings.json"),
                ManagedSourceKind::SettingsFile,
            );
            drop_ins(&mut found, system, limits, &dir.join("managed-settings.d"));
            file_source(
                &mut found,
                system,
                limits,
                &dir.join("managed-mcp.json"),
                ManagedSourceKind::McpFile,
            );
            preferences(
                &mut found,
                system,
                limits,
                "com.anthropic.claudecode.plist",
                CLAUDE_PREFERENCES_NOTE,
            );
        }
        Provider::Codex => {
            let dir = system.join("/etc/codex");
            file_source(
                &mut found,
                system,
                limits,
                &dir.join("requirements.toml"),
                ManagedSourceKind::RequirementsFile,
            );
            file_source(
                &mut found,
                system,
                limits,
                &dir.join("managed_config.toml"),
                ManagedSourceKind::ManagedConfigFile,
            );
            preferences(
                &mut found,
                system,
                limits,
                "com.openai.codex.plist",
                CODEX_PREFERENCES_NOTE,
            );
        }
    }
    found
}

/// The bytes of the regular file at `path`, when it holds at most `limit`.
/// `charged` gets the number of bytes read from the file, whatever comes of
/// them, so a caller with a total to keep to can count them. It is never more
/// than `limit`: no byte past the limit is read to find out that there is one.
///
/// The file is opened without blocking and the opened handle is judged, not
/// the path: a pipe, a device or a directory swapped in after any earlier look
/// is refused here, and opening a pipe with no writer returns at once. Links
/// are followed; the caller has decided that this path may be opened.
fn read_bytes(path: &Path, limit: u64, charged: &mut u64) -> Result<Vec<u8>, Unread> {
    let handle = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC | OFlags::NOCTTY,
        Mode::empty(),
    )
    .map_err(|_| Unread::Unusable)?;
    let file = std::fs::File::from(handle);
    let meta = file.metadata().map_err(|_| Unread::Unusable)?;
    if !meta.is_file() {
        return Err(Unread::Unusable);
    }
    if meta.len() > limit {
        return Err(Unread::OverLimit);
    }
    let bytes = read_at_most(&file, limit, charged).map_err(|_| Unread::Unusable)?;
    // The file may have grown since the look above. When the read stopped at
    // the limit, the size of the open file says whether there is more.
    let size_now = file.metadata().map_or(u64::MAX, |meta| meta.len());
    if bytes.len() as u64 == limit && size_now > limit {
        return Err(Unread::OverLimit);
    }
    Ok(bytes)
}

/// Up to `limit` bytes from `source`, and never one more, whatever `source`
/// goes on to hold. `charged` gets the number read, also when reading fails.
pub(crate) fn read_at_most(
    source: impl Read,
    limit: u64,
    charged: &mut u64,
) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let read = source.take(limit).read_to_end(&mut bytes);
    *charged = bytes.len() as u64;
    read.map(|_| bytes)
}

/// The text in `bytes`, without a byte order mark, when they are UTF-8.
fn decode(bytes: Vec<u8>) -> Option<String> {
    let text = String::from_utf8(bytes).ok()?;
    Some(text.strip_prefix('\u{feff}').unwrap_or(&text).to_string())
}

/// Why a file gave no bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unread {
    /// It is a regular file with more bytes than the limit given.
    OverLimit,
    /// It could not be opened or read, or is not a regular file.
    Unusable,
}

/// What a file's text holds: the keys of a table, and how many others.
#[derive(Debug, Default)]
struct Found {
    keys: Vec<ManagedKey>,
    other: usize,
}

fn file_source(
    found: &mut Vec<ManagedSource>,
    system: &SystemPaths,
    limits: &Limits,
    path: &Path,
    kind: ManagedSourceKind,
) {
    let keys = match system.presence(path) {
        Presence::Absent => return,
        Presence::Unusable => None,
        Presence::Usable => read_bytes(path, limits.file_bytes, &mut 0)
            .ok()
            .and_then(decode)
            .and_then(|text| keys_of(kind, &text)),
    };
    let read = keys.is_some();
    let keys = keys.unwrap_or_default();
    found.push(ManagedSource {
        path: path.display().to_string(),
        kind,
        state: if read {
            ManagedSourceState::Read
        } else {
            ManagedSourceState::Unreadable
        },
        files: None,
        note: (kind == ManagedSourceKind::McpFile && read).then(|| MCP_FILE_NOTE.to_string()),
        keys: keys.keys,
        other_keys: keys.other,
    });
}

/// What `text` holds of the kind's tables, or `None` when it does not parse
/// as the kind's format with a table at the top.
fn keys_of(kind: ManagedSourceKind, text: &str) -> Option<Found> {
    match kind {
        ManagedSourceKind::SettingsFile | ManagedSourceKind::SettingsDropIns => {
            // Claude Code counts an empty managed settings file as `{}`.
            if text.is_empty() {
                return Some(Found::default());
            }
            let value: Value = serde_json::from_str(text).ok()?;
            value.as_object()?;
            Some(Found {
                keys: claude_keys(&value),
                other: 0,
            })
        }
        ManagedSourceKind::McpFile => {
            let value: Value = serde_json::from_str(text).ok()?;
            value.as_object()?;
            Some(Found::default())
        }
        ManagedSourceKind::RequirementsFile => {
            let general = CODEX_REQUIREMENT_NAMES
                .iter()
                .map(|name| (*name, CODEX_REQUIREMENT_KNOWN));
            codex_keys(text, CODEX_REQUIREMENT_KEYS.iter().copied().chain(general))
        }
        ManagedSourceKind::ManagedConfigFile => {
            codex_keys(text, CODEX_MANAGED_CONFIG_KEYS.iter().copied())
        }
        ManagedSourceKind::ManagedPreferences | ManagedSourceKind::UserManagedPreferences => None,
    }
}

fn claude_keys(settings: &Value) -> Vec<ManagedKey> {
    CLAUDE_KEYS
        .iter()
        .filter(|(path, _)| {
            // Present is enough: the value at the end of the path is not read.
            path.iter()
                .try_fold(settings, |value, part| value.as_object()?.get(*part))
                .is_some()
        })
        .map(|(path, effect)| ManagedKey {
            name: path.join("."),
            effect: (*effect).to_string(),
        })
        .collect()
}

/// The top-level keys of a Codex file that are in `table`, under the table's
/// own names and in the order of the names, and the number of the others. A
/// key that is in no table is never named: its name is whatever the file says,
/// which can be a credential or text with a line break, and it may be a typing
/// mistake Codex ignores.
fn codex_keys(
    text: &str,
    table: impl Iterator<Item = (&'static str, &'static str)>,
) -> Option<Found> {
    let parsed: toml::Table = text.parse().ok()?;
    let mut keys: Vec<ManagedKey> = table
        .filter(|(name, _)| parsed.contains_key(*name))
        .map(|(name, effect)| ManagedKey {
            name: name.to_string(),
            effect: effect.to_string(),
        })
        .collect();
    keys.sort_by(|a, b| a.name.cmp(&b.name));
    let other = parsed.len() - keys.len();
    Some(Found { keys, other })
}

/// The drop-in directory as one source: how many `*.json` files that are not
/// hidden it holds, as Claude Code reads them, and the keys of those files
/// together in the table's order. No file is named. A directory that is there
/// and cannot be listed is unreadable.
fn drop_ins(found: &mut Vec<ManagedSource>, system: &SystemPaths, limits: &Limits, dir: &Path) {
    let kind = ManagedSourceKind::SettingsDropIns;
    let entries = match system.presence(dir) {
        Presence::Absent => return,
        Presence::Unusable => None,
        Presence::Usable => std::fs::read_dir(dir).ok(),
    };
    let mut source = ManagedSource {
        path: dir.display().to_string(),
        kind,
        state: ManagedSourceState::Unreadable,
        files: None,
        note: None,
        keys: Vec::new(),
        other_keys: 0,
    };
    let Some(entries) = entries else {
        found.push(source);
        return;
    };
    let mut counts = ManagedFileCounts {
        found: 0,
        unreadable: 0,
        limit_reached: false,
    };
    let mut budget = limits.file_bytes;
    let mut names: Vec<String> = Vec::new();
    for (seen, entry) in entries.enumerate() {
        if seen == limits.directory_entries {
            counts.limit_reached = true;
            break;
        }
        let Ok(entry) = entry else {
            counts.limit_reached = true;
            break;
        };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || !name.ends_with(".json") {
            continue;
        }
        if counts.found == limits.drop_in_files {
            counts.limit_reached = true;
            break;
        }
        counts.found += 1;
        let path = entry.path();
        // Never more than what is left of the total is read, counted as the
        // bytes that came off the disk. Once the total is spent no further
        // file is opened. A file left out this way is counted with the
        // unreadable ones, since its keys are not known, and the report says
        // a limit was reached.
        if budget == 0 || !system.inside(&path) {
            counts.unreadable += 1;
            counts.limit_reached |= budget == 0;
            continue;
        }
        let mut charged = 0;
        let bytes = read_bytes(&path, budget, &mut charged);
        let within_total = budget;
        budget = budget.saturating_sub(charged);
        let found_keys = match bytes {
            Ok(bytes) => decode(bytes).and_then(|text| keys_of(kind, &text)),
            Err(reason) => {
                counts.limit_reached |=
                    reason == Unread::OverLimit && within_total < limits.file_bytes;
                None
            }
        };
        match found_keys {
            Some(keys) => names.extend(keys.keys.into_iter().map(|key| key.name)),
            None => counts.unreadable += 1,
        }
    }
    // Nothing but other files: the directory is not a source.
    if counts.found == 0 && !counts.limit_reached {
        return;
    }
    source.keys = CLAUDE_KEYS
        .iter()
        .map(|(path, effect)| (path.join("."), *effect))
        .filter(|(name, _)| names.contains(name))
        .map(|(name, effect)| ManagedKey {
            name,
            effect: effect.to_string(),
        })
        .collect();
    // A limit means there may be files that were never looked at, so the
    // directory is partly read whatever became of the ones that were.
    source.state = if counts.limit_reached {
        ManagedSourceState::PartlyRead
    } else if counts.unreadable == 0 {
        ManagedSourceState::Read
    } else if counts.unreadable == counts.found {
        ManagedSourceState::Unreadable
    } else {
        ManagedSourceState::PartlyRead
    };
    source.files = Some(counts);
    found.push(source);
}

/// The managed preferences of a domain on macOS: the file for the whole
/// system, and, as one source, the files in users' directories. None is
/// opened, and no user's directory is named.
fn preferences(
    found: &mut Vec<ManagedSource>,
    system: &SystemPaths,
    limits: &Limits,
    file: &str,
    note: &str,
) {
    if system.platform != Platform::Macos {
        return;
    }
    let dir = system.join("/Library/Managed Preferences");
    let whole = dir.join(file);
    if system.presence(&whole) != Presence::Absent {
        found.push(ManagedSource {
            path: whole.display().to_string(),
            kind: ManagedSourceKind::ManagedPreferences,
            state: ManagedSourceState::NotInspected,
            files: None,
            note: Some(note.to_string()),
            keys: Vec::new(),
            other_keys: 0,
        });
    }
    if !system.inside(&dir) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let mut counts = ManagedFileCounts {
        found: 0,
        unreadable: 0,
        limit_reached: false,
    };
    for (seen, entry) in entries.enumerate() {
        if seen == limits.directory_entries {
            counts.limit_reached = true;
            break;
        }
        let Ok(entry) = entry else {
            counts.limit_reached = true;
            break;
        };
        let user = entry.path();
        if system.inside(&user)
            && user.is_dir()
            && system.presence(&user.join(file)) != Presence::Absent
        {
            counts.found += 1;
        }
    }
    // A limit that stopped the listing is reported even when no user's file
    // was seen before it: there may be one further on.
    if counts.found == 0 && !counts.limit_reached {
        return;
    }
    found.push(ManagedSource {
        path: dir.display().to_string(),
        kind: ManagedSourceKind::UserManagedPreferences,
        state: if counts.limit_reached {
            ManagedSourceState::PartlyRead
        } else {
            ManagedSourceState::NotInspected
        },
        files: Some(counts),
        note: Some(USER_PREFERENCES_NOTE.to_string()),
        keys: Vec::new(),
        other_keys: 0,
    });
}
