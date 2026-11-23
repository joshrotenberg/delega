//! Managed (administrator) settings for run planning and `delega doctor`.
//!
//! delega does not apply managed settings and cannot switch them off: each
//! provider reads them itself, above everything delega passes on the command
//! line. They can change what a worker gets, with nothing in a result saying
//! why. [`scan`] looks for the local sources of one provider and names the keys
//! present in them that bear on what delega passes at launch.
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
//! below, each with its category. It makes no claim about what a key does on
//! this system: [`CLAUDE_GUIDANCE`] and [`CODEX_GUIDANCE`] say once, for all of
//! a provider's keys, that they can change how the provider treats what delega
//! passes, and point at the provider's own reference. Every string in a report is a constant of
//! this module or a fixed path: nothing is copied from a file or a directory.
//! It never holds a value. It never holds a name read from a directory: a
//! drop-in file or a user's preferences directory can be named anything,
//! including a credential or text with a line break, so the drop-in directory
//! and the per-user preferences are each one source with the directory's fixed
//! path and a count. It never holds a key name read from a file either, for the
//! same reason: a key that is in a table is reported under the table's name,
//! and a top-level key of a Codex file that is in no table is only counted. A
//! file's text is gone through for the names of its keys and dropped. No value
//! is read or even converted: values are skipped, so a number no type here
//! could hold does not hide the keys beside it, and a key is reported because
//! it is present, not because of what it is set to.
//!
//! **Reading.** A file is opened without blocking, the opened handle must be a
//! regular file, and at most [`MAX_MANAGED_FILE_BYTES`] are read, so a pipe or
//! a device put where a file belongs cannot hang doctor. A drop-in directory is
//! read up to [`MAX_DROP_IN_FILES`] files, [`MAX_DIRECTORY_ENTRIES`] entries
//! and [`MAX_MANAGED_FILE_BYTES`] in all, counted as the bytes read from disk
//! whether or not they turn out to be text, and the report says when a limit
//! stopped it. The size of the open file is taken before and after the read: a
//! file that changed size meanwhile, or holds more than was read, is not
//! reported as read but as partly read with the limit marker.
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
use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::Serialize;
use serde::de::IgnoredAny;

use crate::contract::request::Provider;

use super::refusal::Refusal;

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

    /// The location of Claude Code's managed MCP file under this root.
    pub fn claude_mcp_file(&self) -> PathBuf {
        self.claude_dir().join("managed-mcp.json")
    }

    fn claude_dir(&self) -> PathBuf {
        self.join(match self.platform {
            Platform::Macos => "/Library/Application Support/ClaudeCode",
            Platform::Linux => "/etc/claude-code",
        })
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
    /// Not gone through completely. For a directory: some of its files could
    /// not be read, or a limit stopped the scan (`files.limit_reached`), in
    /// which case there may be files that were never looked at; `keys` lists
    /// what was found in the files read. For a single file: its size changed
    /// while it was read, so no key is reported from it.
    PartlyRead,
    /// It is there and could not be read or parsed, so nothing is known about
    /// what it sets.
    Unreadable,
    /// It is there and doctor does not open it (managed preferences).
    NotInspected,
}

/// What a managed key is about. It places the key; it says nothing about what
/// the key does on this system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ManagedKeyCategory {
    /// Permission rules, working directories and what tools may touch.
    Permissions,
    /// Hooks.
    Hooks,
    /// MCP servers and the flags that configure them.
    Mcp,
    /// Plugins and other customization sources.
    Plugins,
    /// Sandboxing of commands.
    Sandbox,
    /// Login, credentials and which service is used.
    Authentication,
    /// Environment variables.
    Environment,
    /// Which models may be used.
    Model,
    /// Which provider versions may start.
    Version,
    /// How managed settings themselves are computed.
    Policy,
    /// A key of Codex's `requirements.toml`.
    CodexRequirements,
    /// A key of Codex's legacy `managed_config.toml`.
    CodexConfig,
}

