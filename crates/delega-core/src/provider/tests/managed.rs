//! The managed settings scan, against temporary directories.
//!
//! Every test builds its own root, so none reads `/Library`, `/etc` or any
//! other real system location. Both layouts are tested on every system: the
//! layout is a parameter, not the build target.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use crate::contract::request::Provider;
use crate::provider::managed::{
    ManagedSource, ManagedSourceKind, ManagedSourceState, Platform, SystemPaths, scan,
};

/// A value no report may contain.
const SECRET: &str = "sk-distinctive-managed-value-4417";

const MAC_CLAUDE: &str = "Library/Application Support/ClaudeCode";
const LINUX_CLAUDE: &str = "etc/claude-code";
const PREFERENCES: &str = "Library/Managed Preferences";

struct Root {
    dir: TempDir,
}

impl Root {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("temporary root"),
        }
    }

    fn write(&self, relative: &str, content: &str) -> PathBuf {
        let path = self.dir.path().join(relative);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create directories");
        std::fs::write(&path, content).expect("write file");
        path
    }

    fn scan(&self, provider: Provider, platform: Platform) -> Vec<ManagedSource> {
        scan(
            provider,
            &SystemPaths {
                root: self.dir.path().to_path_buf(),
                platform,
            },
        )
    }

    fn mac(&self, provider: Provider) -> Vec<ManagedSource> {
        self.scan(provider, Platform::Macos)
    }
}

fn names(source: &ManagedSource) -> Vec<&str> {
    source.keys.iter().map(|k| k.name.as_str()).collect()
}

fn text(path: &Path) -> String {
    path.display().to_string()
}

/// The key names reported for one `managed-settings.json` with this content.
fn claude_names(json: &str) -> Vec<String> {
    let root = Root::new();
    root.write(&format!("{MAC_CLAUDE}/managed-settings.json"), json);
    let found = root.mac(Provider::Claude);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].kind, ManagedSourceKind::SettingsFile);
    assert_eq!(found[0].state, ManagedSourceState::Read);
    assert!(
        found[0]
            .keys
            .iter()
            .all(|k| !k.effect.is_empty() && !k.effect.contains(SECRET)),
        "{found:?}"
    );
    found[0].keys.iter().map(|k| k.name.clone()).collect()
}

#[test]
fn nothing_is_reported_when_no_managed_source_exists() {
    let root = Root::new();
    for platform in [Platform::Macos, Platform::Linux] {
        for provider in [Provider::Claude, Provider::Codex] {
            assert_eq!(root.scan(provider, platform), Vec::new());
        }
    }
    // Empty directories are not sources either.
    for dir in [MAC_CLAUDE, LINUX_CLAUDE, PREFERENCES, "etc/codex"] {
        std::fs::create_dir_all(root.dir.path().join(dir)).expect("create directory");
    }
    std::fs::create_dir_all(root.dir.path().join(MAC_CLAUDE).join("managed-settings.d"))
        .expect("create drop-in directory");
    for platform in [Platform::Macos, Platform::Linux] {
        for provider in [Provider::Claude, Provider::Codex] {
            assert_eq!(root.scan(provider, platform), Vec::new());
        }
    }
}

#[test]
fn the_claude_settings_file_is_found_in_the_directory_of_its_platform_only() {
    let root = Root::new();
    let mac = root.write(
        &format!("{MAC_CLAUDE}/managed-settings.json"),
        r#"{"disableAllHooks":true}"#,
    );
    let linux = root.write(
        &format!("{LINUX_CLAUDE}/managed-settings.json"),
        r#"{"allowManagedHooksOnly":true}"#,
    );
    let on_mac = root.scan(Provider::Claude, Platform::Macos);
    assert_eq!(on_mac.len(), 1, "{on_mac:?}");
    assert_eq!(on_mac[0].path, text(&mac));
    assert_eq!(names(&on_mac[0]), ["disableAllHooks"]);
    let on_linux = root.scan(Provider::Claude, Platform::Linux);
    assert_eq!(on_linux.len(), 1, "{on_linux:?}");
    assert_eq!(on_linux[0].path, text(&linux));
    assert_eq!(names(&on_linux[0]), ["allowManagedHooksOnly"]);
    // Claude Code's files say nothing about Codex.
    assert_eq!(root.scan(Provider::Codex, Platform::Macos), Vec::new());
}

