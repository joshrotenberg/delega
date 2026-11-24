//! The managed settings scan, against temporary directories.
//!
//! Every test builds its own root, so none reads `/Library`, `/etc` or any
//! other real system location. Both layouts are tested on every system: the
//! layout is a parameter, not the build target.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use crate::contract::outcome::Outcome;
use crate::contract::request::Provider;

use super::{Rig, run_checked};
use crate::provider::managed::{
    CLAUDE_GUIDANCE, CODEX_GUIDANCE, Limits, MAX_DROP_IN_FILES, MAX_MANAGED_FILE_BYTES,
    ManagedFileCounts, ManagedKeyCategory, ManagedSource, ManagedSourceKind, ManagedSourceState,
    Platform, SystemPaths, Unread, check_claude_mcp, guidance, read_stable, scan, scan_with,
};
use crate::test_spawn::Guarded;

/// A value no report may contain.
const SECRET: &str = "sk-distinctive-managed-value-4417";

/// Bounds small enough to reach with a few short files.
const SMALL: Limits = Limits {
    file_bytes: 4096,
    drop_in_files: 4,
    directory_entries: 16,
};

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

    /// A macOS scan with small bounds, so a test reaches one cheaply.
    fn mac_small(&self, provider: Provider) -> Vec<ManagedSource> {
        scan_with(
            provider,
            &SystemPaths {
                root: self.dir.path().to_path_buf(),
                platform: Platform::Macos,
            },
            &SMALL,
        )
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
    assert!(!format!("{found:?}").contains(SECRET), "{found:?}");
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
    let key = &root.mac(Provider::Claude)[0].keys[0];
    assert_eq!(key.category, ManagedKeyCategory::Permissions);
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
fn the_sandbox_keys_that_can_loosen_build_are_named_without_their_values() {
    let names = claude_names(&format!(
        r#"{{"sandbox":{{"autoAllowBashIfSandboxed":true,"allowUnsandboxedCommands":true,
            "failIfUnavailable":false,"excludedCommands":["{SECRET}"],
            "filesystem":{{"allowWrite":["/{SECRET}"]}},
            "network":{{"allowedDomains":["{SECRET}"]}}}}}}"#
    ));
    assert_eq!(
        names,
        [
            "sandbox.autoAllowBashIfSandboxed",
            "sandbox.allowUnsandboxedCommands",
            "sandbox.failIfUnavailable",
            "sandbox.excludedCommands",
            "sandbox.filesystem.allowWrite",
            "sandbox.network.allowedDomains"
        ]
    );
    assert!(!names.iter().any(|n| n.contains(SECRET)));
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
            r#"{"availableModels":["opus"],"deniedModels":["haiku"],"modelOverrides":{"a":"b"},
                "requiredMinimumVersion":"2.1.150","requiredMaximumVersion":"2.2.0",
                "policyHelper":{"path":"/x"}}"#
        ),
        [
            "availableModels",
            "deniedModels",
            "modelOverrides",
            "requiredMinimumVersion",
            "requiredMaximumVersion",
            "policyHelper"
        ]
    );
}

#[test]
fn a_key_that_does_not_change_what_delega_passes_is_not_listed() {
    // delega never asks for bypass mode, so the lock on it changes nothing,
    // and it passes --permission-mode, which takes precedence over a default.
    assert_eq!(
        claude_names(r#"{"permissions":{"defaultMode":"plan"}}"#),
        Vec::<String>::new()
    );
    assert_eq!(
        claude_names(&format!(
            r#"{{"model":"{SECRET}","cleanupPeriodDays":7,"companyAnnouncements":["{SECRET}"],
                "permissions":{{"disableBypassPermissionsMode":"disable"}},
                "sandbox":{{"network":{{"allowLocalBinding":true}}}}}}"#
        )),
        Vec::<String>::new()
    );
    // A name that is relevant at the top level is not looked for elsewhere.
    assert_eq!(
        claude_names(
            r#"{"permissions":{"env":{},"disableAllHooks":true,"hooks":{}},
                "plugins":{"allow":[]}}"#
        ),
        Vec::<String>::new()
    );
    // A hooks object under permissions is not the top-level hooks key.
    assert_eq!(
        claude_names(r#"{"permissions":{"hooks":{},"deny":[]}}"#),
        ["permissions.deny"]
    );
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
    assert_eq!(found[0].note, None);
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
fn the_known_keys_of_a_codex_requirements_file_are_named_and_the_others_counted() {
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
                "mcp_servers"
            ]
        );
        // A key this build does not know is counted, not named.
        assert_eq!(found[0].other_keys, 1);
        assert!(
            found[0]
                .keys
                .iter()
                .all(|k| k.category == ManagedKeyCategory::CodexRequirements)
        );
        assert!(!format!("{found:?}").contains(SECRET), "{found:?}");
    }
    assert_eq!(root.scan(Provider::Claude, Platform::Linux), Vec::new());
}

