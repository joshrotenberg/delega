//! `permissions.strict` and Claude Code's managed settings: a managed source
//! that can widen a run, or one that was not read, refuses a strict run and is
//! a note on any other.
//!
//! Every test builds its own system root, so none reads `/Library`, `/etc` or
//! any other real system location.

use std::path::PathBuf;

use tempfile::TempDir;

use super::Rig;
use crate::contract::permissions::Mode;
use crate::contract::request::{Provider, Request};
use crate::provider::managed::{Platform, SystemPaths};
use crate::provider::{ProviderOptions, Refusal, dry_run};

/// A value no refusal or note may contain.
const SECRET: &str = "sk-distinctive-managed-value-5521";

const MAC_CLAUDE: &str = "Library/Application Support/ClaudeCode";
const LINUX_CLAUDE: &str = "etc/claude-code";
const PREFERENCES: &str = "Library/Managed Preferences";

const NOTE: &str = "managed settings can widen this run: ";

struct Setup {
    rig: Rig,
    root: TempDir,
    platform: Platform,
    /// `permissions.commands` of every request. When it is not empty the
    /// options carry a hook program, so read and write get the command guard.
    commands: Vec<String>,
}

impl Setup {
    fn new(platform: Platform) -> Self {
        Self {
            rig: Rig::new(),
            root: tempfile::tempdir().expect("temporary root"),
            platform,
            commands: Vec::new(),
        }
    }

    /// A setup whose requests list a command and whose options configure the
    /// command guard.
    fn guarded(platform: Platform) -> Self {
        Self {
            commands: vec!["make test".to_string()],
            ..Self::new(platform)
        }
    }

    fn write(&self, relative: &str, content: &str) -> PathBuf {
        let path = self.root.path().join(relative);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create directories");
        std::fs::write(&path, content).expect("write file");
        path
    }

    fn options(&self) -> ProviderOptions {
        let mut options = self.rig.options(Provider::Claude);
        options.system_paths = SystemPaths {
            root: self.root.path().to_path_buf(),
            platform: self.platform,
        };
        if !self.commands.is_empty() {
            options.hook = Some(Rig::guard_hook());
        }
        options
    }

    fn request(&self, mode: Mode, strict: bool) -> Request {
        let mut request = self.rig.request(Provider::Claude, "p");
        request.permissions.mode = mode;
        request.permissions.strict = strict;
        request.permissions.commands = self.commands.clone();
        request
    }

    fn refusal(&self, mode: Mode) -> Refusal {
        match dry_run(&self.request(mode, true), &self.options()) {
            Ok(_) => panic!("expected a refusal in {mode:?}"),
            Err(refusal) => refusal,
        }
    }

    /// The managed-settings notes of a run that is not strict.
    fn notes(&self, mode: Mode) -> Vec<String> {
        dry_run(&self.request(mode, false), &self.options())
            .expect("a run without strict goes ahead")
            .notes
            .into_iter()
            .filter(|n| n.starts_with(NOTE))
            .collect()
    }

    fn runs_strict(&self, mode: Mode) {
        if let Err(refusal) = dry_run(&self.request(mode, true), &self.options()) {
            panic!("expected {mode:?} with strict to run: {refusal:?}");
        }
    }
}

fn text(path: &std::path::Path) -> String {
    path.display().to_string()
}

#[test]
fn strict_builds_existing_refusal_also_names_the_managed_sandbox_keys() {
    let setup = Setup::new(Platform::Macos);
    let path = setup.write(
        &format!("{MAC_CLAUDE}/managed-settings.json"),
        &format!(
            r#"{{"sandbox":{{"enabled":false,"allowUnsandboxedCommands":true,"excludedCommands":["{SECRET}"]}}}}"#
        ),
    );
    // Claude build with strict is refused for command reads whatever is
    // managed; the managed keys are added to that same refusal.
    let refusal = setup.refusal(Mode::Build);
    assert_eq!(refusal.kind, "permissions_not_enforceable");
    assert!(
        refusal
            .message
            .contains("reads by commands are not confined"),
        "{}",
        refusal.message
    );
    let entry = format!(
        "the managed source {} sets sandbox.enabled, sandbox.allowUnsandboxedCommands, \
         sandbox.excludedCommands, and Claude Code applies managed settings above the settings \
         delega passes",
        text(&path)
    );
    assert!(refusal.message.contains(&entry), "{}", refusal.message);
    assert!(!refusal.message.contains(SECRET), "{}", refusal.message);
}