#[test]
fn the_key_that_makes_command_line_grants_ignored_is_named() {
    assert_eq!(
        claude_names(r#"{"allowManagedPermissionRulesOnly":true}"#),
        ["allowManagedPermissionRulesOnly"]
    );
    let root = Root::new();
    root.write(
        &format!("{MAC_CLAUDE}/managed-settings.json"),
        r#"{"allowManagedPermissionRulesOnly":true}"#,
    );
    let effect = &root.mac(Provider::Claude)[0].keys[0].effect;
    assert!(effect.contains("--allowedTools"), "{effect}");
}

#[test]
fn the_keys_that_stop_delegas_hook_are_named() {
    assert_eq!(
        claude_names(
            r#"{"disableAllHooks":true,"allowManagedHooksOnly":true,
                "strictPluginOnlyCustomization":["hooks"]}"#
        ),
        [
            "allowManagedHooksOnly",
            "disableAllHooks",
            "strictPluginOnlyCustomization"
        ]
    );
}

#[test]
fn the_keys_that_can_refuse_the_mcp_configuration_are_named() {
    assert_eq!(
        claude_names(&format!(
            r#"{{"allowManagedMcpServersOnly":true,
                "allowedMcpServers":[{{"serverName":"{SECRET}"}}],
                "deniedMcpServers":[{{"serverName":"{SECRET}"}}],
                "disableSideloadFlags":true}}"#
        )),
        [
            "disableSideloadFlags",
            "allowManagedMcpServersOnly",
            "allowedMcpServers",
            "deniedMcpServers"
        ]
    );
}

#[test]
fn the_children_of_permissions_are_named_with_their_parent() {
    assert_eq!(
        claude_names(&format!(
            r#"{{"permissions":{{
                "allow":["Bash({SECRET})"],"deny":["Read(./{SECRET})"],"ask":[],
                "defaultMode":"plan","additionalDirectories":["/{SECRET}"],
                "blockReadsOutsideWorkingDirectories":true,
                "blockWritesOutsideWorkingDirectories":true}}}}"#
        )),
        [
            "permissions.allow",
            "permissions.deny",
            "permissions.ask",
            "permissions.defaultMode",
            "permissions.additionalDirectories",
            "permissions.blockReadsOutsideWorkingDirectories",
            "permissions.blockWritesOutsideWorkingDirectories"
        ]
    );
    // One child alone, and a `permissions` that is not an object.
    assert_eq!(
        claude_names(r#"{"permissions":{"deny":[]}}"#),
        ["permissions.deny"]
    );
    assert_eq!(claude_names(r#"{"permissions":"x"}"#), Vec::<String>::new());
}

#[test]
fn the_sandbox_keys_that_limit_added_directories_and_domains_are_named() {
    assert_eq!(
        claude_names(
            r#"{"sandbox":{"enabled":true,
                "filesystem":{"allowManagedReadPathsOnly":true,"allowRead":["~/work"]},
                "network":{"allowManagedDomainsOnly":true}}}"#
        ),
        [
            "sandbox.enabled",
            "sandbox.filesystem.allowManagedReadPathsOnly",
            "sandbox.network.allowManagedDomainsOnly"
        ]
    );
}

#[test]
fn the_authentication_keys_are_named() {
    assert_eq!(
        claude_names(&format!(
            r#"{{"forceLoginMethod":"claudeai","forceLoginOrgUUID":"{SECRET}",
                "apiKeyHelper":"/bin/{SECRET}","enforceAnthropicSignIn":true,
                "allowedProviders":["anthropic"]}}"#
        )),
        [
            "forceLoginMethod",
            "forceLoginOrgUUID",
            "enforceAnthropicSignIn",
            "allowedProviders",
            "apiKeyHelper"
        ]
    );
}

