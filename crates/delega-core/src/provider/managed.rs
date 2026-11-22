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
//! below, with fixed text for each. It never holds a value, and it never holds
//! a name read from a directory: a drop-in file or a user's preferences
//! directory can be named anything, including a credential or text with a line
//! break, so the drop-in directory and the per-user preferences are each one
//! source with the directory's fixed path and a count. A file's text is parsed,
//! the names of the keys in the tables are looked up, and the text is dropped.
//! A file that cannot be read or parsed is reported as present and unreadable,
//! without the error, which could quote the file. Managed preferences are
//! reported as present and are not opened. Settings a provider fetches from its
//! server are not on disk and cannot be seen here.
//!
//! **Reading.** A file is opened without blocking, the opened handle must be a
//! regular file, and at most [`MAX_MANAGED_FILE_BYTES`] are read, so a pipe or
//! a device put where a file belongs cannot hang doctor. A drop-in directory is
//! read up to [`MAX_DROP_IN_FILES`] files, [`MAX_DIRECTORY_ENTRIES`] entries
//! and [`MAX_MANAGED_FILE_BYTES`] in all, and the report says when a limit
//! stopped it.
//!
//! **Symbolic links.** Under the real root `/` links are followed, because the
//! providers follow them: a managed file may be a link an administrator's
//! tooling made. Under any other root, which is a test's, nothing outside the
//! root is opened, listed or reported: every path is resolved first and must
//! stay under the resolved root (see [`SystemPaths::inside`]). A source that is
//! a link out of the root is reported as present and unreadable; a source whose
//! directory is outside the root is not reported at all.

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
    /// A directory of which some files were read and some could not be, or
    /// were left out by a limit. `keys` lists what was found in those read, and
    /// `files` says how many were not.
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
    /// What the key can do to a delega launch. Fixed text for the key.
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
    /// the provider's table (Claude Code) or of the names (Codex). For a
    /// directory, the keys of all its files together. Empty when the source was
    /// not read or sets none of them.
    pub keys: Vec<ManagedKey>,
}

/// A Claude Code settings key, as the path to it, and what it does to a launch.
type ClaudeKey = (&'static [&'static str], &'static str);

const MCP_REFUSED: &str =
    "restricts MCP servers, so servers passed with --mcp-config can be refused";
const HOOK_OFF: &str = "delega's command hook, passed with --settings, does not run, so exact \
                        command grants are not checked by it";

