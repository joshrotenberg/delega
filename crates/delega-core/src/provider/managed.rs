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
//! - Claude Code: `managed-settings.json`, every `*.json` file that is not
//!   hidden in `managed-settings.d/`, and `managed-mcp.json`, under
//!   `/Library/Application Support/ClaudeCode` on macOS and `/etc/claude-code`
//!   on Linux; on macOS the managed preferences domain
//!   `com.anthropic.claudecode`.
//! - Codex: `/etc/codex/requirements.toml` and the legacy
//!   `/etc/codex/managed_config.toml` on macOS and Linux; on macOS the managed
//!   preferences domain `com.openai.codex`.
//!
//! A report holds paths and key names. It never holds a value: a file's text is
//! parsed, the names of the keys in the tables below are looked up, and the
//! text is dropped. A file that cannot be read or parsed is reported as present
//! and unreadable, without the error, which could quote the file. Managed
//! preferences are reported as present and are not opened. Settings a provider
//! fetches from its server are not on disk and cannot be seen here.
//!
//! Every path is joined under [`SystemPaths::root`], which is `/` outside
//! tests, so a test never reads the real system locations.

use std::io::Read;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

use crate::contract::request::Provider;

/// Most bytes of a managed file that are read. A larger file is reported as
/// unreadable.
pub const MAX_MANAGED_FILE_BYTES: u64 = 4 * 1024 * 1024;

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
}

/// What kind of managed source a path is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ManagedSourceKind {
    /// Claude Code's `managed-settings.json`.
    SettingsFile,
    /// A `*.json` file in Claude Code's `managed-settings.d/`, or that
    /// directory itself when it cannot be listed.
    SettingsDropIn,
    /// Claude Code's `managed-mcp.json`.
    McpFile,
    /// Codex's `requirements.toml`.
    RequirementsFile,
    /// Codex's legacy `managed_config.toml`.
    ManagedConfigFile,
    /// A macOS managed preferences file of the provider's domain.
    ManagedPreferences,
}

/// What doctor could do with a managed source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ManagedSourceState {
    /// It was read and parsed, and `keys` lists what was found in it.
    Read,
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

/// A managed source that exists on this system.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ManagedSource {
    /// The path of the file or directory.
    pub path: String,
    /// What it is.
    pub kind: ManagedSourceKind,
    /// Whether it was read.
    pub state: ManagedSourceState,
    /// What the presence of the source means for a delega launch, when that
    /// does not depend on a key in it. Fixed text for the kind.
    pub note: Option<String>,
    /// The keys in it that can affect a delega launch, by name, in the order of
    /// the provider's table (Claude Code) or of the names (Codex). Empty when
    /// the source was not read or sets none of them.
    pub keys: Vec<ManagedKey>,
}

/// A Claude Code settings key, as the path to it, and what it does to a launch.
type ClaudeKey = (&'static [&'static str], &'static str);

const MCP_REFUSED: &str =
    "restricts MCP servers, so servers passed with --mcp-config can be refused";
const HOOK_OFF: &str = "delega's command hook, passed with --settings, does not run, so exact \
                        command grants are not checked by it";

/// The managed Claude Code keys that change how what delega passes is
/// honoured. Effects are from the Claude Code settings reference.
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
        &["permissions", "defaultMode"],
        "sets a default permission mode, where delega passes --permission-mode dontAsk",
    ),
    (
        &["permissions", "additionalDirectories"],
        "adds working directories beside the ones passed with --add-dir",
    ),
    (
        &["permissions", "blockReadsOutsideWorkingDirectories"],
        "file tools refuse reads outside the --add-dir directories whatever the request grants",
    ),
    (
        &["permissions", "blockWritesOutsideWorkingDirectories"],
        "file tools refuse writes outside the --add-dir directories whatever the request grants",
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
        "only managed network domains count, so a WebFetch grant passed with --allowedTools can \
         be blocked",
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

/// What specific Codex requirement keys do to a launch.
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

const CODEX_REQUIREMENT_OTHER: &str =
    "a requirement Codex enforces over the -c settings delega passes";

/// What specific keys of Codex's legacy managed configuration do to a launch.
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
];

const CODEX_MANAGED_CONFIG_OTHER: &str = "overrides the same key in the -c settings delega passes; \
                                          --ignore-user-config does not turn it off";

const MCP_FILE_NOTE: &str = "with this file in place Claude Code exits at startup when it is \
                             given --strict-mcp-config, which delega passes on every run";
const CLAUDE_PREFERENCES_NOTE: &str = "can set any managed Claude Code key; doctor does not read \
                                       it";
const CODEX_PREFERENCES_NOTE: &str = "can carry a managed configuration (config_toml_base64) or \
                                      requirements (requirements_toml_base64); doctor does not \
                                      read it";

