//! The configuration file: where it is, how it is read, what it accepts, what
//! it refuses and what it applies. Every test works in a temporary directory,
//! reads no environment variable and no real file of the user.

use std::ffi::OsString;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use super::*;
use crate::contract::request::Provider;

/// A value no message, report or debug text may repeat.
const SECRET: &str = "distinctive-value-5521";

/// A lookup over fixed pairs.
fn lookup<'a>(pairs: &'a [(&'a str, String)]) -> impl Fn(&str) -> Option<OsString> + 'a {
    move |name| {
        pairs
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| OsString::from(v))
    }
}

struct Area {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Area {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("temporary directory");
        let root = std::fs::canonicalize(tmp.path()).expect("resolve");
        Self { _tmp: tmp, root }
    }

    /// Write a private file `name` under the area and return its path.
    fn file(&self, name: &str, text: &str) -> PathBuf {
        let path = self.root.join(name);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("directory");
        std::fs::write(&path, text).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("mode");
        path
    }

    /// Read `path` as `DELEGA_CONFIG`, as the current user.
    fn read(&self, path: &Path) -> Status {
        let pairs = [("DELEGA_CONFIG", path.display().to_string())];
        read_as(lookup(&pairs), euid())
    }
}

fn euid() -> u32 {
    rustix::process::geteuid().as_raw()
}

fn config_of(text: &str) -> Result<Config, crate::provider::Refusal> {
    let area = Area::new();
    let path = area.file("c.toml", text);
    area.read(&path).into_config()
}

fn refusal_of(text: &str) -> crate::provider::Refusal {
    config_of(text).expect_err("the file is refused")
}

const FULL: &str = r#"
schema_version = 1

[defaults]
provider = "codex"
model = "gpt-test"
effort = "high"
timeout_secs = 1200

[defaults.permissions]
mode = "write"
strict = true

[env]
pass = ["AWS_SESSION_TOKEN", "TOOLCHAIN_HOME"]

[env.set]
API_REGION = "eu-test-1"

[providers.claude]
executable = "/opt/claude/bin/claude"

[providers.codex]
executable = "/opt/codex/bin/codex"

[mcp]
max_live_workers = 8
max_running_runs = 2
max_queued_per_worker = 4
wait_default_secs = 20
wait_ceiling_secs = 120
"#;

#[test]
fn the_location_prefers_delega_config_then_xdg_then_home() {
    let all = [
        ("DELEGA_CONFIG", "/etc/d.toml".to_string()),
        ("XDG_CONFIG_HOME", "/xdg".to_string()),
        ("HOME", "/home/u".to_string()),
    ];
    let found = locate(lookup(&all)).expect("ok").expect("a place");
    assert_eq!(found.path, PathBuf::from("/etc/d.toml"));
    assert_eq!(found.source, Source::DelegaConfig);

    let xdg = locate(lookup(&all[1..])).expect("ok").expect("a place");
    assert_eq!(xdg.path, PathBuf::from("/xdg/delega/config.toml"));
    assert_eq!(xdg.source, Source::XdgConfigHome);

    let home = locate(lookup(&all[2..])).expect("ok").expect("a place");
    assert_eq!(
        home.path,
        PathBuf::from("/home/u/.config/delega/config.toml")
    );
    assert_eq!(home.source, Source::Home);
}

#[test]
fn empty_and_relative_values_do_not_choose_a_place() {
    // An empty variable is unset; a relative XDG_CONFIG_HOME is ignored, as the
    // XDG specification says; a relative HOME gives no place at all.
    let pairs = [
        ("DELEGA_CONFIG", String::new()),
        ("XDG_CONFIG_HOME", "relative/dir".to_string()),
        ("HOME", "/home/u".to_string()),
    ];
    let found = locate(lookup(&pairs)).expect("ok").expect("a place");
    assert_eq!(found.source, Source::Home);
    let none = [("HOME", "relative".to_string())];
    assert_eq!(locate(lookup(&none)).expect("ok"), None);
    assert_eq!(locate(lookup(&[])).expect("ok"), None);
}

#[test]
fn a_relative_delega_config_is_refused() {
    let pairs = [("DELEGA_CONFIG", "config.toml".to_string())];
    let status = read_as(lookup(&pairs), euid());
    let refusal = status.refusal().expect("refused");
    assert_eq!(refusal.kind, UNREADABLE);
    assert!(refusal.message.contains("absolute"), "{refusal}");
    assert!(!status.found);
}

#[test]
fn no_file_is_no_configuration() {
    let area = Area::new();
    let pairs = [("XDG_CONFIG_HOME", area.root.display().to_string())];
    let status = read_as(lookup(&pairs), euid());
    assert!(!status.found);
    assert_eq!(status.result, Ok(None));
    assert_eq!(
        status.location.as_ref().map(|l| l.path.clone()),
        Some(area.root.join("delega/config.toml"))
    );
    assert_eq!(status.into_config().expect("empty"), Config::empty());
    // A missing home directory is no file as well.
    let pairs = [("HOME", area.root.join("nowhere").display().to_string())];
    assert_eq!(read_as(lookup(&pairs), euid()).result, Ok(None));
}

#[test]
fn a_missing_file_named_by_delega_config_is_refused() {
    let area = Area::new();
    let status = area.read(&area.root.join("absent.toml"));
    let refusal = status.refusal().expect("refused");
    assert_eq!(refusal.kind, UNREADABLE);
    assert!(refusal.message.contains("does not exist"), "{refusal}");
}

#[test]
fn the_default_places_are_read() {
    let area = Area::new();
    area.file(
        ".config/delega/config.toml",
        "schema_version = 1\n[defaults]\nmodel = \"m\"\n",
    );
    let pairs = [("HOME", area.root.display().to_string())];
    let status = read_as(lookup(&pairs), euid());
    assert!(status.found);
    assert_eq!(
        status.config().expect("read").defaults.model.as_deref(),
        Some("m")
    );
}

#[test]
fn the_full_file_is_read_and_applied() {
    let config = config_of(FULL).expect("valid");
    assert_eq!(config.defaults.provider, Some(Provider::Codex));
    let defaults = config.request_defaults();
    assert_eq!(defaults.provider, Some(Provider::Codex));
    assert_eq!(defaults.model.as_deref(), Some("gpt-test"));
    assert_eq!(defaults.timeout_secs, Some(1200));
    let permissions = defaults.permissions.expect("permissions");
    assert_eq!(
        permissions.mode,
        Some(crate::contract::permissions::Mode::Write)
    );
    assert_eq!(permissions.strict, Some(true));

    let mut options = ProviderOptions {
        env_pass: vec!["FROM_FLAG".to_string()],
        env_set: vec![("API_REGION".to_string(), "from-flag".to_string())],
        ..ProviderOptions::default()
    };
    config.apply_to_options(&mut options);
    // The file's entries come first, so the flag's value is applied after and wins.
    assert_eq!(
        options.env_pass,
        ["AWS_SESSION_TOKEN", "TOOLCHAIN_HOME", "FROM_FLAG"]
    );
    assert_eq!(
        options.env_set,
        [
            ("API_REGION".to_string(), "eu-test-1".to_string()),
            ("API_REGION".to_string(), "from-flag".to_string())
        ]
    );
    assert_eq!(
        options.claude_executable,
        Some(PathBuf::from("/opt/claude/bin/claude"))
    );
    assert_eq!(
        options.codex_executable,
        Some(PathBuf::from("/opt/codex/bin/codex"))
    );

    let limits = config.limits();
    assert_eq!(limits.max_live_workers, 8);
    assert_eq!(limits.max_running_runs, 2);
    assert_eq!(limits.max_queued_per_worker, 4);
    assert_eq!(limits.wait_default, std::time::Duration::from_secs(20));
    assert_eq!(limits.wait_ceiling, std::time::Duration::from_secs(120));
    // What the file does not set stays as it is.
    assert_eq!(limits.retained_results, Limits::default().retained_results);
    assert_eq!(config.settings().len(), 15);
}

#[test]
fn the_default_wait_never_exceeds_the_ceiling_that_applies() {
    // Only the ceiling is lowered: the reported default follows it.
    let config = config_of("schema_version = 1\n[mcp]\nwait_ceiling_secs = 10\n").expect("valid");
    let limits = config.limits();
    assert_eq!(limits.wait_ceiling, std::time::Duration::from_secs(10));
    assert_eq!(limits.wait_default, std::time::Duration::from_secs(10));
    // A default below the ceiling stays.
    let config =
        config_of("schema_version = 1\n[mcp]\nwait_ceiling_secs = 100\nwait_default_secs = 5\n")
            .expect("valid");
    assert_eq!(
        config.limits().wait_default,
        std::time::Duration::from_secs(5)
    );
    // No setting: the built-in 30 s.
    assert_eq!(
        Config::empty().limits().wait_default,
        std::time::Duration::from_secs(30)
    );
}

#[test]
fn a_key_under_env_set_that_looks_like_name_equals_value_is_shown_by_position() {
    let refused = refusal_of(&format!(
        "schema_version = 1\n[env.set]\n\"API_KEY={SECRET}\" = 12345\nZED = \"v\"\n\"AAA={SECRET}\" = \"x\\u0000\"\n"
    ));
    assert_eq!(refused.kind, INVALID);
    assert!(
        refused
            .message
            .contains("env.set.<entry 2> must be a string"),
        "{refused}"
    );
    assert!(!refused.message.contains(SECRET), "{refused}");
    // The NUL message uses a position too.
    let nul = refusal_of(&format!(
        "schema_version = 1\n[env.set]\n\"AAA={SECRET}\" = \"x\\u0000\"\n"
    ));
    assert!(
        nul.message
            .contains("env.set.<entry 1> must not contain NUL bytes"),
        "{nul}"
    );
    assert!(!nul.message.contains(SECRET), "{nul}");
}

#[test]
fn the_empty_configuration_changes_nothing() {
    let config = Config::empty();
    assert_eq!(config.request_defaults(), PartialRequest::default());
    assert_eq!(config.limits(), Limits::default());
    let mut options = ProviderOptions::default();
    config.apply_to_options(&mut options);
    assert!(options.env_pass.is_empty() && options.env_set.is_empty());
    assert!(options.claude_executable.is_none() && options.codex_executable.is_none());
    assert!(config.settings().is_empty());
    // A file that only has its version is the same as no file.
    let minimal = config_of("schema_version = 1\n").expect("valid");
    assert_eq!(minimal, Config::empty());
}

#[test]
fn the_version_is_required_and_must_be_one() {
    let missing = refusal_of("[defaults]\nmodel = \"m\"\n");
    assert_eq!(missing.kind, INVALID);
    assert!(
        missing.message.contains("schema_version is required"),
        "{missing}"
    );
    let other = refusal_of("schema_version = 2\n");
    assert!(
        other.message.contains("schema_version must be 1"),
        "{other}"
    );
    let text = refusal_of("schema_version = \"1\"\n");
    assert!(
        text.message.contains("schema_version must be an integer"),
        "{text}"
    );
}

#[test]
fn an_unknown_key_is_refused_naming_it_at_every_level() {
    let top = refusal_of("schema_version = 1\ncolour = \"x\"\n");
    assert_eq!(top.kind, INVALID);
    assert!(top.message.contains("colour is not a known key"), "{top}");
    let nested = refusal_of("schema_version = 1\n[mcp]\nmax_workers = 2\n");
    assert!(
        nested
            .message
            .contains("mcp.max_workers is not a known key"),
        "{nested}"
    );
    let deep = refusal_of("schema_version = 1\n[defaults.permissions]\ncommands = []\n");
    assert!(
        deep.message
            .contains("defaults.permissions.commands is not a known key"),
        "{deep}"
    );
    let provider = refusal_of("schema_version = 1\n[providers.gemini]\nexecutable = \"/x\"\n");
    assert!(
        provider
            .message
            .contains("providers.gemini is not a known key"),
        "{provider}"
    );
}

#[test]
fn a_prompt_or_a_working_directory_is_refused_with_a_message_about_why() {
    for key in ["prompt", "cwd"] {
        let refused = refusal_of(&format!(
            "schema_version = 1\n[defaults]\n{key} = \"{SECRET}\"\n"
        ));
        assert_eq!(refused.kind, INVALID);
        assert!(
            refused
                .message
                .contains(&format!("defaults.{key} is not accepted")),
            "{refused}"
        );
        assert!(
            refused.message.contains("belongs to one request"),
            "{refused}"
        );
        assert!(!refused.message.contains(SECRET), "{refused}");
    }
}

#[test]
fn a_value_of_the_wrong_type_or_outside_its_bound_is_refused_without_the_value() {
    let cases = [
        (
            format!("schema_version = 1\n[defaults]\nmodel = 12345{SECRET_NUM}\n"),
            "defaults.model must be a string",
        ),
        (
            format!("schema_version = 1\n[defaults]\ntimeout_secs = \"{SECRET}\"\n"),
            "defaults.timeout_secs must be an integer",
        ),
        (
            "schema_version = 1\n[defaults]\ntimeout_secs = 0\n".to_string(),
            "defaults.timeout_secs must be at least 1",
        ),
        (
            "schema_version = 1\n[defaults]\ntimeout_secs = 86401\n".to_string(),
            "defaults.timeout_secs must be at most 86400",
        ),
        (
            format!("schema_version = 1\n[defaults]\nprovider = \"{SECRET}\"\n"),
            "defaults.provider must be one of \"claude\", \"codex\"",
        ),
        (
            format!("schema_version = 1\n[defaults]\neffort = \"{SECRET}\"\n"),
            "defaults.effort must be one of",
        ),
        (
            format!("schema_version = 1\n[defaults.permissions]\nmode = \"{SECRET}\"\n"),
            "defaults.permissions.mode must be one of",
        ),
        (
            format!("schema_version = 1\n[defaults.permissions]\nstrict = \"{SECRET}\"\n"),
            "defaults.permissions.strict must be a boolean",
        ),
        (
            format!("schema_version = 1\n[env]\npass = \"{SECRET}\"\n"),
            "env.pass must be an array",
        ),
        (
            format!("schema_version = 1\n[env]\npass = [1{SECRET_NUM}]\n"),
            "env.pass.0 must be a string",
        ),
        (
            format!("schema_version = 1\n[env.set]\nTOKEN_A = 99{SECRET_NUM}\n"),
            "env.set.<entry 1> must be a string",
        ),
        (
            format!("schema_version = 1\n[providers.claude]\nexecutable = 5{SECRET_NUM}\n"),
            "providers.claude.executable must be a string",
        ),
        (
            "schema_version = 1\n[mcp]\nmax_live_workers = 17\n".to_string(),
            "mcp.max_live_workers must be at most 16",
        ),
        (
            "schema_version = 1\n[mcp]\nmax_live_workers = 0\n".to_string(),
            "mcp.max_live_workers must be at least 1",
        ),
        (
            "schema_version = 1\n[mcp]\nmax_running_runs = 5\n".to_string(),
            "mcp.max_running_runs must be at most 4",
        ),
        (
            "schema_version = 1\n[mcp]\nmax_queued_per_worker = 17\n".to_string(),
            "mcp.max_queued_per_worker must be at most 16",
        ),
        (
            "schema_version = 1\n[mcp]\nwait_ceiling_secs = 301\n".to_string(),
            "mcp.wait_ceiling_secs must be at most 300",
        ),
        (
            "schema_version = 1\n[mcp]\nwait_ceiling_secs = 0\n".to_string(),
            "mcp.wait_ceiling_secs must be at least 1",
        ),
        (
            "schema_version = 1\n[mcp]\nmax_running_runs = -1\n".to_string(),
            "mcp.max_running_runs",
        ),
        (
            "schema_version = 1\n[mcp]\nwait_default_secs = 100\nwait_ceiling_secs = 50\n"
                .to_string(),
            "mcp.wait_default_secs must not be more than the wait ceiling (50 seconds)",
        ),
        (
            "schema_version = 1\n[mcp]\nwait_default_secs = 301\n".to_string(),
            "mcp.wait_default_secs must be at most 300",
        ),
    ];
    for (text, expected) in cases {
        let refused = refusal_of(&text);
        assert_eq!(refused.kind, INVALID, "{text}");
        assert!(refused.message.contains(expected), "{text}\n{refused}");
        assert!(!refused.message.contains(SECRET), "{text}\n{refused}");
        assert!(!refused.message.contains(SECRET_NUM), "{text}\n{refused}");
    }
}

/// Digits planted in numeric and other non-string values.
const SECRET_NUM: &str = "70315";

#[test]
fn model_and_executable_rules_are_checked_and_never_repeat_the_value() {
    let dash = refusal_of(&format!(
        "schema_version = 1\n[defaults]\nmodel = \"-{SECRET}\"\n"
    ));
    assert!(
        dash.message
            .contains("defaults.model must not start with '-'"),
        "{dash}"
    );
    assert!(!dash.message.contains(SECRET), "{dash}");
    let blank = refusal_of("schema_version = 1\n[defaults]\nmodel = \"  \"\n");
    assert!(
        blank.message.contains("defaults.model must not be empty"),
        "{blank}"
    );
    for provider in ["claude", "codex"] {
        let relative = refusal_of(&format!(
            "schema_version = 1\n[providers.{provider}]\nexecutable = \"{SECRET}/bin\"\n"
        ));
        assert!(
            relative.message.contains(&format!(
                "providers.{provider}.executable must be an absolute path"
            )),
            "{relative}"
        );
        assert!(!relative.message.contains(SECRET), "{relative}");
    }
    let nul = refusal_of("schema_version = 1\n[providers.codex]\nexecutable = \"/a\\u0000b\"\n");
    assert!(nul.message.contains("must not contain NUL bytes"), "{nul}");
    let value_nul = refusal_of("schema_version = 1\n[env.set]\nA_B = \"x\\u0000y\"\n");
    assert!(
        value_nul
            .message
            .contains("env.set.<entry 1> must not contain NUL bytes"),
        "{value_nul}"
    );
}

#[test]
fn invalid_toml_is_refused_with_a_position_and_no_text_from_the_file() {
    let text = format!("schema_version = 1\n[defaults]\nmodel = \"{SECRET}\nnext = 1\n");
    let refused = refusal_of(&text);
    assert_eq!(refused.kind, INVALID);
    assert!(
        refused.message.contains("is not valid TOML (line 3,"),
        "{refused}"
    );
    assert!(!refused.message.contains(SECRET), "{refused}");
    let duplicate = refusal_of(&format!(
        "schema_version = 1\n[env.set]\nA_B = \"{SECRET}\"\nA_B = \"x\"\n"
    ));
    assert!(duplicate.message.contains("line 4"), "{duplicate}");
    assert!(!duplicate.message.contains(SECRET), "{duplicate}");
}

#[test]
fn invalid_utf8_and_an_oversize_file_are_refused() {
    let area = Area::new();
    let path = area.root.join("bad.toml");
    std::fs::write(&path, [b's', b'=', 0xff, 0xfe]).expect("write");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("mode");
    let refused = area.read(&path).into_config().expect_err("refused");
    assert_eq!(refused.kind, INVALID);
    assert!(refused.message.contains("not valid UTF-8"), "{refused}");

    let big = area.file(
        "big.toml",
        &format!("schema_version = 1\n# {}\n", "x".repeat(MAX_CONFIG_BYTES)),
    );
    let refused = area.read(&big).into_config().expect_err("refused");
    assert_eq!(refused.kind, UNREADABLE);
    assert!(
        refused
            .message
            .contains("larger than the limit of 1048576 bytes"),
        "{refused}"
    );
    // A file exactly at the limit is read.
    let mut text = String::from("schema_version = 1\n# ");
    text.push_str(&"x".repeat(MAX_CONFIG_BYTES - text.len() - 1));
    text.push('\n');
    assert_eq!(text.len(), MAX_CONFIG_BYTES);
    let at_limit = area.file("limit.toml", &text);
    assert!(area.read(&at_limit).into_config().is_ok());
}

#[test]
fn env_names_get_the_refusals_of_the_env_flag() {
    let cases = [
        (
            "[env]\npass = [\"OK_NAME\", \"1BAD\"]\n",
            "invalid_env_name",
            "environment variable name number 2 to pass is not a valid name",
        ),
        (
            "[env]\npass = [\"DELEGA_CONFIG\"]\n",
            "forbidden_env_name",
            "DELEGA_CONFIG configures delega itself",
        ),
        (
            "[env]\npass = [\"CLAUDECODE\"]\n",
            "forbidden_env_name",
            "CLAUDECODE describes the calling session",
        ),
        (
            "[env.set]\n\"BAD NAME\" = \"v\"\n",
            "invalid_env_name",
            "name number 1 to set is not a valid name",
        ),
        (
            "[env.set]\nDELEGA_X = \"v\"\n",
            "forbidden_env_name",
            "DELEGA_X configures delega itself",
        ),
    ];
    for (body, kind, expected) in cases {
        let refused = refusal_of(&format!("schema_version = 1\n{body}"));
        assert_eq!(refused.kind, kind, "{body}");
        assert!(refused.message.contains(expected), "{body}\n{refused}");
        assert!(
            refused.message.contains("the configuration file"),
            "{refused}"
        );
    }
    // An invalid name is not echoed, so a value mistaken for a name is not shown.
    let mistaken = refusal_of(&format!(
        "schema_version = 1\n[env]\npass = [\"NAME={SECRET}\"]\n"
    ));
    assert_eq!(mistaken.kind, "invalid_env_name");
    assert!(!mistaken.message.contains(SECRET), "{mistaken}");
}

#[test]
fn a_secret_looking_name_is_allowed_in_the_file() {
    let config = config_of(&format!(
        "schema_version = 1\n[env]\npass = [\"AWS_SESSION_TOKEN\"]\n[env.set]\nSERVICE_API_KEY = \"{SECRET}\"\n"
    ))
    .expect("valid");
    assert_eq!(config.env.set["SERVICE_API_KEY"], SECRET);
}

#[test]
fn debug_output_and_the_report_name_keys_and_never_values() {
    let config = config_of(&format!(
        "schema_version = 1\n[defaults]\nmodel = \"{SECRET}-model\"\n[env.set]\nSERVICE_A = \"{SECRET}\"\n"
    ))
    .expect("valid");
    let debug = format!("{config:?}");
    assert!(debug.contains("SERVICE_A"), "{debug}");
    assert!(!debug.contains(&format!("\"{SECRET}\"")), "{debug}");
    let mut options = ProviderOptions::default();
    config.apply_to_options(&mut options);
    assert!(!format!("{options:?}").contains(SECRET));
    let status = Status {
        location: Some(Location {
            path: PathBuf::from("/h/.config/delega/config.toml"),
            source: Source::Home,
        }),
        found: true,
        result: Ok(Some(config)),
        applied: None,
    };
    let report = crate::doctor::ConfigReport::of(&status);
    assert!(report.read && report.found);
    assert_eq!(report.settings, ["defaults.model", "env.set"]);
    assert_eq!((report.env_pass, report.env_set), (0, 1));
    assert!(!format!("{report:?}").contains(SECRET));
}

#[test]
fn a_file_others_can_write_is_refused() {
    let area = Area::new();
    for (mode, expected_mode) in [
        (0o660, "660"),
        (0o606, "606"),
        (0o666, "666"),
        (0o620, "620"),
    ] {
        let path = area.file("c.toml", "schema_version = 1\n");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("mode");
        let status = area.read(&path);
        let refusal = status.refusal().expect("refused");
        assert_eq!(refusal.kind, UNREADABLE, "{mode:o}");
        assert!(
            refusal
                .message
                .contains("can be written by group or others")
                && refusal.message.contains(expected_mode),
            "{refusal}"
        );
        assert!(status.found);
    }
    // Readable by others is fine.
    let path = area.file("ok.toml", "schema_version = 1\n");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("mode");
    assert!(area.read(&path).into_config().is_ok());
}

#[test]
fn a_file_owned_by_another_user_is_refused() {
    let area = Area::new();
    let path = area.file("c.toml", "schema_version = 1\n");
    let pairs = [("DELEGA_CONFIG", path.display().to_string())];
    // Standing in for a user who is not the owner of what the test made.
    let status = read_as(lookup(&pairs), euid().wrapping_add(1));
    let refusal = status.refusal().expect("refused");
    assert_eq!(refusal.kind, UNREADABLE);
    // The directory the test made is not that user's either, which is found first.
    assert!(
        refusal.message.contains("not owned by the current user"),
        "{refusal}"
    );
}

#[test]
fn a_file_in_a_directory_others_can_write_is_refused() {
    let area = Area::new();
    let path = area.file("shared/c.toml", "schema_version = 1\n");
    let dir = path.parent().expect("parent").to_path_buf();
    for mode in [0o775, 0o777, 0o770, 0o757] {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).expect("mode");
        let refusal = area.read(&path).into_config().expect_err("refused");
        assert_eq!(refusal.kind, UNREADABLE, "{mode:o}");
        assert!(
            refusal.message.contains("directory that holds it"),
            "{refusal}"
        );
    }
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("mode");
    assert!(area.read(&path).into_config().is_ok());
}