#[test]
fn managed_sandbox_keys_are_a_note_on_build_without_strict() {
    let setup = Setup::new(Platform::Linux);
    let path = setup.write(
        &format!("{LINUX_CLAUDE}/managed-settings.json"),
        r#"{"sandbox":{"filesystem":{"allowWrite":["/"]}}}"#,
    );
    let notes = setup.notes(Mode::Build);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains(&text(&path)), "{notes:?}");
    assert!(
        notes[0].contains("sets sandbox.filesystem.allowWrite"),
        "{notes:?}"
    );
}

#[test]
fn managed_sandbox_keys_alone_do_not_refuse_strict_outside_build() {
    let setup = Setup::new(Platform::Macos);
    setup.write(
        &format!("{MAC_CLAUDE}/managed-settings.json"),
        r#"{"sandbox":{"enabled":false},"permissions":{"deny":["Bash"]}}"#,
    );
    for mode in [Mode::Read, Mode::Write] {
        setup.runs_strict(mode);
        assert_eq!(setup.notes(mode), Vec::<String>::new());
    }
}

#[test]
fn a_managed_permissions_allow_refuses_strict_in_read_write_and_build() {
    let setup = Setup::new(Platform::Macos);
    let path = setup.write(
        &format!("{MAC_CLAUDE}/managed-settings.json"),
        &format!(r#"{{"permissions":{{"allow":["Read(/{SECRET}/**)"]}}}}"#),
    );
    for mode in [Mode::Read, Mode::Write, Mode::Build] {
        let refusal = setup.refusal(mode);
        assert_eq!(refusal.kind, "permissions_not_enforceable");
        assert!(
            refusal.message.contains(&format!(
                "the managed source {} sets permissions.allow,",
                text(&path)
            )),
            "{mode:?}: {}",
            refusal.message
        );
        assert!(!refusal.message.contains(SECRET), "{}", refusal.message);
        assert_eq!(setup.notes(mode).len(), 1, "{mode:?}");
    }
}

#[test]
fn full_with_strict_runs_whatever_managed_settings_hold() {
    let setup = Setup::new(Platform::Macos);
    setup.write(
        &format!("{MAC_CLAUDE}/managed-settings.json"),
        r#"{"permissions":{"allow":["Bash"]},"sandbox":{"enabled":false}}"#,
    );
    setup.write(
        &format!("{PREFERENCES}/com.anthropic.claudecode.plist"),
        "x",
    );
    setup.runs_strict(Mode::Full);
    assert_eq!(setup.notes(Mode::Full), Vec::<String>::new());
}

#[test]
fn a_drop_in_file_with_permissions_allow_refuses_strict_naming_the_directory_only() {
    let setup = Setup::new(Platform::Linux);
    setup.write(
        &format!("{LINUX_CLAUDE}/managed-settings.d/{SECRET}.json"),
        r#"{"permissions":{"allow":["Edit"]}}"#,
    );
    let dir = setup
        .root
        .path()
        .join(format!("{LINUX_CLAUDE}/managed-settings.d"));
    let refusal = setup.refusal(Mode::Read);
    assert_eq!(refusal.kind, "permissions_not_enforceable");
    assert!(
        refusal.message.contains(&format!(
            "the managed source {} sets permissions.allow,",
            text(&dir)
        )),
        "{}",
        refusal.message
    );
    assert!(!refusal.message.contains(SECRET), "{}", refusal.message);
}

#[test]
fn managed_preferences_refuse_strict_because_they_are_not_read() {
    let setup = Setup::new(Platform::Macos);
    let path = setup.write(
        &format!("{PREFERENCES}/com.anthropic.claudecode.plist"),
        SECRET,
    );
    let refusal = setup.refusal(Mode::Read);
    assert_eq!(refusal.kind, "permissions_not_enforceable");
    assert!(
        refusal.message.contains(&format!(
            "the managed source {} is managed preferences, which delega does not open, so \
             it may set permissions.allow,",
            text(&path)
        )),
        "{}",
        refusal.message
    );
    assert!(!refusal.message.contains(SECRET), "{}", refusal.message);
    let build = setup.refusal(Mode::Build);
    assert!(
        build
            .message
            .contains("so it may set permissions.allow or sandbox keys,"),
        "{}",
        build.message
    );
    assert_eq!(setup.notes(Mode::Write).len(), 1);
}

#[test]
fn a_managed_settings_file_that_does_not_parse_refuses_strict() {
    let setup = Setup::new(Platform::Linux);
    let path = setup.write(
        &format!("{LINUX_CLAUDE}/managed-settings.json"),
        &format!("{{\"permissions\": {SECRET}"),
    );
    let refusal = setup.refusal(Mode::Write);
    assert_eq!(refusal.kind, "permissions_not_enforceable");
    assert!(
        refusal.message.contains(&format!(
            "the managed source {} could not be read or parsed, so it may set",
            text(&path)
        )),
        "{}",
        refusal.message
    );
    assert!(!refusal.message.contains(SECRET), "{}", refusal.message);
}

#[test]
fn a_managed_settings_file_that_cannot_be_opened_refuses_strict() {
    use std::os::unix::fs::PermissionsExt;
    let setup = Setup::new(Platform::Linux);
    let path = setup.write(&format!("{LINUX_CLAUDE}/managed-settings.json"), "{}");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    // A privileged caller can still read a file with mode 000.
    if std::fs::File::open(&path).is_ok() {
        return;
    }
    let refusal = setup.refusal(Mode::Read);
    assert!(
        refusal
            .message
            .contains("could not be read or parsed, so it may set"),
        "{}",
        refusal.message
    );
}

#[test]
fn no_managed_source_means_no_refusal_and_no_note() {
    let setup = Setup::new(Platform::Macos);
    for mode in [Mode::Read, Mode::Write] {
        setup.runs_strict(mode);
        assert_eq!(setup.notes(mode), Vec::<String>::new());
    }
    assert_eq!(setup.notes(Mode::Build), Vec::<String>::new());
}

#[test]
fn a_read_managed_file_without_widening_keys_and_an_unreadable_mcp_file_do_not_refuse_strict() {
    let setup = Setup::new(Platform::Linux);
    setup.write(
        &format!("{LINUX_CLAUDE}/managed-settings.json"),
        r#"{"permissions":{"deny":["Bash"],"ask":["Edit"]},"env":{}}"#,
    );
    // The managed MCP file holds no settings keys, so not reading it widens nothing.
    setup.write(&format!("{LINUX_CLAUDE}/managed-mcp.json"), "{");
    for mode in [Mode::Read, Mode::Write] {
        setup.runs_strict(mode);
        assert_eq!(setup.notes(mode), Vec::<String>::new());
    }
}

#[test]
fn claude_managed_settings_add_no_note_to_a_codex_run() {
    let setup = Setup::new(Platform::Macos);
    setup.write(
        &format!("{MAC_CLAUDE}/managed-settings.json"),
        r#"{"permissions":{"allow":["Bash"]}}"#,
    );
    setup.write(
        &format!("{PREFERENCES}/com.anthropic.claudecode.plist"),
        "x",
    );
    let mut options = setup.rig.options(Provider::Codex);
    options.system_paths = setup.options().system_paths;
    let mut request = setup.rig.request(Provider::Codex, "p");
    request.permissions.mode = Mode::Read;
    let notes = dry_run(&request, &options)
        .expect("codex read without strict runs")
        .notes;
    assert!(!notes.iter().any(|n| n.starts_with(NOTE)), "{notes:?}");
}

#[test]
fn a_users_managed_preferences_refuse_strict_without_naming_the_user() {
    let setup = Setup::new(Platform::Macos);
    let user = "sentinel-user-7731";
    setup.write(
        &format!("{PREFERENCES}/{user}/com.anthropic.claudecode.plist"),
        SECRET,
    );
    let dir = setup.root.path().join(PREFERENCES);
    let refusal = setup.refusal(Mode::Read);
    assert_eq!(refusal.kind, "permissions_not_enforceable");
    assert!(
        refusal.message.contains(&format!(
            "the managed source {} holds a managed preferences file in a user's directory; only \
             the current user's applies and delega does not tell whose it is, so it may set \
             permissions.allow,",
            text(&dir)
        )),
        "{}",
        refusal.message
    );
    assert!(!refusal.message.contains(user), "{}", refusal.message);
    assert!(!refusal.message.contains(SECRET), "{}", refusal.message);
}

/// Restores a directory's mode when a test ends, so the temporary root can be
/// removed.
struct Mode0755(PathBuf);

impl Drop for Mode0755 {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

/// Set `dir` to `mode`, or `None` when the caller can list it anyway (a
/// privileged caller), so the test has nothing to check.
fn deny_listing(dir: &std::path::Path, mode: u32) -> Option<Mode0755> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).expect("chmod");
    let guard = Mode0755(dir.to_path_buf());
    std::fs::read_dir(dir).is_err().then_some(guard)
}