#[test]
fn the_keys_of_a_codex_managed_config_that_delega_also_passes_are_named() {
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
    assert_eq!(names(&found[0]), ["approval_policy", "sandbox_mode"]);
    assert!(
        found[0]
            .keys
            .iter()
            .all(|k| k.category == ManagedKeyCategory::CodexConfig)
    );
    // `model` and `shell_environment_policy` are counted, not named.
    assert_eq!(found[0].other_keys, 2);
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
    let padding = " ".repeat(MAX_MANAGED_FILE_BYTES as usize);
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
fn the_drop_in_directory_is_one_source_with_a_count_and_the_keys_of_its_files() {
    let root = Root::new();
    let dir = format!("{MAC_CLAUDE}/managed-settings.d");
    root.write(
        &format!("{dir}/20-security.json"),
        r#"{"allowManagedPermissionRulesOnly":true,"disableAllHooks":true}"#,
    );
    root.write(
        &format!("{dir}/10-hooks.json"),
        r#"{"disableAllHooks":true}"#,
    );
    // Hidden files and files that are not `*.json` are not read, as in Claude Code.
    root.write(&format!("{dir}/.hidden.json"), r#"{"env":{}}"#);
    root.write(&format!("{dir}/notes.txt"), r#"{"env":{}}"#);
    let found = root.mac(Provider::Claude);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].kind, ManagedSourceKind::SettingsDropIns);
    assert_eq!(found[0].state, ManagedSourceState::Read);
    assert_eq!(found[0].path, text(&root.dir.path().join(&dir)));
    assert_eq!(found[0].files, counts(2, 0, false));
    // The keys of both files together, each once, in the table's order.
    assert_eq!(
        names(&found[0]),
        ["allowManagedPermissionRulesOnly", "disableAllHooks"]
    );
    // With the main file, the main file comes first and has no count.
    let main = root.write(&format!("{MAC_CLAUDE}/managed-settings.json"), "{}");
    let found = root.mac(Provider::Claude);
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(found[0].path, text(&main));
    assert_eq!(found[0].files, None);
    // One file that does not parse makes the directory partly read.
    root.write(&format!("{dir}/30-broken.json"), &format!("no {SECRET}"));
    let found = root.mac(Provider::Claude);
    assert_eq!(found[1].state, ManagedSourceState::PartlyRead);
    assert_eq!(found[1].files, counts(3, 1, false));
    assert_eq!(names(&found[1]).len(), 2);
    assert!(!format!("{found:?}").contains(SECRET));
}

#[test]
fn a_drop_in_file_name_never_reaches_the_report() {
    let root = Root::new();
    let dir = format!("{MAC_CLAUDE}/managed-settings.d");
    // A name that looks like a credential, and one that carries a line that
    // would read as a report line of its own.
    root.write(
        &format!("{dir}/10-{SECRET}.json"),
        r#"{"disableAllHooks":true}"#,
    );
    root.write(
        &format!("{dir}/20-x\nproblems: none\n  forged-{SECRET}.json"),
        "not json",
    );
    let found = root.mac(Provider::Claude);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].path, text(&root.dir.path().join(&dir)));
    assert_eq!(found[0].files, counts(2, 1, false));
    assert_eq!(names(&found[0]), ["disableAllHooks"]);
    let shown = format!("{found:?}");
    assert!(!shown.contains(SECRET), "{shown}");
    assert!(!shown.contains("forged"), "{shown}");
    assert!(!shown.contains("problems"), "{shown}");
    let json = serde_json::to_string(&found).expect("JSON");
    assert!(!json.contains(SECRET) && !json.contains("forged"), "{json}");
    // Every path is exactly a fixed location under the root.
    let fixed = text(&root.dir.path().join(&dir));
    assert!(found.iter().all(|s| s.path == fixed));
}

#[test]
fn a_drop_in_directory_is_read_up_to_its_file_limit_and_says_so() {
    let root = Root::new();
    let dir = format!("{MAC_CLAUDE}/managed-settings.d");
    for index in 0..SMALL.drop_in_files + 5 {
        root.write(&format!("{dir}/{index:03}.json"), "{}");
    }
    let found = root.mac_small(Provider::Claude);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].files, counts(SMALL.drop_in_files, 0, true));
    // Files were left out, so the directory is not reported as wholly read.
    assert_eq!(found[0].state, ManagedSourceState::PartlyRead);
}

#[test]
fn a_drop_in_directory_is_read_up_to_its_byte_limit_and_says_so() {
    let root = Root::new();
    let dir = format!("{MAC_CLAUDE}/managed-settings.d");
    // Each fits alone; the two together are over the total.
    let padding = " ".repeat(SMALL.file_bytes as usize * 3 / 4);
    for name in ["a.json", "b.json"] {
        root.write(
            &format!("{dir}/{name}"),
            &format!(r#"{{"disableAllHooks":true}}{padding}"#),
        );
    }
    let found = root.mac_small(Provider::Claude);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].files, counts(2, 1, true));
    assert_eq!(found[0].state, ManagedSourceState::PartlyRead);
    assert_eq!(names(&found[0]), ["disableAllHooks"]);
}

#[test]
fn an_empty_settings_file_is_read_as_an_empty_object_as_claude_code_does() {
    let root = Root::new();
    root.write(&format!("{MAC_CLAUDE}/managed-settings.json"), "");
    root.write(&format!("{MAC_CLAUDE}/managed-settings.d/a.json"), "");
    // An empty MCP file is not an MCP configuration.
    root.write(&format!("{MAC_CLAUDE}/managed-mcp.json"), "");
    let found = root.mac(Provider::Claude);
    assert_eq!(found.len(), 3, "{found:?}");
    assert_eq!(found[0].state, ManagedSourceState::Read);
    assert_eq!(found[0].keys, Vec::new());
    assert_eq!(found[1].state, ManagedSourceState::Read);
    assert_eq!(found[1].files, counts(1, 0, false));
    assert_eq!(found[2].state, ManagedSourceState::Unreadable);
    // Whitespace alone is not empty and is not JSON.
    root.write(&format!("{MAC_CLAUDE}/managed-settings.json"), " \n");
    assert_eq!(
        root.mac(Provider::Claude)[0].state,
        ManagedSourceState::Unreadable
    );
}