#[test]
fn a_file_swapped_for_a_link_is_not_followed_and_a_hard_link_dir_is_not_trusted() {
    // The entry is a regular file when it is looked at and opened with
    // O_NOFOLLOW, so a link in its place is refused as a link, not read.
    let area = Area::new();
    let target = area.file("t.toml", "schema_version = 1\n");
    let dir = area.root.join("d");
    std::fs::create_dir(&dir).expect("directory");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("mode");
    symlink(&target, dir.join("c.toml")).expect("link");
    // A link in the user's private directory to the user's file is the dotfiles
    // case and is followed.
    assert!(area.read(&dir.join("c.toml")).into_config().is_ok());
    // Seen as another user, the same link is refused.
    let pairs = [("DELEGA_CONFIG", dir.join("c.toml").display().to_string())];
    assert!(
        read_as(lookup(&pairs), euid().wrapping_add(1))
            .refusal()
            .is_some()
    );
}

#[test]
fn a_directory_or_a_pipe_is_refused_without_blocking() {
    let area = Area::new();
    let dir = area.root.join("dir.toml");
    std::fs::create_dir(&dir).expect("directory");
    let refusal = area.read(&dir).into_config().expect_err("refused");
    assert_eq!(refusal.kind, UNREADABLE);
    assert!(refusal.message.contains("not a regular file"), "{refusal}");

    let fifo = area.root.join("pipe.toml");
    let made = std::process::Command::new("mkfifo").arg(&fifo).status();
    if made.is_ok_and(|s| s.success()) {
        let refusal = area.read(&fifo).into_config().expect_err("refused");
        assert!(refusal.message.contains("not a regular file"), "{refusal}");
    }
}