impl ManagedKeyCategory {
    /// The name the JSON report and the text report use.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Permissions => "permissions",
            Self::Hooks => "hooks",
            Self::Mcp => "mcp",
            Self::Plugins => "plugins",
            Self::Sandbox => "sandbox",
            Self::Authentication => "authentication",
            Self::Environment => "environment",
            Self::Model => "model",
            Self::Version => "version",
            Self::Policy => "policy",
            Self::CodexRequirements => "codex_requirements",
            Self::CodexConfig => "codex_config",
        }
    }
}

/// A key in a managed source that can change how a delega launch is honoured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ManagedKey {
    /// The key's name, dotted when it is nested, such as `permissions.deny`.
    /// Never its value.
    pub name: String,
    /// What the key is about. Not what it does: no value is read, and what a
    /// key does is for the provider's reference to say.
    pub category: ManagedKeyCategory,
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
    /// For a directory, whether a limit on files, directory entries or bytes
    /// stopped the scan before the directory was exhausted, or a file in it
    /// changed size while it was read, so there may be more than `found` and
    /// keys may be missing. For a single file, set when the file changed size
    /// while it was read; there is no other file to find.
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
    /// For a source that is a directory, how many files it holds. For a
    /// single file it is null, with one exception: when the file changed size
    /// while it was read it is one file found, one not read and
    /// `limit_reached`, and the state is `partly_read`.
    pub files: Option<ManagedFileCounts>,
    /// What is known of the source that does not depend on a key in it. Fixed
    /// text for the kind.
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

/// What doctor says once about every Claude Code key it lists.
pub const CLAUDE_GUIDANCE: &str = "These managed keys can change how Claude Code treats what \
                                   delega passes at launch; see \
                                   https://code.claude.com/docs/en/settings-reference.";

/// What doctor says once about every Codex key it lists.
pub const CODEX_GUIDANCE: &str = "These managed keys can change how Codex treats what delega \
                                  passes at launch; see codex-rs/config/src/config_requirements.rs \
                                  and codex-rs/config/src/loader/README.md in \
                                  https://github.com/openai/codex.";

/// The sentence for a provider's keys.
pub fn guidance(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude => CLAUDE_GUIDANCE,
        Provider::Codex => CODEX_GUIDANCE,
    }
}

/// A Claude Code settings key, as the path to it, and its category.
type ClaudeKey = (&'static [&'static str], ManagedKeyCategory);