#[test]
fn a_managed_env_block_is_named_without_its_variables() {
    let root = Root::new();
    root.write(
        &format!("{MAC_CLAUDE}/managed-settings.json"),
        &format!(r#"{{"env":{{"ANTHROPIC_API_KEY":"{SECRET}","NAME_{SECRET}":"1"}}}}"#),
    );
    let found = root.mac(Provider::Claude);
    assert_eq!(names(&found[0]), ["env"]);
    assert!(!format!("{found:?}").contains(SECRET), "{found:?}");
    assert!(!format!("{found:?}").contains("ANTHROPIC_API_KEY"));
}

#[test]
fn the_model_version_and_helper_keys_are_named() {
    assert_eq!(
        claude_names(
            r#"{"availableModels":["opus"],"deniedModels":["haiku"],
                "requiredMinimumVersion":"2.1.150","requiredMaximumVersion":"2.2.0",
                "policyHelper":{"path":"/x"}}"#
        ),
        [
            "availableModels",
            "deniedModels",
            "requiredMinimumVersion",
            "requiredMaximumVersion",
            "policyHelper"
        ]
    );
}

#[test]
fn a_key_that_does_not_change_what_delega_passes_is_not_listed() {
    // delega never asks for bypass mode, so the lock on it changes nothing.
    assert_eq!(
        claude_names(&format!(
            r#"{{"model":"{SECRET}","cleanupPeriodDays":7,"companyAnnouncements":["{SECRET}"],
                "permissions":{{"disableBypassPermissionsMode":"disable"}},
                "sandbox":{{"network":{{"allowedDomains":["{SECRET}"]}}}}}}"#
        )),
        Vec::<String>::new()
    );
    // A name that is relevant at the top level is not looked for elsewhere.
    assert_eq!(
        claude_names(r#"{"permissions":{"env":{},"disableAllHooks":true},"hooks":{"allow":[]}}"#),
        Vec::<String>::new()
    );
}

#[test]
fn each_drop_in_file_is_a_source_and_hidden_or_other_files_are_not() {
    let root = Root::new();
    let dir = format!("{MAC_CLAUDE}/managed-settings.d");
    let second = root.write(
        &format!("{dir}/20-security.json"),
        r#"{"allowManagedPermissionRulesOnly":true}"#,
    );
    let first = root.write(
        &format!("{dir}/10-hooks.json"),
        r#"{"disableAllHooks":true}"#,
    );
    root.write(
        &format!("{dir}/.hidden.json"),
        r#"{"disableAllHooks":true}"#,
    );
    root.write(&format!("{dir}/notes.txt"), r#"{"disableAllHooks":true}"#);
    let found = root.mac(Provider::Claude);
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found.iter().all(
        |s| s.kind == ManagedSourceKind::SettingsDropIn && s.state == ManagedSourceState::Read
    ));
    assert_eq!(found[0].path, text(&first));
    assert_eq!(names(&found[0]), ["disableAllHooks"]);
    assert_eq!(found[1].path, text(&second));
    assert_eq!(names(&found[1]), ["allowManagedPermissionRulesOnly"]);
    // With the main file, the main file comes first.
    let main = root.write(&format!("{MAC_CLAUDE}/managed-settings.json"), "{}");
    let found = root.mac(Provider::Claude);
    assert_eq!(found.len(), 3, "{found:?}");
    assert_eq!(found[0].path, text(&main));
    assert_eq!(found[0].keys, Vec::new());
}