#[test]
fn managed_fifos_neither_hang_nor_refuse_a_claude_plan() {
    let root = Root::new();
    let dir = root.dir.path().join(MAC_CLAUDE);
    std::fs::create_dir_all(dir.join("managed-settings.d")).expect("create directories");
    for name in [
        "managed-settings.json",
        "managed-settings.d/a.json",
        "managed-mcp.json",
    ] {
        let status = std::process::Command::new("/usr/bin/mkfifo")
            .arg(dir.join(name))
            .guarded_status()
            .expect("run mkfifo");
        assert!(status.success());
    }
    // With no writer, a blocking open would never return. The scan runs on
    // another thread so a regression fails the test and does not hang it.
    let system = SystemPaths {
        root: root.dir.path().to_path_buf(),
        platform: Platform::Macos,
    };
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        assert_eq!(check_claude_mcp(&system), Ok(()));
        let _ = send.send(scan(Provider::Claude, &system));
    });
    let found = receive
        .recv_timeout(std::time::Duration::from_secs(20))
        .expect("the scan returned");
    assert_eq!(found.len(), 3, "{found:?}");
    assert_eq!(found[0].state, ManagedSourceState::Unreadable);
    assert_eq!(found[1].state, ManagedSourceState::Unreadable);
    assert_eq!(found[1].files, counts(1, 1, false));
    assert_eq!(found[2].state, ManagedSourceState::Unreadable);
}

#[test]
fn a_link_out_of_an_injected_root_is_never_opened_or_listed() {
    use std::os::unix::fs::symlink;
    // What a fixture must not reach: files outside the root with keys in them.
    let outside = Root::new();
    let settings = outside.write("settings.json", r#"{"disableAllHooks":true}"#);
    outside.write("codex/requirements.toml", "allowed_sandbox_modes = []\n");
    outside.write("drop/a.json", r#"{"env":{}}"#);
    outside.write("prefs/someone/com.anthropic.claudecode.plist", "");

    // A file that is a link out: present, unreadable, nothing from the target.
    let root = Root::new();
    let dir = root.dir.path().join(MAC_CLAUDE);
    std::fs::create_dir_all(dir.join("managed-settings.d")).expect("create directories");
    symlink(&settings, dir.join("managed-settings.json")).expect("symlink");
    // A drop-in file that is a link out: counted, not read.
    symlink(&settings, dir.join("managed-settings.d/a.json")).expect("symlink");
    let found = root.mac(Provider::Claude);
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(found[0].state, ManagedSourceState::Unreadable);
    assert_eq!(found[0].keys, Vec::new());
    assert_eq!(found[1].state, ManagedSourceState::Unreadable);
    assert_eq!(found[1].files, counts(1, 1, false));
    assert_eq!(found[1].keys, Vec::new());

    // Directories that are links out: nothing below them is looked at.
    let root = Root::new();
    std::fs::create_dir_all(root.dir.path().join("etc")).expect("create directory");
    std::fs::create_dir_all(root.dir.path().join(MAC_CLAUDE)).expect("create directory");
    std::fs::create_dir_all(root.dir.path().join("Library")).expect("create directory");
    symlink(
        outside.dir.path().join("codex"),
        root.dir.path().join("etc/codex"),
    )
    .expect("symlink");
    symlink(
        outside.dir.path().join("prefs"),
        root.dir.path().join(PREFERENCES),
    )
    .expect("symlink");
    assert_eq!(root.mac(Provider::Codex), Vec::new());
    // The drop-in directory itself is a fixed location: a link out is
    // reported as present and unreadable, and is not listed.
    symlink(
        outside.dir.path().join("drop"),
        root.dir.path().join(MAC_CLAUDE).join("managed-settings.d"),
    )
    .expect("symlink");
    let found = root.mac(Provider::Claude);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].kind, ManagedSourceKind::SettingsDropIns);
    assert_eq!(found[0].state, ManagedSourceState::Unreadable);
    assert_eq!(found[0].files, None);

    // A user's preferences directory that is a link out is not counted.
    let root = Root::new();
    std::fs::create_dir_all(root.dir.path().join(PREFERENCES)).expect("create directory");
    symlink(
        outside.dir.path().join("prefs/someone"),
        root.dir.path().join(PREFERENCES).join("linked"),
    )
    .expect("symlink");
    assert_eq!(root.mac(Provider::Claude), Vec::new());
}

#[test]
fn a_link_that_stays_inside_the_root_is_followed_as_the_providers_follow_it() {
    let root = Root::new();
    let target = root.write("opt/policy/claude.json", r#"{"disableAllHooks":true}"#);
    let dir = root.dir.path().join(MAC_CLAUDE);
    std::fs::create_dir_all(&dir).expect("create directory");
    std::os::unix::fs::symlink(&target, dir.join("managed-settings.json")).expect("symlink");
    let found = root.mac(Provider::Claude);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].state, ManagedSourceState::Read);
    assert_eq!(names(&found[0]), ["disableAllHooks"]);
    // The path reported is the fixed location, not the target.
    assert_eq!(found[0].path, text(&dir.join("managed-settings.json")));
}