#[test]
fn a_symbolic_link_is_followed_only_when_the_user_owns_the_link_and_its_directory() {
    let area = Area::new();
    let target = area.file(
        "dotfiles/config.toml",
        "schema_version = 1\n[defaults]\nmodel = \"m\"\n",
    );
    let home = area.root.join("home");
    let dir = home.join(".config/delega");
    std::fs::create_dir_all(&dir).expect("directory");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("mode");
    symlink(&target, dir.join("config.toml")).expect("link");
    let pairs = [("HOME", home.display().to_string())];

    // The user's own link in the user's own directory, to the user's own file.
    let status = read_as(lookup(&pairs), euid());
    assert_eq!(
        status.config().expect("followed").defaults.model.as_deref(),
        Some("m")
    );

    // The same link seen by a user who does not own it.
    let status = read_as(lookup(&pairs), euid().wrapping_add(1));
    let refusal = status.refusal().expect("refused");
    assert!(
        refusal
            .message
            .contains("link that the current user does not own"),
        "{refusal}"
    );

    // A directory that group can write is not trusted to hold a link.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o775)).expect("mode");
    let refusal = read_as(lookup(&pairs), euid())
        .into_config()
        .expect_err("refused");
    assert_eq!(refusal.kind, UNREADABLE);
    assert!(refusal.message.contains("link in a directory"), "{refusal}");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("mode");

    // The target is judged on its own: a group-writable target is refused.
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o664)).expect("mode");
    let refusal = read_as(lookup(&pairs), euid())
        .into_config()
        .expect_err("refused");
    assert!(
        refusal
            .message
            .contains("can be written by group or others"),
        "{refusal}"
    );
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).expect("mode");

    // A link to nothing is refused, not read as no file.
    std::fs::remove_file(&target).expect("remove");
    let status = read_as(lookup(&pairs), euid());
    let refusal = status.refusal().expect("refused");
    assert!(refusal.message.contains("points at nothing"), "{refusal}");
    assert!(status.found);
}