#[test]
fn a_managed_mcp_file_is_reported_by_its_presence_without_its_servers() {
    let root = Root::new();
    let path = root.write(
        &format!("{MAC_CLAUDE}/managed-mcp.json"),
        &format!(r#"{{"mcpServers":{{"{SECRET}":{{"command":"/bin/{SECRET}"}}}}}}"#),
    );
    let found = root.mac(Provider::Claude);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].path, text(&path));
    assert_eq!(found[0].kind, ManagedSourceKind::McpFile);
    assert_eq!(found[0].state, ManagedSourceState::Read);
    assert_eq!(found[0].keys, Vec::new());
    let note = found[0].note.as_deref().expect("note");
    assert!(note.contains("--strict-mcp-config"), "{note}");
    assert!(!format!("{found:?}").contains(SECRET));
    // On Linux it is looked for under /etc/claude-code.
    root.write(&format!("{LINUX_CLAUDE}/managed-mcp.json"), "{}");
    let linux = root.scan(Provider::Claude, Platform::Linux);
    assert_eq!(linux.len(), 1);
    assert_eq!(linux[0].kind, ManagedSourceKind::McpFile);
}

#[test]
fn a_file_that_cannot_be_parsed_is_present_and_unreadable_with_no_content() {
    for content in [
        format!("not json {SECRET}"),
        format!(r#"["{SECRET}"]"#),
        format!(r#"{{"disableAllHooks": true, "{SECRET}"#),
    ] {
        let root = Root::new();
        for file in [
            "managed-settings.json",
            "managed-mcp.json",
            "managed-settings.d/a.json",
        ] {
            root.write(&format!("{MAC_CLAUDE}/{file}"), &content);
        }
        let found = root.mac(Provider::Claude);
        assert_eq!(found.len(), 3, "{found:?}");
        for source in &found {
            assert_eq!(source.state, ManagedSourceState::Unreadable, "{source:?}");
            assert_eq!(source.keys, Vec::new());
            assert_eq!(source.note, None);
        }
        assert!(!format!("{found:?}").contains(SECRET), "{found:?}");
    }
}

#[test]
fn a_file_that_cannot_be_opened_or_is_not_a_file_is_present_and_unreadable() {
    let root = Root::new();
    // A directory where the file belongs.
    std::fs::create_dir_all(
        root.dir
            .path()
            .join(MAC_CLAUDE)
            .join("managed-settings.json"),
    )
    .expect("create directory");
    let found = root.mac(Provider::Claude);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].state, ManagedSourceState::Unreadable);
    // A file without read permission. A superuser can read it anyway, so the
    // state is asserted only when the read is in fact denied.
    let root = Root::new();
    let path = root.write(
        "etc/codex/requirements.toml",
        "allowed_sandbox_modes = [\"read-only\"]\n",
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let denied = std::fs::read(&path).is_err();
    let found = root.scan(Provider::Codex, Platform::Linux);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod back");
    assert_eq!(found.len(), 1, "{found:?}");
    if denied {
        assert_eq!(found[0].state, ManagedSourceState::Unreadable);
        assert_eq!(found[0].keys, Vec::new());
    }
    // A link that points nowhere is still something an administrator put there.
    let root = Root::new();
    std::fs::create_dir_all(root.dir.path().join("etc/codex")).expect("create directory");
    std::os::unix::fs::symlink(
        root.dir.path().join("gone"),
        root.dir.path().join("etc/codex/managed_config.toml"),
    )
    .expect("symlink");
    let found = root.scan(Provider::Codex, Platform::Linux);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].state, ManagedSourceState::Unreadable);
}