#[test]
fn a_preferences_directory_that_cannot_be_listed_refuses_strict_as_unchecked() {
    let setup = Setup::new(Platform::Macos);
    let dir = setup.root.path().join(PREFERENCES);
    std::fs::create_dir_all(&dir).expect("create directory");
    // Search is allowed and listing is not: macOS can still serve the current
    // user's preferences, and delega cannot see whether there are any.
    let Some(_restore) = deny_listing(&dir, 0o311) else {
        return;
    };
    let refusal = setup.refusal(Mode::Read);
    assert_eq!(refusal.kind, "permissions_not_enforceable");
    assert!(
        refusal.message.contains(&format!(
            "delega could not check the managed preferences directory {}, so managed \
             preferences may set permissions.allow,",
            text(&dir)
        )),
        "{}",
        refusal.message
    );
    assert_eq!(setup.notes(Mode::Write).len(), 1);
}

#[test]
fn a_users_preferences_directory_that_denies_search_refuses_strict_without_naming_the_user() {
    let setup = Setup::new(Platform::Macos);
    let dir = setup.root.path().join(PREFERENCES);
    let user = dir.join("sentinel-user-4419");
    std::fs::create_dir_all(&user).expect("create directory");
    let Some(_restore) = deny_listing(&user, 0o000) else {
        return;
    };
    let refusal = setup.refusal(Mode::Read);
    assert_eq!(refusal.kind, "permissions_not_enforceable");
    assert!(
        refusal.message.contains(&format!(
            "delega could not check a user's directory in {}, so managed preferences may set \
             permissions.allow,",
            text(&dir)
        )),
        "{}",
        refusal.message
    );
    assert!(
        !refusal.message.contains("sentinel-user-4419"),
        "{}",
        refusal.message
    );
}