/// The managed Claude Code keys that bear on what delega passes at launch
/// (grants, the hook in `--settings`, the MCP configuration, added
/// directories, the model, the credential), each with a category. A key is
/// reported when it is present, whatever its value. Nothing here says what a
/// key does: that is in Claude Code's settings reference, which
/// [`CLAUDE_GUIDANCE`] points at. `permissions.defaultMode` is not here: delega
/// passes `--permission-mode`, which takes precedence over it. Neither is
/// `permissions.disableBypassPermissionsMode`: delega never asks for that
/// mode.
const CLAUDE_KEYS: &[ClaudeKey] = {
    use ManagedKeyCategory::{
        Authentication, Environment, Hooks, Mcp, Model, Permissions, Plugins, Policy, Sandbox,
        Version,
    };
    &[
        (&["allowManagedPermissionRulesOnly"], Permissions),
        (&["allowManagedHooksOnly"], Hooks),
        (&["disableAllHooks"], Hooks),
        (&["strictPluginOnlyCustomization"], Plugins),
        (&["disableSideloadFlags"], Mcp),
        (&["allowManagedMcpServersOnly"], Mcp),
        (&["allowedMcpServers"], Mcp),
        (&["deniedMcpServers"], Mcp),
        (&["permissions", "allow"], Permissions),
        (&["permissions", "deny"], Permissions),
        (&["permissions", "ask"], Permissions),
        (&["permissions", "additionalDirectories"], Permissions),
        (
            &["permissions", "blockReadsOutsideWorkingDirectories"],
            Permissions,
        ),
        (
            &["permissions", "blockWritesOutsideWorkingDirectories"],
            Permissions,
        ),
        (&["sandbox", "enabled"], Sandbox),
        // Each of these can loosen the sandbox delega passes in `build`.
        (&["sandbox", "autoAllowBashIfSandboxed"], Sandbox),
        (&["sandbox", "allowUnsandboxedCommands"], Sandbox),
        (&["sandbox", "failIfUnavailable"], Sandbox),
        (&["sandbox", "excludedCommands"], Sandbox),
        (&["sandbox", "filesystem", "allowWrite"], Sandbox),
        (&["sandbox", "network", "allowedDomains"], Sandbox),
        (
            &["sandbox", "filesystem", "allowManagedReadPathsOnly"],
            Sandbox,
        ),
        (&["sandbox", "network", "allowManagedDomainsOnly"], Sandbox),
        (&["forceLoginMethod"], Authentication),
        (&["forceLoginOrgUUID"], Authentication),
        (&["enforceAnthropicSignIn"], Authentication),
        (&["allowedProviders"], Authentication),
        (&["apiKeyHelper"], Authentication),
        (&["env"], Environment),
        (&["availableModels"], Model),
        (&["deniedModels"], Model),
        (&["modelOverrides"], Model),
        (&["requiredMinimumVersion"], Version),
        (&["requiredMaximumVersion"], Version),
        (&["policyHelper"], Policy),
    ]
};

/// The top-level keys of `requirements.toml` Codex reads: the fields of
/// `ConfigRequirementsToml` in `codex-rs/config/src/config_requirements.rs`,
/// with the aliases it accepts.
const CODEX_REQUIREMENT_NAMES: &[&str] = &[
    "allowed_approval_policies",
    "allowed_sandbox_modes",
    "allowed_permission_profiles",
    "default_permissions",
    "mcp_servers",
    "allow_login_shell",
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

/// The top-level keys of Codex's legacy managed configuration that are named:
/// the settings delega also passes with `-c`, and `approvals_reviewer`, which
/// Codex's loader reads from this file beside `approval_policy` and
/// `sandbox_mode`. Every other key of the file is counted.
const CODEX_MANAGED_CONFIG_NAMES: &[&str] = &[
    "approval_policy",
    "sandbox_mode",
    "approvals_reviewer",
    "allow_login_shell",
    "sandbox_workspace_write",
    "developer_instructions",
    "model_reasoning_effort",
    "mcp_servers",
];

const CHANGED_NOTE: &str = "the file changed size while doctor read it, so nothing is reported \
                            from it";
const CLAUDE_PREFERENCES_NOTE: &str = "can set any managed Claude Code key; doctor does not read \
                                       it";
const CODEX_PREFERENCES_NOTE: &str = "can carry a managed configuration (config_toml_base64) or \
                                      requirements (requirements_toml_base64); doctor does not \
                                      read it";
const USER_PREFERENCES_NOTE: &str = "managed preferences of the provider in users' directories; \
                                     only the current user's apply, and doctor does not read them";

pub(crate) const MCP_EXCLUSIVE: &str = "managed_mcp_exclusive";

/// Refuse the strict MCP launch when Claude Code's managed file parses.
pub(super) fn check_claude_mcp(system: &SystemPaths) -> Result<(), Refusal> {
    let path = system.claude_mcp_file();
    if file_source(
        system,
        &Limits::default(),
        &path,
        ManagedSourceKind::McpFile,
    )
    .is_some_and(|source| source.state == ManagedSourceState::Read)
    {
        return Err(Refusal::new(
            MCP_EXCLUSIVE,
            format!(
                "Claude Code exits at startup when given --strict-mcp-config while {} is deployed. \
                 See https://code.claude.com/docs/en/managed-mcp#servers-passed-with---mcp-config-or---strict-mcp-config",
                path.display()
            ),
        ));
    }
    Ok(())
}

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
            let dir = system.claude_dir();
            found.extend(file_source(
                system,
                limits,
                &dir.join("managed-settings.json"),
                ManagedSourceKind::SettingsFile,
            ));
            drop_ins(&mut found, system, limits, &dir.join("managed-settings.d"));
            found.extend(file_source(
                system,
                limits,
                &system.claude_mcp_file(),
                ManagedSourceKind::McpFile,
            ));
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
            found.extend(file_source(
                system,
                limits,
                &dir.join("requirements.toml"),
                ManagedSourceKind::RequirementsFile,
            ));
            found.extend(file_source(
                system,
                limits,
                &dir.join("managed_config.toml"),
                ManagedSourceKind::ManagedConfigFile,
            ));
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

/// The bytes of the regular file at `path`, when it holds at most `limit` and
/// kept its size while it was read. `charged` gets the number of bytes read
/// from the file, whatever comes of them, so a caller with a total to keep to
/// can count them. It is never more than `limit`.
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
    let mut file = std::fs::File::from(handle);
    if !file.metadata().map_err(|_| Unread::Unusable)?.is_file() {
        return Err(Unread::Unusable);
    }
    read_stable(
        &mut file,
        |file| file.metadata().map(|meta| meta.len()),
        limit,
        charged,
    )
}