#[test]
fn managed_preferences_are_reported_present_and_never_opened() {
    let root = Root::new();
    // Not a property list at all: nothing may depend on what is inside.
    let system = root.write(
        &format!("{PREFERENCES}/com.anthropic.claudecode.plist"),
        &format!("{{\"disableAllHooks\":true}} {SECRET}"),
    );
    // Users' directories can be named anything; none is named in the report.
    root.write(
        &format!("{PREFERENCES}/user-{SECRET}/com.anthropic.claudecode.plist"),
        SECRET,
    );
    root.write(
        &format!("{PREFERENCES}/line\nbreak/com.anthropic.claudecode.plist"),
        SECRET,
    );
    root.write(
        &format!("{PREFERENCES}/user-{SECRET}/com.openai.codex.plist"),
        SECRET,
    );
    root.write(&format!("{PREFERENCES}/com.example.other.plist"), SECRET);
    let directory = text(&root.dir.path().join(PREFERENCES));
    let found = root.mac(Provider::Claude);
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(found[0].path, text(&system));
    assert_eq!(found[0].kind, ManagedSourceKind::ManagedPreferences);
    assert_eq!(found[0].files, None);
    assert_eq!(found[1].path, directory);
    assert_eq!(found[1].kind, ManagedSourceKind::UserManagedPreferences);
    assert_eq!(found[1].files, counts(2, 0, false));
    for source in &found {
        assert_eq!(source.state, ManagedSourceState::NotInspected);
        assert_eq!(source.keys, Vec::new());
        assert!(source.note.is_some());
    }
    assert!(!format!("{found:?}").contains(SECRET), "{found:?}");
    assert!(!format!("{found:?}").contains("break"), "{found:?}");
    let found = root.mac(Provider::Codex);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].path, directory);
    assert_eq!(found[0].kind, ManagedSourceKind::UserManagedPreferences);
    assert_eq!(found[0].files, counts(1, 0, false));
    assert!(!format!("{found:?}").contains(SECRET));
    // The file for the whole system says which keys it can carry.
    root.write(&format!("{PREFERENCES}/com.openai.codex.plist"), SECRET);
    let found = root.mac(Provider::Codex);
    let note = found[0].note.as_deref().expect("note");
    assert!(note.contains("requirements_toml_base64"), "{note}");
    // Managed preferences are a macOS mechanism.
    assert_eq!(root.scan(Provider::Claude, Platform::Linux), Vec::new());
    assert_eq!(root.scan(Provider::Codex, Platform::Linux), Vec::new());
}

#[test]
fn a_key_name_written_in_a_codex_file_never_reaches_the_report() {
    let root = Root::new();
    // Quoted TOML keys can hold anything: a credential, and a line break that
    // would read as a report line of its own.
    let hostile = format!("\"{SECRET}\\nproblems: forged\" = true\n");
    root.write(
        "etc/codex/requirements.toml",
        &format!("{hostile}allowed_sandox_modes = []\nallowed_sandbox_modes = []\nrules = {{}}\n"),
    );
    root.write(
        "etc/codex/managed_config.toml",
        &format!("{hostile}sandbox_mdoe = \"read-only\"\nallow_login_shell = true\n"),
    );
    let found = root.scan(Provider::Codex, Platform::Linux);
    assert_eq!(found.len(), 2, "{found:?}");
    // Known names are reported; the hostile key and the typing mistake are
    // counted.
    assert_eq!(names(&found[0]), ["allowed_sandbox_modes", "rules"]);
    assert_eq!(found[0].other_keys, 2);
    assert_eq!(names(&found[1]), ["allow_login_shell"]);
    assert_eq!(found[1].other_keys, 2);
    for shown in [
        format!("{found:?}"),
        serde_json::to_string(&found).expect("JSON"),
    ] {
        assert!(!shown.contains(SECRET), "{shown}");
        assert!(!shown.contains("forged"), "{shown}");
        assert!(
            !shown.contains("sandox") && !shown.contains("mdoe"),
            "{shown}"
        );
    }
    // Every name in the report is one of this build's own.
    let hostile_json = format!(r#"{{"{SECRET}\nforged":true,"disableAllHooks":true}}"#);
    root.write(
        &format!("{MAC_CLAUDE}/managed-settings.json"),
        &hostile_json,
    );
    let claude = root.mac(Provider::Claude);
    assert_eq!(names(&claude[0]), ["disableAllHooks"]);
    assert_eq!(claude[0].other_keys, 0);
    assert!(!format!("{claude:?}").contains(SECRET));
}

#[test]
fn the_drop_in_byte_limit_counts_bytes_read_before_decoding() {
    let dir = format!("{MAC_CLAUDE}/managed-settings.d");
    let half = SMALL.file_bytes as usize / 2;
    // Two files whose bytes on disk are 3 over the total, the 3 being a byte
    // order mark that decoding removes.
    let root = Root::new();
    let body = format!("{{}}{}", " ".repeat(half - 2));
    root.write(&format!("{dir}/a.json"), &format!("\u{feff}{body}"));
    root.write(&format!("{dir}/b.json"), &body);
    let found = root.mac_small(Provider::Claude);
    assert_eq!(found[0].files, counts(2, 1, true), "{found:?}");
    assert_eq!(found[0].state, ManagedSourceState::PartlyRead);
    // Exactly the total is read whole.
    let root = Root::new();
    root.write(&format!("{dir}/a.json"), &body);
    root.write(&format!("{dir}/b.json"), &body);
    let found = root.mac_small(Provider::Claude);
    assert_eq!(found[0].files, counts(2, 0, false), "{found:?}");
    assert_eq!(found[0].state, ManagedSourceState::Read);
}

#[test]
fn bytes_that_are_not_text_count_toward_the_drop_in_byte_limit() {
    let root = Root::new();
    let dir = root.dir.path().join(MAC_CLAUDE).join("managed-settings.d");
    std::fs::create_dir_all(&dir).expect("create directory");
    let invalid = vec![0xff_u8; SMALL.file_bytes as usize / 2];
    for name in ["a.json", "b.json", "c.json", "d.json"] {
        std::fs::write(dir.join(name), &invalid).expect("write file");
    }
    // Two of them spend the total; the others are not opened.
    let found = root.mac_small(Provider::Claude);
    assert_eq!(found[0].files, counts(4, 4, true), "{found:?}");
    assert_eq!(found[0].state, ManagedSourceState::PartlyRead);
}

#[test]
fn a_directory_cut_off_by_the_entry_limit_is_partly_read_even_with_nothing_found() {
    let root = Root::new();
    let drop_ins = root.dir.path().join(MAC_CLAUDE).join("managed-settings.d");
    let preferences = root.dir.path().join(PREFERENCES);
    for dir in [&drop_ins, &preferences] {
        std::fs::create_dir_all(dir).expect("create directory");
        // More entries than are looked at, none of them a drop-in file or a
        // user's directory.
        for index in 0..=SMALL.directory_entries {
            std::fs::write(dir.join(format!("{index}.txt")), "").expect("write file");
        }
    }
    let found = root.mac_small(Provider::Claude);
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(found[0].kind, ManagedSourceKind::SettingsDropIns);
    assert_eq!(found[0].state, ManagedSourceState::PartlyRead);
    assert_eq!(found[0].files, counts(0, 0, true));
    assert_eq!(found[1].kind, ManagedSourceKind::UserManagedPreferences);
    assert_eq!(found[1].state, ManagedSourceState::PartlyRead);
    assert_eq!(found[1].files, counts(0, 0, true));
    // The same for Codex's preferences, which share the directory.
    let found = root.mac_small(Provider::Codex);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].state, ManagedSourceState::PartlyRead);
}