/// The managed Claude Code keys that change how what delega passes is
/// honoured. Effects are from the Claude Code settings reference and its
/// sandboxing page. `permissions.defaultMode` is not here: delega passes
/// `--permission-mode`, which takes precedence over it. Neither is
/// `permissions.disableBypassPermissionsMode`: delega never asks for that mode.
const CLAUDE_KEYS: &[ClaudeKey] = &[
    (
        &["allowManagedPermissionRulesOnly"],
        "only managed permission rules count: the tools granted with --allowedTools and the deny \
         rules passed in --settings are ignored",
    ),
    (&["allowManagedHooksOnly"], HOOK_OFF),
    (&["disableAllHooks"], HOOK_OFF),
    (
        &["strictPluginOnlyCustomization"],
        "can block hooks and MCP servers that do not come from a plugin, which covers the hook in \
         --settings and the servers in --mcp-config",
    ),
    (
        &["disableSideloadFlags"],
        "Claude Code rejects --mcp-config at startup, which delega passes on every run",
    ),
    (&["allowManagedMcpServersOnly"], MCP_REFUSED),
    (&["allowedMcpServers"], MCP_REFUSED),
    (&["deniedMcpServers"], MCP_REFUSED),
    (
        &["permissions", "allow"],
        "managed allow rules apply beside the tools granted with --allowedTools, so a worker can \
         be allowed more than the request grants",
    ),
    (
        &["permissions", "deny"],
        "managed deny rules apply over the tools granted with --allowedTools",
    ),
    (
        &["permissions", "ask"],
        "managed ask rules apply over the tools granted with --allowedTools, and a call that \
         would ask is denied under --permission-mode dontAsk",
    ),
    (
        &["permissions", "additionalDirectories"],
        "adds working directories beside the ones passed with --add-dir, so a worker can reach \
         directories the request did not name",
    ),
    (
        &["permissions", "blockReadsOutsideWorkingDirectories"],
        "file tools refuse reads outside the working directories (the ones passed with --add-dir \
         and any managed permissions.additionalDirectories) whatever the request grants",
    ),
    (
        &["permissions", "blockWritesOutsideWorkingDirectories"],
        "file tools refuse writes outside the working directories (the ones passed with --add-dir \
         and any managed permissions.additionalDirectories) whatever the request grants",
    ),
    (
        &["sandbox", "enabled"],
        "Bash commands run in Claude Code's sandbox whatever the request grants",
    ),
    (
        &["sandbox", "filesystem", "allowManagedReadPathsOnly"],
        "only managed sandbox read paths count, so a sandboxed command may not read a directory \
         passed with --add-dir",
    ),
    (
        &["sandbox", "network", "allowManagedDomainsOnly"],
        "sandboxed commands reach only the network domains managed settings allow, and any other \
         domain is blocked without asking, whatever the request grants",
    ),
    (
        &["forceLoginMethod"],
        "restricts which kind of account the worker may be logged in with",
    ),
    (
        &["forceLoginOrgUUID"],
        "restricts which organization the worker's login may belong to",
    ),
    (
        &["enforceAnthropicSignIn"],
        "requires a claude.ai login, so a worker authenticated another way is refused",
    ),
    (
        &["allowedProviders"],
        "a worker that reaches Claude through a service not listed is refused at startup",
    ),
    (
        &["apiKeyHelper"],
        "the worker's credential comes from a managed command, not from the environment delega \
         passes",
    ),
    (
        &["env"],
        "sets environment variables in the worker, after delega's environment policy has run",
    ),
    (
        &["availableModels"],
        "limits the models a --model passed by delega may name",
    ),
    (
        &["deniedModels"],
        "blocks models a --model passed by delega may name",
    ),
    (
        &["modelOverrides"],
        "maps model IDs to provider model IDs, so a model named with --model can be sent to a \
         different one",
    ),
    (
        &["requiredMinimumVersion"],
        "Claude Code refuses to start when it is older than this",
    ),
    (
        &["requiredMaximumVersion"],
        "Claude Code refuses to start when it is newer than this",
    ),
    (
        &["policyHelper"],
        "managed settings are computed by a command at startup, and doctor cannot see what it \
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
const CODEX_REQUIREMENT_UNKNOWN: &str =
    "not a requirements key this version of doctor knows; Codex may ignore it";

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

const CODEX_MANAGED_CONFIG_OTHER: &str = "not a setting delega passes; a key Codex knows applies \
                                          whatever --ignore-user-config says, and one it does not \
                                          know is ignored";

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
                &dir.join("managed-settings.json"),
                ManagedSourceKind::SettingsFile,
            );
            drop_ins(&mut found, system, &dir.join("managed-settings.d"));
            file_source(
                &mut found,
                system,
                &dir.join("managed-mcp.json"),
                ManagedSourceKind::McpFile,
            );
            preferences(
                &mut found,
                system,
                "com.anthropic.claudecode.plist",
                CLAUDE_PREFERENCES_NOTE,
            );
        }
        Provider::Codex => {
            let dir = system.join("/etc/codex");
            file_source(
                &mut found,
                system,
                &dir.join("requirements.toml"),
                ManagedSourceKind::RequirementsFile,
            );
            file_source(
                &mut found,
                system,
                &dir.join("managed_config.toml"),
                ManagedSourceKind::ManagedConfigFile,
            );
            preferences(
                &mut found,
                system,
                "com.openai.codex.plist",
                CODEX_PREFERENCES_NOTE,
            );
        }
    }
    found
}

/// The text of the regular file at `path`, when it is at most `limit` bytes.
///
/// The file is opened without blocking and the opened handle is judged, not
/// the path: a pipe, a device or a directory swapped in after any earlier look
/// is refused here, and opening a pipe with no writer returns at once. Links
/// are followed; the caller has decided that this path may be opened.
fn read_text(path: &Path, limit: u64) -> Result<String, Unread> {
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
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Unread::Unusable)?;
    if bytes.len() as u64 > limit {
        return Err(Unread::OverLimit);
    }
    let text = String::from_utf8(bytes).map_err(|_| Unread::Unusable)?;
    Ok(text.strip_prefix('\u{feff}').unwrap_or(&text).to_string())
}

/// Why a file gave no text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unread {
    /// It is a regular file with more bytes than the limit given.
    OverLimit,
    /// It could not be opened or read, is not a regular file or is not UTF-8.
    Unusable,
}

fn file_source(
    found: &mut Vec<ManagedSource>,
    system: &SystemPaths,
    path: &Path,
    kind: ManagedSourceKind,
) {
    let keys = match system.presence(path) {
        Presence::Absent => return,
        Presence::Unusable => None,
        Presence::Usable => read_text(path, MAX_MANAGED_FILE_BYTES)
            .ok()
            .and_then(|text| keys_of(kind, &text)),
    };
    let read = keys.is_some();
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
        keys: keys.unwrap_or_default(),
    });
}