#[test]
fn a_settings_directory_that_denies_search_adds_nothing_because_claude_code_runs_as_the_same_user()
{
    let setup = Setup::new(Platform::Macos);
    let dir = setup.root.path().join(MAC_CLAUDE);
    let path = setup.write(
        &format!("{MAC_CLAUDE}/managed-settings.json"),
        r#"{"permissions":{"allow":["Bash"]}}"#,
    );
    let Some(restore) = deny_listing(&dir, 0o000) else {
        return;
    };
    for mode in [Mode::Read, Mode::Write] {
        setup.runs_strict(mode);
        assert_eq!(setup.notes(mode), Vec::<String>::new());
    }
    assert_eq!(setup.notes(Mode::Build), Vec::<String>::new());
    // With search allowed again the same file refuses, so it was hidden by
    // the denial, not ignored.
    drop(restore);
    let refusal = setup.refusal(Mode::Read);
    assert!(
        refusal.message.contains(&format!(
            "the managed source {} sets permissions.allow,",
            text(&path)
        )),
        "{}",
        refusal.message
    );
}

#[test]
fn a_regular_file_in_place_of_a_managed_directory_adds_nothing() {
    let setup = Setup::new(Platform::Macos);
    setup.write(PREFERENCES, SECRET);
    setup.write(MAC_CLAUDE, SECRET);
    for mode in [Mode::Read, Mode::Write] {
        setup.runs_strict(mode);
        assert_eq!(setup.notes(mode), Vec::<String>::new());
    }
    assert_eq!(setup.notes(Mode::Build), Vec::<String>::new());
}