#[test]
fn the_bounds_doctor_uses_are_the_documented_ones() {
    assert_eq!(
        Limits::default(),
        Limits {
            file_bytes: 4 * 1024 * 1024,
            drop_in_files: 64,
            directory_entries: 4096,
        }
    );
    assert_eq!(MAX_MANAGED_FILE_BYTES, 4 * 1024 * 1024);
    assert_eq!(MAX_DROP_IN_FILES, 64);
}

fn counts(found: usize, unreadable: usize, limit_reached: bool) -> Option<ManagedFileCounts> {
    Some(ManagedFileCounts {
        found,
        unreadable,
        limit_reached,
    })
}

/// One settings object with every key of the table present, each set to
/// `value`.
fn every_claude_key(value: &str) -> String {
    format!(
        r#"{{"allowManagedPermissionRulesOnly":{v},"allowManagedHooksOnly":{v},
            "disableAllHooks":{v},"hooks":{v},"strictPluginOnlyCustomization":{v},
            "disableSideloadFlags":{v},
            "allowManagedMcpServersOnly":{v},"allowedMcpServers":{v},"deniedMcpServers":{v},
            "permissions":{{"allow":{v},"deny":{v},"ask":{v},"additionalDirectories":{v},
              "blockReadsOutsideWorkingDirectories":{v},
              "blockWritesOutsideWorkingDirectories":{v}}},
            "sandbox":{{"enabled":{v},"autoAllowBashIfSandboxed":{v},
              "allowUnsandboxedCommands":{v},"failIfUnavailable":{v},"excludedCommands":{v},
              "filesystem":{{"allowManagedReadPathsOnly":{v},"allowWrite":{v}}},
              "network":{{"allowManagedDomainsOnly":{v},"allowedDomains":{v}}}}},
            "forceLoginMethod":{v},"forceLoginOrgUUID":{v},"enforceAnthropicSignIn":{v},
            "allowedProviders":{v},"apiKeyHelper":{v},"env":{v},"availableModels":{v},
            "deniedModels":{v},"modelOverrides":{v},"requiredMinimumVersion":{v},
            "requiredMaximumVersion":{v},"policyHelper":{v}}}"#,
        v = value
    )
}