/// The whole of `source`, when it holds at most `limit` bytes and its size, as
/// `size` gives it, is the same before and after the read and is the number of
/// bytes read. Anything else is not a consistent copy: a source that grew or
/// shrank meanwhile, in whatever order that and the read happened, is
/// [`Unread::Changed`]. No byte past `limit` is read to find out that there is
/// one, and `charged` gets the number read, also on failure.
pub(crate) fn read_stable<R: Read>(
    source: &mut R,
    size: impl Fn(&R) -> std::io::Result<u64>,
    limit: u64,
    charged: &mut u64,
) -> Result<Vec<u8>, Unread> {
    *charged = 0;
    let before = size(source).map_err(|_| Unread::Unusable)?;
    if before > limit {
        return Err(Unread::OverLimit);
    }
    let mut bytes = Vec::new();
    let read = source.by_ref().take(limit).read_to_end(&mut bytes);
    *charged = bytes.len() as u64;
    read.map_err(|_| Unread::Unusable)?;
    let after = size(source).map_err(|_| Unread::Unusable)?;
    if after != before || after != bytes.len() as u64 {
        return Err(Unread::Changed);
    }
    Ok(bytes)
}

/// The text in `bytes`, without a byte order mark, when they are UTF-8.
fn decode(bytes: Vec<u8>) -> Option<String> {
    let text = String::from_utf8(bytes).ok()?;
    Some(text.strip_prefix('\u{feff}').unwrap_or(&text).to_string())
}

/// Why a file gave no bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unread {
    /// It is a regular file with more bytes than the limit given.
    OverLimit,
    /// Its size changed while it was read, or it holds more than was read.
    Changed,
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
    system: &SystemPaths,
    limits: &Limits,
    path: &Path,
    kind: ManagedSourceKind,
) -> Option<ManagedSource> {
    let mut changed = false;
    let keys = match system.presence(path) {
        Presence::Absent => return None,
        Presence::Unusable => None,
        Presence::Usable => match read_bytes(path, limits.file_bytes, &mut 0) {
            Ok(bytes) => decode(bytes).and_then(|text| keys_of(kind, &text)),
            Err(reason) => {
                changed = reason == Unread::Changed;
                None
            }
        },
    };
    let read = keys.is_some();
    let keys = keys.unwrap_or_default();
    Some(ManagedSource {
        path: path.display().to_string(),
        kind,
        state: if read {
            ManagedSourceState::Read
        } else if changed {
            ManagedSourceState::PartlyRead
        } else {
            ManagedSourceState::Unreadable
        },
        // A file that changed under the read is reported as cut short.
        files: changed.then_some(ManagedFileCounts {
            found: 1,
            unreadable: 1,
            limit_reached: true,
        }),
        note: changed.then(|| CHANGED_NOTE.to_string()),
        keys: keys.keys,
        other_keys: keys.other,
    })
}