#[test]
fn a_drop_in_directory_with_one_malformed_file_refuses_strict() {
    let setup = Setup::new(Platform::Linux);
    setup.write(
        &format!("{LINUX_CLAUDE}/managed-settings.d/a.json"),
        r#"{"permissions":{"deny":["Bash"]}}"#,
    );
    setup.write(&format!("{LINUX_CLAUDE}/managed-settings.d/b.json"), "{");
    let dir = setup
        .root
        .path()
        .join(format!("{LINUX_CLAUDE}/managed-settings.d"));
    let refusal = setup.refusal(Mode::Read);
    assert_eq!(refusal.kind, "permissions_not_enforceable");
    assert!(
        refusal.message.contains(&format!(
            "the managed source {} was only partly read, so it may set",
            text(&dir)
        )),
        "{}",
        refusal.message
    );
}

/// The three managed keys that can stop delega's command guard from running
/// or deciding alone, each as a settings file that sets it, with [`SECRET`]
/// in a value.
fn hook_key_files() -> [(&'static str, String); 3] {
    [
        (
            "disableAllHooks",
            format!(r#"{{"disableAllHooks":true,"model":"{SECRET}"}}"#),
        ),
        (
            "allowManagedHooksOnly",
            format!(r#"{{"allowManagedHooksOnly":true,"model":"{SECRET}"}}"#),
        ),
        (
            "hooks",
            format!(
                r#"{{"hooks":{{"PreToolUse":[{{"matcher":"Bash","hooks":[{{"type":"command",
                    "command":"/bin/{SECRET}"}}]}}]}}}}"#
            ),
        ),
    ]
}

/// Every hook key in one file.
const ALL_HOOK_KEYS: &str = r#"{"disableAllHooks":true,"allowManagedHooksOnly":true,
    "hooks":{"PreToolUse":[]}}"#;

fn names_a_hook_key(message: &str) -> bool {
    [
        "disableAllHooks",
        "allowManagedHooksOnly",
        "sets hooks",
        "hook keys",
    ]
    .iter()
    .any(|name| message.contains(name))
}

#[test]
fn a_managed_hook_key_refuses_strict_read_and_write_that_rely_on_the_command_guard() {
    for (key, content) in hook_key_files() {
        for (platform, dir) in [
            (Platform::Macos, MAC_CLAUDE),
            (Platform::Linux, LINUX_CLAUDE),
        ] {
            let setup = Setup::guarded(platform);
            let path = setup.write(&format!("{dir}/managed-settings.json"), &content);
            for mode in [Mode::Read, Mode::Write] {
                let refusal = setup.refusal(mode);
                assert_eq!(
                    refusal.kind, "permissions_not_enforceable",
                    "{key} {mode:?}"
                );
                let entry = format!(
                    "the managed source {} sets {key}, and Claude Code applies managed settings \
                     above the settings delega passes",
                    text(&path)
                );
                assert!(
                    refusal.message.contains(&entry),
                    "{key} {mode:?}: {}",
                    refusal.message
                );
                assert!(!refusal.message.contains(SECRET), "{}", refusal.message);
            }
        }
    }
}