#[test]
fn a_key_that_is_present_is_listed_whatever_its_value() {
    // doctor does not decide whether a key is on: how Claude Code reads a
    // value is not reproduced, so the same keys are listed for every value.
    let with_true = claude_names(&every_claude_key("true"));
    assert_eq!(with_true.len(), 36, "{with_true:?}");
    for value in ["false", r#""false""#, "null", "[]", "{}", "0", r#""x""#] {
        assert_eq!(claude_names(&every_claude_key(value)), with_true, "{value}");
    }
    assert_eq!(
        claude_names(r#"{"disableSideloadFlags":false,"sandbox":{"enabled":false}}"#),
        ["disableSideloadFlags", "sandbox.enabled"]
    );
}

#[test]
fn the_bytes_read_from_a_drop_in_directory_never_exceed_its_total() {
    // Files of every size around the total: whatever the order the directory
    // gives them in, the report is consistent with at most the total being
    // read, and a file at exactly the total alone is read whole.
    let dir = format!("{MAC_CLAUDE}/managed-settings.d");
    let total = SMALL.file_bytes as usize;
    let root = Root::new();
    root.write(
        &format!("{dir}/a.json"),
        &format!("{{}}{}", " ".repeat(total - 2)),
    );
    let found = root.mac_small(Provider::Claude);
    assert_eq!(found[0].files, counts(1, 0, false), "{found:?}");
    // One byte more is over the limit for a single file and is not read.
    root.write(
        &format!("{dir}/a.json"),
        &format!("{{}}{}", " ".repeat(total - 1)),
    );
    let found = root.mac_small(Provider::Claude);
    assert_eq!(found[0].files, counts(1, 1, false), "{found:?}");
    // With one byte of the total spent, a file of exactly the total is left
    // out by the limit, or the one-byte file is, never neither.
    root.write(
        &format!("{dir}/a.json"),
        &format!("{{}}{}", " ".repeat(total - 2)),
    );
    root.write(&format!("{dir}/b.json"), " ");
    let found = root.mac_small(Provider::Claude);
    let files = found[0].files.expect("counts");
    assert_eq!(files.found, 2);
    assert!(files.limit_reached, "{found:?}");
}

#[test]
fn a_bound_of_the_largest_number_does_not_overflow() {
    let root = Root::new();
    root.write(
        &format!("{MAC_CLAUDE}/managed-settings.json"),
        r#"{"disableAllHooks":true}"#,
    );
    root.write(&format!("{MAC_CLAUDE}/managed-settings.d/a.json"), "{}");
    root.write("etc/codex/requirements.toml", "rules = {}\n");
    let system = SystemPaths {
        root: root.dir.path().to_path_buf(),
        platform: Platform::Macos,
    };
    let unbounded = Limits {
        file_bytes: u64::MAX,
        drop_in_files: usize::MAX,
        directory_entries: usize::MAX,
    };
    let claude = scan_with(Provider::Claude, &system, &unbounded);
    assert_eq!(claude.len(), 2, "{claude:?}");
    assert_eq!(names(&claude[0]), ["disableAllHooks"]);
    assert_eq!(claude[1].files, counts(1, 0, false));
    let codex = scan_with(Provider::Codex, &system, &unbounded);
    assert_eq!(names(&codex[0]), ["rules"]);
}

#[test]
fn every_listed_key_has_the_category_of_its_table() {
    use ManagedKeyCategory::{
        Authentication, Environment, Hooks, Mcp, Model, Permissions, Plugins, Policy, Sandbox,
        Version,
    };
    let root = Root::new();
    root.write(
        &format!("{MAC_CLAUDE}/managed-settings.json"),
        &every_claude_key("true"),
    );
    let found = root.mac(Provider::Claude);
    let listed: Vec<(&str, ManagedKeyCategory)> = found[0]
        .keys
        .iter()
        .map(|k| (k.name.as_str(), k.category))
        .collect();
    assert_eq!(
        listed,
        [
            ("allowManagedPermissionRulesOnly", Permissions),
            ("allowManagedHooksOnly", Hooks),
            ("disableAllHooks", Hooks),
            ("hooks", Hooks),
            ("strictPluginOnlyCustomization", Plugins),
            ("disableSideloadFlags", Mcp),
            ("allowManagedMcpServersOnly", Mcp),
            ("allowedMcpServers", Mcp),
            ("deniedMcpServers", Mcp),
            ("permissions.allow", Permissions),
            ("permissions.deny", Permissions),
            ("permissions.ask", Permissions),
            ("permissions.additionalDirectories", Permissions),
            (
                "permissions.blockReadsOutsideWorkingDirectories",
                Permissions
            ),
            (
                "permissions.blockWritesOutsideWorkingDirectories",
                Permissions
            ),
            ("sandbox.enabled", Sandbox),
            ("sandbox.autoAllowBashIfSandboxed", Sandbox),
            ("sandbox.allowUnsandboxedCommands", Sandbox),
            ("sandbox.failIfUnavailable", Sandbox),
            ("sandbox.excludedCommands", Sandbox),
            ("sandbox.filesystem.allowWrite", Sandbox),
            ("sandbox.network.allowedDomains", Sandbox),
            ("sandbox.filesystem.allowManagedReadPathsOnly", Sandbox),
            ("sandbox.network.allowManagedDomainsOnly", Sandbox),
            ("forceLoginMethod", Authentication),
            ("forceLoginOrgUUID", Authentication),
            ("enforceAnthropicSignIn", Authentication),
            ("allowedProviders", Authentication),
            ("apiKeyHelper", Authentication),
            ("env", Environment),
            ("availableModels", Model),
            ("deniedModels", Model),
            ("modelOverrides", Model),
            ("requiredMinimumVersion", Version),
            ("requiredMaximumVersion", Version),
            ("policyHelper", Policy),
        ]
    );
    // A key is a name and a category in the JSON, and nothing about what it
    // does: no sentence of its own.
    let json = serde_json::to_value(&found[0].keys[0]).expect("JSON");
    assert_eq!(
        json,
        serde_json::json!({"name": "allowManagedPermissionRulesOnly", "category": "permissions"})
    );
    assert_eq!(
        ManagedKeyCategory::CodexRequirements.as_str(),
        "codex_requirements"
    );
    assert_eq!(
        serde_json::to_value(ManagedKeyCategory::CodexConfig).expect("JSON"),
        "codex_config"
    );
}

#[test]
fn one_sentence_per_provider_points_at_the_providers_reference() {
    assert_eq!(guidance(Provider::Claude), CLAUDE_GUIDANCE);
    assert_eq!(guidance(Provider::Codex), CODEX_GUIDANCE);
    assert_eq!(
        CLAUDE_GUIDANCE,
        "These managed keys can change how Claude Code treats what delega passes at launch; see \
         https://code.claude.com/docs/en/settings-reference."
    );
    assert!(CODEX_GUIDANCE.starts_with(
        "These managed keys can change how Codex treats what delega passes at launch; see "
    ));
    assert!(CODEX_GUIDANCE.contains("https://github.com/openai/codex"));
    for sentence in [CLAUDE_GUIDANCE, CODEX_GUIDANCE] {
        assert!(
            !sentence.contains('\u{2014}') && !sentence.contains("  "),
            "{sentence}"
        );
    }
    // The notes of sources hold no em dash or doubled space either.
    let root = Root::new();
    root.write(&format!("{MAC_CLAUDE}/managed-mcp.json"), "{}");
    root.write(&format!("{PREFERENCES}/com.anthropic.claudecode.plist"), "");
    root.write(&format!("{PREFERENCES}/u/com.openai.codex.plist"), "");
    root.write(&format!("{PREFERENCES}/com.openai.codex.plist"), "");
    let claude = root.mac(Provider::Claude);
    let codex = root.mac(Provider::Codex);
    let notes: Vec<&String> = claude
        .iter()
        .chain(&codex)
        .filter_map(|s| s.note.as_ref())
        .collect();
    assert_eq!(notes.len(), 3, "{notes:?}");
    for note in notes {
        assert!(
            !note.is_empty() && !note.contains('\u{2014}') && !note.contains("  "),
            "{note}"
        );
    }
}

#[test]
fn a_number_no_type_can_hold_does_not_hide_the_keys_of_a_file() {
    // `1e400` is valid JSON. Values are skipped, not converted, so the keys
    // beside it and under it are still found.
    assert_eq!(
        claude_names(
            r#"{"cleanupPeriodDays":1e400,"disableAllHooks":1e400,
                "big":123456789012345678901234567890123456789012345678901234567890,
                "permissions":{"deny":1e400,"other":-1e400},
                "sandbox":{"enabled":1e400,"network":{"allowManagedDomainsOnly":1E+999}}}"#
        ),
        [
            "disableAllHooks",
            "permissions.deny",
            "sandbox.enabled",
            "sandbox.network.allowManagedDomainsOnly"
        ]
    );
    // The MCP file is judged the same way.
    let root = Root::new();
    root.write(
        &format!("{MAC_CLAUDE}/managed-mcp.json"),
        r#"{"mcpServers":{"a":{"timeout":1e400}}}"#,
    );
    assert_eq!(
        root.mac(Provider::Claude)[0].state,
        ManagedSourceState::Read
    );
    // What is not JSON is still refused: skipping values checks their syntax.
    for broken in [
        r#"{"disableAllHooks":1e}"#,
        r#"{"disableAllHooks":tru}"#,
        r#"{"a":1} x"#,
    ] {
        let root = Root::new();
        root.write(&format!("{MAC_CLAUDE}/managed-settings.json"), broken);
        assert_eq!(
            root.mac(Provider::Claude)[0].state,
            ManagedSourceState::Unreadable,
            "{broken}"
        );
    }
    // A nested object that is not an object loses its own keys only.
    assert_eq!(
        claude_names(r#"{"permissions":"x","sandbox":{"enabled":true},"env":{}}"#),
        ["sandbox.enabled", "env"]
    );
}

#[test]
fn a_float_out_of_range_does_not_hide_the_keys_of_a_codex_file() {
    let root = Root::new();
    root.write(
        "etc/codex/requirements.toml",
        "huge = 1e400\nallowed_sandbox_modes = [\"read-only\"]\n",
    );
    let found = root.scan(Provider::Codex, Platform::Linux);
    assert_eq!(found[0].state, ManagedSourceState::Read, "{found:?}");
    assert_eq!(names(&found[0]), ["allowed_sandbox_modes"]);
    assert_eq!(found[0].other_keys, 1);
}

/// A source whose reported size follows a script, for a file that changes
/// while it is read.
struct Scripted {
    data: std::io::Cursor<Vec<u8>>,
    sizes: std::cell::RefCell<Vec<u64>>,
}

impl std::io::Read for Scripted {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.data.read(buffer)
    }
}

