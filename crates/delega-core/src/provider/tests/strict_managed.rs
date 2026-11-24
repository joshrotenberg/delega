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
}

impl Setup {
    fn new(platform: Platform) -> Self {
        Self {
            rig: Rig::new(),
            root: tempfile::tempdir().expect("temporary root"),
            platform,
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
        options
    }

    fn request(&self, mode: Mode, strict: bool) -> Request {
        let mut request = self.rig.request(Provider::Claude, "p");
        request.permissions.mode = mode;
        request.permissions.strict = strict;
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
fn a_preferences_directory_that_cannot_be_listed_refuses_strict() {
    let setup = Setup::new(Platform::Macos);
    let dir = setup.root.path().join(PREFERENCES);
    std::fs::create_dir_all(&dir).expect("create directory");
    // Search is allowed, so the current user's file could still be opened by
    // its path; listing is not.
    let Some(_restore) = deny_listing(&dir, 0o311) else {
        return;
    };
    let refusal = setup.refusal(Mode::Read);
    assert!(
        refusal.message.contains(&format!(
            "the managed source {} could not be listed completely, so it may hold the current \
             user's managed preferences file",
            text(&dir)
        )),
        "{}",
        refusal.message
    );
}

#[test]
fn a_managed_directory_that_denies_search_refuses_strict() {
    let setup = Setup::new(Platform::Linux);
    let dir = setup.root.path().join(LINUX_CLAUDE);
    std::fs::create_dir_all(&dir).expect("create directory");
    // Nothing under it can be looked at, so nothing proves it holds no
    // managed settings file.
    let Some(_restore) = deny_listing(&dir, 0o000) else {
        return;
    };
    let refusal = setup.refusal(Mode::Read);
    assert!(
        refusal.message.contains(&format!(
            "the managed source {} could not be read or parsed, so it may set",
            text(&dir.join("managed-settings.json"))
        )),
        "{}",
        refusal.message
    );
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