#[test]
fn a_managed_hook_key_is_a_note_on_read_and_write_with_the_command_guard_without_strict() {
    for (key, content) in hook_key_files() {
        let setup = Setup::guarded(Platform::Linux);
        let path = setup.write(&format!("{LINUX_CLAUDE}/managed-settings.json"), &content);
        for mode in [Mode::Read, Mode::Write] {
            let notes = setup.notes(mode);
            assert_eq!(notes.len(), 1, "{key} {mode:?}: {notes:?}");
            assert!(
                notes[0].contains(&format!("the managed source {} sets {key},", text(&path))),
                "{notes:?}"
            );
            assert!(!notes[0].contains(SECRET), "{notes:?}");
        }
    }
}

#[test]
fn a_drop_in_with_a_managed_hook_key_refuses_strict_with_the_guard_naming_the_directory_only() {
    let setup = Setup::guarded(Platform::Linux);
    setup.write(
        &format!("{LINUX_CLAUDE}/managed-settings.d/{SECRET}.json"),
        r#"{"allowManagedHooksOnly":true}"#,
    );
    let dir = setup
        .root
        .path()
        .join(format!("{LINUX_CLAUDE}/managed-settings.d"));
    let refusal = setup.refusal(Mode::Read);
    assert!(
        refusal.message.contains(&format!(
            "the managed source {} sets allowManagedHooksOnly,",
            text(&dir)
        )),
        "{}",
        refusal.message
    );
    assert!(!refusal.message.contains(SECRET), "{}", refusal.message);
}

#[test]
fn managed_hook_keys_are_named_beside_permissions_allow_with_the_guard() {
    let setup = Setup::guarded(Platform::Macos);
    let path = setup.write(
        &format!("{MAC_CLAUDE}/managed-settings.json"),
        r#"{"disableAllHooks":true,"allowManagedHooksOnly":true,"hooks":{},
            "permissions":{"allow":["Bash"]}}"#,
    );
    let refusal = setup.refusal(Mode::Write);
    assert!(
        refusal.message.contains(&format!(
            "the managed source {} sets allowManagedHooksOnly, disableAllHooks, hooks, \
             permissions.allow,",
            text(&path)
        )),
        "{}",
        refusal.message
    );
}

#[test]
fn managed_hook_keys_do_not_affect_read_and_write_without_permissions_commands() {
    for (key, content) in hook_key_files() {
        let setup = Setup::new(Platform::Macos);
        setup.write(&format!("{MAC_CLAUDE}/managed-settings.json"), &content);
        for mode in [Mode::Read, Mode::Write] {
            setup.runs_strict(mode);
            assert_eq!(setup.notes(mode), Vec::<String>::new(), "{key} {mode:?}");
        }
    }
    // Beside permissions.allow, which refuses, the hook keys are not named.
    let setup = Setup::new(Platform::Macos);
    setup.write(
        &format!("{MAC_CLAUDE}/managed-settings.json"),
        r#"{"disableAllHooks":true,"hooks":{},"permissions":{"allow":["Bash"]}}"#,
    );
    let refusal = setup.refusal(Mode::Read);
    assert!(
        refusal.message.contains("sets permissions.allow,"),
        "{}",
        refusal.message
    );
    assert!(!names_a_hook_key(&refusal.message), "{}", refusal.message);
}

#[test]
fn managed_hook_keys_are_not_named_when_permissions_commands_has_no_guard_configured() {
    // Without a hook program, permissions.commands is not granted and no
    // guard is installed; strict refuses for that reason alone.
    let setup = Setup::guarded(Platform::Linux);
    setup.write(
        &format!("{LINUX_CLAUDE}/managed-settings.json"),
        ALL_HOOK_KEYS,
    );
    let mut options = setup.options();
    options.hook = None;
    let refusal = match dry_run(&setup.request(Mode::Read, true), &options) {
        Ok(_) => panic!("expected a refusal"),
        Err(refusal) => refusal,
    };
    assert!(
        refusal.message.contains("no command guard is configured"),
        "{}",
        refusal.message
    );
    assert!(!names_a_hook_key(&refusal.message), "{}", refusal.message);
}