/// What `text` holds of the kind's tables, or `None` when it does not parse
/// as the kind's format.
fn keys_of(kind: ManagedSourceKind, text: &str) -> Option<Found> {
    match kind {
        ManagedSourceKind::SettingsFile | ManagedSourceKind::SettingsDropIns => {
            // Claude Code counts an empty managed settings file as `{}`.
            if text.is_empty() {
                return Some(Found::default());
            }
            Some(Found {
                keys: claude_keys(text)?,
                other: 0,
            })
        }
        ManagedSourceKind::McpFile => is_json(text).then(Found::default),
        ManagedSourceKind::RequirementsFile => codex_keys(
            text,
            CODEX_REQUIREMENT_NAMES,
            ManagedKeyCategory::CodexRequirements,
        ),
        ManagedSourceKind::ManagedConfigFile => codex_keys(
            text,
            CODEX_MANAGED_CONFIG_NAMES,
            ManagedKeyCategory::CodexConfig,
        ),
        ManagedSourceKind::ManagedPreferences | ManagedSourceKind::UserManagedPreferences => None,
    }
}

/// Check JSON syntax without converting values, including very large numbers.
fn is_json(text: &str) -> bool {
    serde_json::from_str::<IgnoredAny>(text).is_ok()
}

fn is_json_object(text: &str) -> bool {
    text.trim_start().starts_with('{') && is_json(text)
}

/// The keys of [`CLAUDE_KEYS`] that are present in the settings `text`, or
/// `None` when it is not JSON with an object at the top. Present is all that
/// is asked: no value is read.
fn claude_keys(text: &str) -> Option<Vec<ManagedKey>> {
    if !is_json_object(text) {
        return None;
    }
    let mut present = vec![false; CLAUDE_KEYS.len()];
    let mut walk = Walk {
        bytes: text.as_bytes(),
        at: 0,
    };
    walk.object(&mut Vec::new(), &mut present)?;
    Some(
        CLAUDE_KEYS
            .iter()
            .zip(present)
            .filter(|(_, present)| *present)
            .map(|((path, category), _)| ManagedKey {
                name: path.join("."),
                category: *category,
            })
            .collect(),
    )
}