#[test]
fn managed_preferences_are_reported_present_and_never_opened() {
    let root = Root::new();
    // Not a property list at all: nothing may depend on what is inside.
    let system = root.write(
        &format!("{PREFERENCES}/com.anthropic.claudecode.plist"),
        &format!("{{\"disableAllHooks\":true}} {SECRET}"),
    );
    let user = root.write(
        &format!("{PREFERENCES}/someone/com.anthropic.claudecode.plist"),
        SECRET,
    );
    let codex = root.write(
        &format!("{PREFERENCES}/someone/com.openai.codex.plist"),
        SECRET,
    );
    root.write(&format!("{PREFERENCES}/com.example.other.plist"), SECRET);
    let found = root.mac(Provider::Claude);
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(found[0].path, text(&system));
    assert_eq!(found[1].path, text(&user));
    for source in &found {
        assert_eq!(source.kind, ManagedSourceKind::ManagedPreferences);
        assert_eq!(source.state, ManagedSourceState::NotInspected);
        assert_eq!(source.keys, Vec::new());
        assert!(source.note.is_some());
    }
    let found = root.mac(Provider::Codex);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].path, text(&codex));
    assert_eq!(found[0].state, ManagedSourceState::NotInspected);
    let note = found[0].note.as_deref().expect("note");
    assert!(note.contains("requirements_toml_base64"), "{note}");
    assert!(!format!("{found:?}").contains(SECRET));
    // Managed preferences are a macOS mechanism.
    assert_eq!(root.scan(Provider::Claude, Platform::Linux), Vec::new());
    assert_eq!(root.scan(Provider::Codex, Platform::Linux), Vec::new());
}

#[test]
fn every_top_level_key_of_a_codex_requirements_file_is_named() {
    let root = Root::new();
    let path = root.write(
        "etc/codex/requirements.toml",
        &format!(
            "default_permissions = \"{SECRET}\"\n\
             allowed_sandbox_modes = [\"read-only\"]\n\
             allowed_approval_policies = [\"on-request\"]\n\
             some_future_requirement = \"{SECRET}\"\n\
             [allowed_permission_profiles]\n\
             \"{SECRET}\" = true\n\
             [mcp_servers.{SECRET}]\n\
             identity = {{ command = \"{SECRET}\" }}\n"
        ),
    );
    for platform in [Platform::Macos, Platform::Linux] {
        let found = root.scan(Provider::Codex, platform);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].path, text(&path));
        assert_eq!(found[0].kind, ManagedSourceKind::RequirementsFile);
        assert_eq!(found[0].state, ManagedSourceState::Read);
        assert_eq!(
            names(&found[0]),
            [
                "allowed_approval_policies",
                "allowed_permission_profiles",
                "allowed_sandbox_modes",
                "default_permissions",
                "mcp_servers",
                "some_future_requirement"
            ]
        );
        let effect = |name: &str| {
            found[0]
                .keys
                .iter()
                .find(|k| k.name == name)
                .expect("key")
                .effect
                .clone()
        };
        assert!(effect("allowed_approval_policies").contains("approval_policy"));
        assert!(effect("allowed_sandbox_modes").contains("sandbox_mode"));
        // A key this build does not know still gets a reason.
        assert!(effect("some_future_requirement").contains("requirement"));
        assert!(!format!("{found:?}").contains(SECRET), "{found:?}");
    }
    assert_eq!(root.scan(Provider::Claude, Platform::Linux), Vec::new());
}

#[test]
fn every_top_level_key_of_a_codex_managed_config_is_named() {
    let root = Root::new();
    let path = root.write(
        "etc/codex/managed_config.toml",
        &format!(
            "sandbox_mode = \"read-only\"\n\
             approval_policy = \"on-request\"\n\
             model = \"{SECRET}\"\n\
             [shell_environment_policy]\n\
             set = {{ TOKEN = \"{SECRET}\" }}\n"
        ),
    );
    let found = root.scan(Provider::Codex, Platform::Linux);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].path, text(&path));
    assert_eq!(found[0].kind, ManagedSourceKind::ManagedConfigFile);
    assert_eq!(found[0].state, ManagedSourceState::Read);
    assert_eq!(
        names(&found[0]),
        [
            "approval_policy",
            "model",
            "sandbox_mode",
            "shell_environment_policy"
        ]
    );
    assert!(found[0].keys[1].effect.contains("--ignore-user-config"));
    assert!(!format!("{found:?}").contains(SECRET), "{found:?}");
    // Both files together: requirements first.
    root.write("etc/codex/requirements.toml", "");
    let found = root.scan(Provider::Codex, Platform::Linux);
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].kind, ManagedSourceKind::RequirementsFile);
    assert_eq!(found[0].state, ManagedSourceState::Read);
    assert_eq!(found[0].keys, Vec::new());
}