#[test]
fn managed_hook_keys_do_not_affect_full_with_or_without_permissions_commands() {
    for setup in [Setup::new(Platform::Macos), Setup::guarded(Platform::Macos)] {
        setup.write(
            &format!("{MAC_CLAUDE}/managed-settings.json"),
            ALL_HOOK_KEYS,
        );
        setup.runs_strict(Mode::Full);
        assert_eq!(setup.notes(Mode::Full), Vec::<String>::new());
    }
}

#[test]
fn strict_builds_refusal_does_not_name_managed_hook_keys() {
    let setup = Setup::new(Platform::Linux);
    setup.write(
        &format!("{LINUX_CLAUDE}/managed-settings.json"),
        ALL_HOOK_KEYS,
    );
    // Strict build is refused for command reads whatever is managed.
    let refusal = setup.refusal(Mode::Build);
    assert_eq!(refusal.kind, "permissions_not_enforceable");
    assert!(!names_a_hook_key(&refusal.message), "{}", refusal.message);
    assert_eq!(setup.notes(Mode::Build), Vec::<String>::new());
}

#[test]
fn sources_not_read_say_they_may_set_hook_keys_only_with_the_guard() {
    let setup = Setup::guarded(Platform::Macos);
    let settings = setup.write(&format!("{MAC_CLAUDE}/managed-settings.json"), "{");
    let preferences = setup.write(
        &format!("{PREFERENCES}/com.anthropic.claudecode.plist"),
        SECRET,
    );
    setup.write(
        &format!("{PREFERENCES}/sentinel-user-2207/com.anthropic.claudecode.plist"),
        SECRET,
    );
    let users = setup.root.path().join(PREFERENCES);
    let refusal = setup.refusal(Mode::Read);
    for entry in [
        format!(
            "the managed source {} could not be read or parsed, so it may set permissions.allow \
             or hook keys,",
            text(&settings)
        ),
        format!(
            "the managed source {} is managed preferences, which delega does not open, so it may \
             set permissions.allow or hook keys,",
            text(&preferences)
        ),
        format!(
            "the managed source {} holds a managed preferences file in a user's directory; only \
             the current user's applies and delega does not tell whose it is, so it may set \
             permissions.allow or hook keys,",
            text(&users)
        ),
    ] {
        assert!(
            refusal.message.contains(&entry),
            "{entry}\n{}",
            refusal.message
        );
    }
    assert!(!refusal.message.contains(SECRET), "{}", refusal.message);
    assert!(
        !refusal.message.contains("sentinel-user-2207"),
        "{}",
        refusal.message
    );
    // Without the guard the same sources say permissions.allow alone.
    let plain = Setup {
        commands: Vec::new(),
        ..setup
    };
    let refusal = plain.refusal(Mode::Read);
    assert!(!names_a_hook_key(&refusal.message), "{}", refusal.message);
    assert!(
        refusal
            .message
            .contains("so it may set permissions.allow, and Claude Code"),
        "{}",
        refusal.message
    );
}

#[test]
fn unchecked_managed_preferences_say_they_may_set_hook_keys_with_the_guard() {
    let setup = Setup::guarded(Platform::Macos);
    let dir = setup.root.path().join(PREFERENCES);
    std::fs::create_dir_all(&dir).expect("create directory");
    let Some(_restore) = deny_listing(&dir, 0o311) else {
        return;
    };
    let refusal = setup.refusal(Mode::Write);
    assert!(
        refusal.message.contains(&format!(
            "delega could not check the managed preferences directory {}, so managed \
             preferences may set permissions.allow or hook keys,",
            text(&dir)
        )),
        "{}",
        refusal.message
    );
}