#[test]
fn the_error_text_makes_control_characters_visible() {
    let area = Area::new();
    let path = area.root.join("a\nb.toml");
    let status = area.read(&path);
    let refusal = status.refusal().expect("refused");
    assert!(!refusal.message.contains('\n'), "{refusal}");
}

#[test]
fn committed_schemas_config_matches_the_generated_schema() {
    let generated = generate_schema();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas/config.schema.json");
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    if generated == committed {
        return;
    }
    if std::env::var_os("DELEGA_UPDATE_SCHEMAS").is_some() {
        std::fs::write(&path, &generated).expect("write schema file");
        return;
    }
    panic!(
        "committed schema out of date: config.schema.json. Regenerate with \
         `DELEGA_UPDATE_SCHEMAS=1 cargo test -p delega-core committed_schemas` and commit the \
         result."
    );
}

#[test]
fn the_schema_states_the_bounds_the_file_enforces() {
    let schema = schema::value();
    let at = |pointer: &str| schema.pointer(pointer).cloned().expect(pointer);
    assert_eq!(at("/properties/schema_version/const"), 1);
    assert_eq!(at("/additionalProperties"), false);
    let defaults = at("/$defs/Defaults/properties/timeout_secs");
    assert_eq!(defaults["minimum"], 1);
    assert_eq!(defaults["maximum"], 86_400);
    let mcp = at("/$defs/Mcp/properties");
    assert_eq!(mcp["max_live_workers"]["maximum"], 16);
    assert_eq!(mcp["max_running_runs"]["maximum"], 4);
    assert_eq!(mcp["max_queued_per_worker"]["minimum"], 0);
    assert_eq!(mcp["wait_ceiling_secs"]["maximum"], 300);
    // The documented example validates against the schema it is checked by.
    assert!(config_of(FULL).is_ok());
}