/// One pass over text that [`is_json_object`] has accepted, marking which
/// paths of [`CLAUDE_KEYS`] are present.
///
/// It goes by the shape of the text and converts nothing. Every member of an
/// object is visited, so a name written twice counts both times: an occurrence
/// whose value is an object is entered when keys are looked for under it, and
/// an occurrence with any other value is skipped, without that ending the
/// walk. A member name that does not decode to text (an escaped lone
/// surrogate) matches no key, and only that member is skipped.
struct Walk<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Walk<'a> {
    /// The next byte that is not white space, without passing it.
    fn peek(&mut self) -> Option<u8> {
        while matches!(self.bytes.get(self.at)?, b' ' | b'\t' | b'\n' | b'\r') {
            self.at += 1;
        }
        self.bytes.get(self.at).copied()
    }

    /// Pass `byte`, which must come next.
    fn expect(&mut self, byte: u8) -> Option<()> {
        (self.peek()? == byte).then(|| self.at += 1)
    }

    /// Pass the string that starts here and return it with its quotes.
    fn string(&mut self) -> Option<&'a [u8]> {
        let start = self.at;
        self.expect(b'"')?;
        loop {
            match self.bytes.get(self.at)? {
                b'\\' => self.at += 2,
                b'"' => {
                    self.at += 1;
                    return self.bytes.get(start..self.at);
                }
                _ => self.at += 1,
            }
        }
    }

    /// Pass the value that starts here, whatever it is, without recursion.
    fn skip(&mut self) -> Option<()> {
        let mut depth = 0_usize;
        loop {
            match self.peek()? {
                b'"' => {
                    self.string()?;
                }
                b'{' | b'[' => {
                    depth += 1;
                    self.at += 1;
                }
                b'}' | b']' => {
                    depth = depth.checked_sub(1)?;
                    self.at += 1;
                }
                // Between values inside a container.
                b',' | b':' if depth > 0 => self.at += 1,
                // A number or a literal: up to what ends it.
                _ => {
                    while !matches!(
                        self.bytes.get(self.at),
                        None | Some(b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r')
                    ) {
                        self.at += 1;
                    }
                }
            }
            if depth == 0 {
                return Some(());
            }
        }
    }

    /// Go through the object that starts here. `path` is the names that lead
    /// to it. Each member whose path is a key of the table is marked in
    /// `present`, and a member that is an object is entered when the table has
    /// keys under its path.
    fn object(&mut self, path: &mut Vec<String>, present: &mut [bool]) -> Option<()> {
        self.expect(b'{')?;
        if self.peek()? == b'}' {
            self.at += 1;
            return Some(());
        }
        loop {
            self.peek()?;
            let raw = self.string()?;
            self.expect(b':')?;
            // A name that is not text matches nothing; its value is passed.
            match serde_json::from_slice::<String>(raw).ok() {
                Some(name) => {
                    path.push(name);
                    let mut parent = false;
                    for (index, (key, _)) in CLAUDE_KEYS.iter().enumerate() {
                        if key.len() >= path.len()
                            && key.iter().zip(path.iter()).all(|(a, b)| a == b)
                        {
                            if key.len() == path.len() {
                                present[index] = true;
                            } else {
                                parent = true;
                            }
                        }
                    }
                    let entered = if parent && self.peek()? == b'{' {
                        self.object(path, present)
                    } else {
                        self.skip()
                    };
                    path.pop();
                    entered?;
                }
                None => self.skip()?,
            }
            match self.peek()? {
                b',' => self.at += 1,
                b'}' => {
                    self.at += 1;
                    return Some(());
                }
                _ => return None,
            }
        }
    }
}

/// The top-level keys of a Codex file that are in `table`, under the table's
/// own names and in the order of the names, and the number of the others. A
/// key that is in no table is never named: its name is whatever the file says,
/// which can be a credential or text with a line break, and it may be a typing
/// mistake Codex ignores.
fn codex_keys(text: &str, table: &[&'static str], category: ManagedKeyCategory) -> Option<Found> {
    // The table as the parser leaves it, before numbers are converted: a
    // number out of range does not fail here, so it hides no key.
    let parsed = toml::de::DeTable::parse(text).ok()?;
    let present: BTreeSet<&str> = parsed
        .get_ref()
        .keys()
        .map(|name| name.get_ref().as_ref())
        .collect();
    let mut keys: Vec<ManagedKey> = table
        .iter()
        .filter(|name| present.contains(**name))
        .map(|name| ManagedKey {
            name: (*name).to_string(),
            category,
        })
        .collect();
    keys.sort_by(|a, b| a.name.cmp(&b.name));
    let other = present.len() - keys.len();
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
                // Left out by the total, or changed under the read: either
                // way the directory was not gone through as it stands.
                counts.limit_reached |= reason == Unread::Changed
                    || (reason == Unread::OverLimit && within_total < limits.file_bytes);
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
        .map(|(path, category)| (path.join("."), *category))
        .filter(|(name, _)| names.contains(name))
        .map(|(name, category)| ManagedKey { name, category })
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