/// The relevant key names in `text`, or `None` when it does not parse as the
/// kind's format with a table at the top.
fn keys_of(kind: ManagedSourceKind, text: &str) -> Option<Vec<ManagedKey>> {
    match kind {
        ManagedSourceKind::SettingsFile | ManagedSourceKind::SettingsDropIns => {
            // Claude Code counts an empty managed settings file as `{}`.
            if text.is_empty() {
                return Some(Vec::new());
            }
            let value: Value = serde_json::from_str(text).ok()?;
            value.as_object()?;
            Some(claude_keys(&value))
        }
        ManagedSourceKind::McpFile => {
            let value: Value = serde_json::from_str(text).ok()?;
            value.as_object()?;
            Some(Vec::new())
        }
        ManagedSourceKind::RequirementsFile => codex_keys(text, |name| {
            specific(CODEX_REQUIREMENT_KEYS, name).unwrap_or(
                if CODEX_REQUIREMENT_NAMES.contains(&name) {
                    CODEX_REQUIREMENT_KNOWN
                } else {
                    CODEX_REQUIREMENT_UNKNOWN
                },
            )
        }),
        ManagedSourceKind::ManagedConfigFile => codex_keys(text, |name| {
            specific(CODEX_MANAGED_CONFIG_KEYS, name).unwrap_or(CODEX_MANAGED_CONFIG_OTHER)
        }),
        ManagedSourceKind::ManagedPreferences | ManagedSourceKind::UserManagedPreferences => None,
    }
}

fn specific(table: &[(&str, &'static str)], name: &str) -> Option<&'static str> {
    table
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, effect)| *effect)
}

fn claude_keys(settings: &Value) -> Vec<ManagedKey> {
    CLAUDE_KEYS
        .iter()
        .filter(|(path, _)| {
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

/// Every top-level key of a Codex file, by name, with the effect `effect`
/// gives for it. A key Codex does not know gets a reason that claims nothing:
/// the name may be a typing mistake that Codex ignores.
fn codex_keys(text: &str, effect: impl Fn(&str) -> &'static str) -> Option<Vec<ManagedKey>> {
    let table: toml::Table = text.parse().ok()?;
    let mut names: Vec<&String> = table.keys().collect();
    names.sort();
    Some(
        names
            .into_iter()
            .map(|name| ManagedKey {
                name: name.clone(),
                effect: effect(name).to_string(),
            })
            .collect(),
    )
}

/// The drop-in directory as one source: how many `*.json` files that are not
/// hidden it holds, as Claude Code reads them, and the keys of those files
/// together in the table's order. No file is named. A directory that is there
/// and cannot be listed is unreadable.
fn drop_ins(found: &mut Vec<ManagedSource>, system: &SystemPaths, dir: &Path) {
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
    let mut budget = MAX_MANAGED_FILE_BYTES;
    let mut names: Vec<String> = Vec::new();
    for (seen, entry) in entries.enumerate() {
        if seen == MAX_DIRECTORY_ENTRIES {
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
        if counts.found == MAX_DROP_IN_FILES {
            counts.limit_reached = true;
            break;
        }
        counts.found += 1;
        let path = entry.path();
        // Never more than what is left of the total is read. A file that
        // would fit alone and does not fit in the rest is left out by the
        // limit; it is counted with the unreadable ones, since its keys are
        // not known, and the report says a limit was reached.
        let text = if system.inside(&path) {
            read_text(&path, budget)
        } else {
            Err(Unread::Unusable)
        };
        let text = match text {
            Ok(text) => text,
            Err(reason) => {
                counts.unreadable += 1;
                counts.limit_reached |=
                    reason == Unread::OverLimit && budget < MAX_MANAGED_FILE_BYTES;
                continue;
            }
        };
        budget -= text.len() as u64;
        match keys_of(kind, &text) {
            Some(keys) => names.extend(keys.into_iter().map(|key| key.name)),
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
    source.state = if counts.unreadable == 0 && !counts.limit_reached {
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
fn preferences(found: &mut Vec<ManagedSource>, system: &SystemPaths, file: &str, note: &str) {
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
        if seen == MAX_DIRECTORY_ENTRIES {
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
    if counts.found == 0 {
        return;
    }
    found.push(ManagedSource {
        path: dir.display().to_string(),
        kind: ManagedSourceKind::UserManagedPreferences,
        state: ManagedSourceState::NotInspected,
        files: Some(counts),
        note: Some(USER_PREFERENCES_NOTE.to_string()),
        keys: Vec::new(),
    });
}