#[test]
fn a_codex_file_that_is_not_toml_is_present_and_unreadable_with_no_content() {
    let root = Root::new();
    root.write(
        "etc/codex/requirements.toml",
        &format!("allowed_sandbox_modes = [\n{SECRET}"),
    );
    root.write("etc/codex/managed_config.toml", &format!("= {SECRET}"));
    let found = root.scan(Provider::Codex, Platform::Macos);
    assert_eq!(found.len(), 2, "{found:?}");
    for source in &found {
        assert_eq!(source.state, ManagedSourceState::Unreadable);
        assert_eq!(source.keys, Vec::new());
    }
    assert!(!format!("{found:?}").contains(SECRET), "{found:?}");
}

#[test]
fn a_file_over_the_size_bound_is_unreadable_and_is_not_parsed() {
    let root = Root::new();
    let padding = " ".repeat(crate::provider::managed::MAX_MANAGED_FILE_BYTES as usize);
    root.write(
        &format!("{MAC_CLAUDE}/managed-settings.json"),
        &format!(r#"{{"disableAllHooks":true}}{padding}"#),
    );
    let found = root.mac(Provider::Claude);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].state, ManagedSourceState::Unreadable);
    assert_eq!(found[0].keys, Vec::new());
}

#[test]
fn no_effect_or_note_holds_an_em_dash_or_is_left_empty() {
    let root = Root::new();
    root.write(
        &format!("{MAC_CLAUDE}/managed-settings.json"),
        r#"{"allowManagedPermissionRulesOnly":1,"allowManagedHooksOnly":1,"disableAllHooks":1,
            "strictPluginOnlyCustomization":1,"disableSideloadFlags":1,
            "allowManagedMcpServersOnly":1,"allowedMcpServers":1,"deniedMcpServers":1,
            "permissions":{"allow":1,"deny":1,"ask":1,"defaultMode":1,"additionalDirectories":1,
              "blockReadsOutsideWorkingDirectories":1,"blockWritesOutsideWorkingDirectories":1},
            "sandbox":{"enabled":1,"filesystem":{"allowManagedReadPathsOnly":1},
              "network":{"allowManagedDomainsOnly":1}},
            "forceLoginMethod":1,"forceLoginOrgUUID":1,"enforceAnthropicSignIn":1,
            "allowedProviders":1,"apiKeyHelper":1,"env":1,"availableModels":1,"deniedModels":1,
            "requiredMinimumVersion":1,"requiredMaximumVersion":1,"policyHelper":1}"#,
    );
    root.write(&format!("{MAC_CLAUDE}/managed-mcp.json"), "{}");
    root.write(&format!("{PREFERENCES}/com.anthropic.claudecode.plist"), "");
    root.write(&format!("{PREFERENCES}/com.openai.codex.plist"), "");
    root.write(
        "etc/codex/requirements.toml",
        "allowed_approval_policies = 1\nallowed_sandbox_modes = 1\nallow_login_shell = 1\n\
         allowed_permission_profiles = 1\ndefault_permissions = 1\nmcp_servers = 1\nother = 1\n",
    );
    root.write(
        "etc/codex/managed_config.toml",
        "approval_policy = 1\nsandbox_mode = 1\napprovals_reviewer = 1\nother = 1\n",
    );
    let claude = root.mac(Provider::Claude);
    // Every key of the table was found, so every effect is checked.
    assert_eq!(claude[0].keys.len(), 29, "{:?}", claude[0].keys);
    let codex = root.mac(Provider::Codex);
    for source in claude.iter().chain(&codex) {
        for words in source
            .keys
            .iter()
            .map(|k| &k.effect)
            .chain(source.note.iter())
        {
            assert!(!words.is_empty() && !words.contains('\u{2014}'), "{words}");
            assert!(!words.contains("  "), "{words}");
        }
    }
}