/// The managed sources of `provider` that exist under `system`, in a fixed
/// order: files, drop-in files by name, then managed preferences by path.
/// Nothing is returned for a source that is absent.
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
                &dir.join("managed-settings.json"),
                ManagedSourceKind::SettingsFile,
            );
            drop_ins(&mut found, &dir.join("managed-settings.d"));
            file_source(
                &mut found,
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
                &dir.join("requirements.toml"),
                ManagedSourceKind::RequirementsFile,
            );
            file_source(
                &mut found,
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

/// Whether something is at `path`, without following a final link.
fn exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// The text of a regular file within the size bound, or `None` for anything
/// else. A pipe or a device is never opened, so nothing here can block.
fn read_text(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_MANAGED_FILE_BYTES {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_MANAGED_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_MANAGED_FILE_BYTES {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    Some(text.strip_prefix('\u{feff}').unwrap_or(&text).to_string())
}

fn file_source(found: &mut Vec<ManagedSource>, path: &Path, kind: ManagedSourceKind) {
    if !exists(path) {
        return;
    }
    let keys = read_text(path).and_then(|text| keys_of(kind, &text));
    let read = keys.is_some();
    found.push(ManagedSource {
        path: path.display().to_string(),
        kind,
        state: if read {
            ManagedSourceState::Read
        } else {
            ManagedSourceState::Unreadable
        },
        note: (kind == ManagedSourceKind::McpFile && read).then(|| MCP_FILE_NOTE.to_string()),
        keys: keys.unwrap_or_default(),
    });
}

/// The relevant key names in `text`, or `None` when it does not parse as the
/// kind's format with a table at the top.
fn keys_of(kind: ManagedSourceKind, text: &str) -> Option<Vec<ManagedKey>> {
    match kind {
        ManagedSourceKind::SettingsFile | ManagedSourceKind::SettingsDropIn => {
            let value: Value = serde_json::from_str(text).ok()?;
            value.as_object()?;
            Some(claude_keys(&value))
        }
        ManagedSourceKind::McpFile => {
            let value: Value = serde_json::from_str(text).ok()?;
            value.as_object()?;
            Some(Vec::new())
        }
        ManagedSourceKind::RequirementsFile => Some(codex_keys(
            text,
            CODEX_REQUIREMENT_KEYS,
            CODEX_REQUIREMENT_OTHER,
        )?),
        ManagedSourceKind::ManagedConfigFile => Some(codex_keys(
            text,
            CODEX_MANAGED_CONFIG_KEYS,
            CODEX_MANAGED_CONFIG_OTHER,
        )?),
        ManagedSourceKind::ManagedPreferences => None,
    }
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

/// Every top-level key of a Codex file, by name. A `toml::Table` iterates in
/// the order of the names.
fn codex_keys(text: &str, known: &[(&str, &str)], other: &str) -> Option<Vec<ManagedKey>> {
    let table: toml::Table = text.parse().ok()?;
    let mut names: Vec<&String> = table.keys().collect();
    names.sort();
    Some(
        names
            .into_iter()
            .map(|name| ManagedKey {
                name: name.clone(),
                effect: known
                    .iter()
                    .find(|(key, _)| key == name)
                    .map_or(other, |(_, effect)| effect)
                    .to_string(),
            })
            .collect(),
    )
}

/// The `*.json` files of the drop-in directory that are not hidden, by name,
/// as Claude Code reads them. A directory that is there and cannot be listed
/// is one unreadable source.
fn drop_ins(found: &mut Vec<ManagedSource>, dir: &Path) {
    if !exists(dir) {
        return;
    }
    let Some(mut names) = std::fs::read_dir(dir).ok().and_then(|entries| {
        entries
            .map(|entry| entry.map(|e| e.file_name()))
            .collect::<Result<Vec<_>, _>>()
            .ok()
    }) else {
        found.push(ManagedSource {
            path: dir.display().to_string(),
            kind: ManagedSourceKind::SettingsDropIn,
            state: ManagedSourceState::Unreadable,
            note: None,
            keys: Vec::new(),
        });
        return;
    };
    names.sort();
    for name in names {
        let text = name.to_string_lossy();
        if text.starts_with('.') || !text.ends_with(".json") {
            continue;
        }
        file_source(found, &dir.join(&name), ManagedSourceKind::SettingsDropIn);
    }
}

/// The managed preferences files of a domain on macOS: the one for the whole
/// system and the one in each user's directory. They are not opened.
fn preferences(found: &mut Vec<ManagedSource>, system: &SystemPaths, file: &str, note: &str) {
    if system.platform != Platform::Macos {
        return;
    }
    let dir = system.join("/Library/Managed Preferences");
    let mut paths = vec![dir.join(file)];
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let mut users: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .map(|path| path.join(file))
            .collect();
        users.sort();
        paths.extend(users);
    }
    for path in paths {
        if exists(&path) {
            found.push(ManagedSource {
                path: path.display().to_string(),
                kind: ManagedSourceKind::ManagedPreferences,
                state: ManagedSourceState::NotInspected,
                note: Some(note.to_string()),
                keys: Vec::new(),
            });
        }
    }
}
