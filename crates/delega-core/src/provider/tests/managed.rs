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
    MAX_DROP_IN_FILES, MAX_MANAGED_FILE_BYTES, ManagedFileCounts, ManagedSource, ManagedSourceKind,
    ManagedSourceState, Platform, SystemPaths, scan,
};
use crate::test_spawn::Guarded;

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
        // A key this build does not know is named without a claim about it.
        assert!(effect("some_future_requirement").contains("Codex may ignore it"));
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
fn no_effect_or_note_holds_an_em_dash_or_is_left_empty() {
    let root = Root::new();
    root.write(
        &format!("{MAC_CLAUDE}/managed-settings.json"),
        r#"{"allowManagedPermissionRulesOnly":1,"allowManagedHooksOnly":1,"disableAllHooks":1,
            "strictPluginOnlyCustomization":1,"disableSideloadFlags":1,
            "allowManagedMcpServersOnly":1,"allowedMcpServers":1,"deniedMcpServers":1,
            "permissions":{"allow":1,"deny":1,"ask":1,"additionalDirectories":1,
              "blockReadsOutsideWorkingDirectories":1,"blockWritesOutsideWorkingDirectories":1},
            "sandbox":{"enabled":1,"filesystem":{"allowManagedReadPathsOnly":1},
              "network":{"allowManagedDomainsOnly":1}},
            "forceLoginMethod":1,"forceLoginOrgUUID":1,"enforceAnthropicSignIn":1,
            "allowedProviders":1,"apiKeyHelper":1,"env":1,"availableModels":1,"deniedModels":1,"modelOverrides":1,
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

fn counts(found: usize, unreadable: usize, limit_reached: bool) -> Option<ManagedFileCounts> {
    Some(ManagedFileCounts {
        found,
        unreadable,
        limit_reached,
    })
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
    for index in 0..MAX_DROP_IN_FILES + 5 {
        root.write(&format!("{dir}/{index:03}.json"), "{}");
    }
    let found = root.mac(Provider::Claude);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].files, counts(MAX_DROP_IN_FILES, 0, true));
    // Files were left out, so the directory is not reported as wholly read.
    assert_eq!(found[0].state, ManagedSourceState::PartlyRead);
}

#[test]
fn a_drop_in_directory_is_read_up_to_its_byte_limit_and_says_so() {
    let root = Root::new();
    let dir = format!("{MAC_CLAUDE}/managed-settings.d");
    // Each fits alone; the two together are over the total.
    let padding = " ".repeat(MAX_MANAGED_FILE_BYTES as usize * 3 / 4);
    for name in ["a.json", "b.json"] {
        root.write(
            &format!("{dir}/{name}"),
            &format!(r#"{{"disableAllHooks":true}}{padding}"#),
        );
    }
    let found = root.mac(Provider::Claude);
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
fn a_pipe_where_a_file_belongs_is_unreadable_and_does_not_block() {
    let root = Root::new();
    let dir = root.dir.path().join(MAC_CLAUDE);
    std::fs::create_dir_all(dir.join("managed-settings.d")).expect("create directories");
    for name in ["managed-settings.json", "managed-settings.d/a.json"] {
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
        let _ = send.send(scan(Provider::Claude, &system));
    });
    let found = receive
        .recv_timeout(std::time::Duration::from_secs(20))
        .expect("the scan returned");
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(found[0].state, ManagedSourceState::Unreadable);
    assert_eq!(found[1].state, ManagedSourceState::Unreadable);
    assert_eq!(found[1].files, counts(1, 1, false));
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
fn a_codex_key_that_is_not_known_is_named_without_a_claim_that_it_is_enforced() {
    let root = Root::new();
    root.write(
        "etc/codex/requirements.toml",
        "allowed_sandox_modes = [\"read-only\"]\nrules = {}\nfeatures = {}\n",
    );
    root.write(
        "etc/codex/managed_config.toml",
        "sandbox_mdoe = \"read-only\"\nallow_login_shell = true\n",
    );
    let found = root.scan(Provider::Codex, Platform::Linux);
    let effect = |source: usize, name: &str| {
        found[source]
            .keys
            .iter()
            .find(|k| k.name == name)
            .expect("key")
            .effect
            .clone()
    };
    // A typing mistake is listed, since an administrator meant something by
    // it, and nothing says Codex enforces it.
    let typo = effect(0, "allowed_sandox_modes");
    assert!(typo.contains("Codex may ignore it"), "{typo}");
    assert!(!typo.contains("enforce"), "{typo}");
    // A key Codex reads that has no bearing on a delega flag is a requirement.
    assert!(effect(0, "rules").contains("enforces"));
    assert!(effect(0, "features").contains("enforces"));
    let typo = effect(1, "sandbox_mdoe");
    assert!(typo.contains("one it does not know is ignored"), "{typo}");
    assert!(
        !typo.contains("replaces") && !typo.contains("enforce"),
        "{typo}"
    );
    assert!(effect(1, "allow_login_shell").contains("allow_login_shell=false"));
}