/// Read `data` when the size is `before` at the first look and `after` at the
/// second. Returns the outcome and the bytes charged.
fn scripted(data: &[u8], before: u64, after: u64, limit: u64) -> (Result<Vec<u8>, Unread>, u64) {
    let mut source = Scripted {
        data: std::io::Cursor::new(data.to_vec()),
        sizes: std::cell::RefCell::new(vec![after, before]),
    };
    let mut charged = u64::MAX;
    let outcome = read_stable(
        &mut source,
        |source| Ok(source.sizes.borrow_mut().pop().expect("a size")),
        limit,
        &mut charged,
    );
    (outcome, charged)
}

#[test]
fn a_file_whose_size_changes_under_the_read_is_not_reported_as_read() {
    // Unchanged and wholly read.
    assert_eq!(scripted(b"abc", 3, 3, 10), (Ok(b"abc".to_vec()), 3));
    assert_eq!(scripted(b"abc", 3, 3, 3), (Ok(b"abc".to_vec()), 3));
    assert_eq!(scripted(b"", 0, 0, 0), (Ok(Vec::new()), 0));
    // A short read, then the file grew: within the limit, and past it.
    assert_eq!(scripted(b"abc", 3, 5, 10), (Err(Unread::Changed), 3));
    assert_eq!(scripted(b"abc", 3, 5000, 10), (Err(Unread::Changed), 3));
    // It grew before the read finished: more came than the first size said.
    assert_eq!(scripted(b"abcde", 3, 5, 10), (Err(Unread::Changed), 5));
    // It shrank.
    assert_eq!(scripted(b"ab", 3, 2, 10), (Err(Unread::Changed), 2));
    // The read stops at the limit and never charges past it, however much
    // the file went on to hold.
    assert_eq!(scripted(b"abcdefgh", 3, 8, 4), (Err(Unread::Changed), 4));
    assert_eq!(scripted(&[b' '; 4096], 10, 4096, 16).1, 16);
    // Larger than allowed at the first look: nothing is read or charged.
    assert_eq!(scripted(b"abcdef", 6, 6, 5), (Err(Unread::OverLimit), 0));
    // The largest bound does not overflow.
    assert_eq!(scripted(b"abc", 3, 3, u64::MAX), (Ok(b"abc".to_vec()), 3));
}

#[test]
fn every_occurrence_of_a_repeated_key_is_looked_into() {
    // The first value of a repeated name not being an object does not end the
    // search under the name: each occurrence is judged by itself.
    assert_eq!(
        claude_names(r#"{"permissions":false,"permissions":{"deny":[]},"env":{}}"#),
        ["permissions.deny", "env"]
    );
    assert_eq!(
        claude_names(r#"{"permissions":{"deny":[]},"permissions":1e400,"env":{}}"#),
        ["permissions.deny", "env"]
    );
    // The keys of every occurrence that is an object count, at each depth.
    assert_eq!(
        claude_names(
            r#"{"permissions":{"allow":[]},"permissions":[{"deny":[]}],"permissions":{"ask":[]},
                "sandbox":"x","sandbox":{"network":null,"network":{"allowManagedDomainsOnly":1},
                  "filesystem":{"allowManagedReadPathsOnly":1},"filesystem":7},
                "sandbox":{"enabled":0}}"#
        ),
        [
            "permissions.allow",
            "permissions.ask",
            "sandbox.enabled",
            "sandbox.filesystem.allowManagedReadPathsOnly",
            "sandbox.network.allowManagedDomainsOnly"
        ]
    );
    // A key is looked for where the table has it and nowhere else: not inside
    // an array, not under another parent, not as a dotted top-level name.
    assert_eq!(
        claude_names(
            r#"{"permissions.deny":1,"other":{"permissions":{"deny":[]}},
                "list":[{"disableAllHooks":true}],"permissions":{"sandbox":{"enabled":1}},
                "text":"{\"disableAllHooks\":true}","nested":{"a":[1,{"b":"}"}],"c":"]\\\""},
                "apiKeyHelper":"x"}"#
        ),
        ["apiKeyHelper"]
    );
}

#[test]
fn a_member_name_that_is_not_text_hides_only_that_member() {
    // An escaped lone surrogate is valid JSON and decodes to no text. It is
    // treated the same everywhere: the member is passed over, its neighbours
    // are found.
    assert_eq!(
        claude_names(
            r#"{"\uDEAD":{"deny":[]},"x":"\uDEAD",
                "permissions":{"\uDEAD":1,"deny":[],"\uD800x":{"allow":[]}},
                "sandbox":{"\uDEAD":{"enabled":1},"enabled":"\uDBFF"},"disableAllHooks":"\uDEAD"}"#
        ),
        ["disableAllHooks", "permissions.deny", "sandbox.enabled"]
    );
    // Escapes that do decode are compared as the text they stand for.
    assert_eq!(
        claude_names(r#"{"env":{},"permissions":{"deny":[]}}"#),
        ["permissions.deny", "env"]
    );
}

#[tokio::test]
async fn valid_managed_mcp_refuses_claude_before_even_its_version_probe() {
    for body in [
        r#"{"mcpServers":{}}"#,
        r#"{"mcpServers":{"sk-distinctive-managed-value-4417":{"command":"x"}},"number":1e9999}"#,
    ] {
        let rig = Rig::new();
        let root = Root::new();
        let path = root.write(&format!("{MAC_CLAUDE}/managed-mcp.json"), body);
        let mut options = rig.options(Provider::Claude);
        options.system_paths = SystemPaths {
            root: root.dir.path().to_path_buf(),
            platform: Platform::Macos,
        };
        options
            .env_set
            .push(("FAKE_VERSION_SLEEP".into(), "0".into()));
        let result = run_checked(rig.request(Provider::Claude, "success"), options).await;
        assert_eq!(result.outcome, Outcome::Refused);
        let error = result.error.expect("refusal");
        assert_eq!(error.kind, "managed_mcp_exclusive");
        assert!(error.message.contains(&text(&path)));
        assert!(error.message.contains("--strict-mcp-config"));
        assert!(
            error
                .message
                .contains("https://code.claude.com/docs/en/managed-mcp")
        );
        assert!(!error.message.contains(SECRET));
        assert!(rig.record.read_dir().expect("records").next().is_none());
    }
}

#[tokio::test]
async fn absent_invalid_and_zero_byte_managed_mcp_leave_claude_runnable() {
    let rig = Rig::new();
    let root = Root::new();
    let mut options = rig.options(Provider::Claude);
    options.system_paths = SystemPaths {
        root: root.dir.path().to_path_buf(),
        platform: Platform::Linux,
    };
    for body in [None, Some("{"), Some("")] {
        if let Some(body) = body {
            root.write(&format!("{LINUX_CLAUDE}/managed-mcp.json"), body);
        }
        let result = run_checked(rig.request(Provider::Claude, "success"), options.clone()).await;
        assert_eq!(result.outcome, Outcome::Completed, "{body:?}: {result:?}");
    }
}

#[test]
fn a_managed_mcp_file_that_cannot_be_read_does_not_refuse_claude() {
    let root = Root::new();
    let path = root.write(
        &format!("{MAC_CLAUDE}/managed-mcp.json"),
        r#"{"mcpServers":{}}"#,
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    // A privileged caller can still read a file with mode 000.
    if std::fs::File::open(&path).is_err() {
        let rig = Rig::new();
        let mut options = rig.options(Provider::Claude);
        options.system_paths = SystemPaths {
            root: root.dir.path().to_path_buf(),
            platform: Platform::Macos,
        };
        crate::provider::dry_run(&rig.request(Provider::Claude, "success"), &options)
            .expect("unreadable managed MCP must not refuse planning");
    }
}

#[tokio::test]
async fn a_parsing_claude_managed_mcp_file_does_not_refuse_codex() {
    let rig = Rig::new();
    let root = Root::new();
    root.write(
        &format!("{LINUX_CLAUDE}/managed-mcp.json"),
        r#"{"mcpServers":{}}"#,
    );
    let mut options = rig.options(Provider::Codex);
    options.system_paths = SystemPaths {
        root: root.dir.path().to_path_buf(),
        platform: Platform::Linux,
    };
    let result = run_checked(rig.request(Provider::Codex, "success"), options).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:?}");
}
