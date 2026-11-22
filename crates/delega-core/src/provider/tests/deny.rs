//! The deny rules of the user's and the project's Claude Code settings: where
//! they are read from, what is taken from the files, how rules are anchored and
//! bounded, what a run and a dry run report, and enforcement through the fake
//! Claude Code, which models assumed CLI behaviour.
//!
//! Every test writes its own settings files under a temporary home, a
//! temporary configuration directory and a temporary working directory.

use std::ffi::OsString;
use std::path::Path;

use serde_json::{Value, json};

use super::{Rig, run_checked};
use crate::contract::outcome::Outcome;
use crate::contract::permissions::Mode;
use crate::contract::request::Provider;
use crate::contract::result::RunResult;
use crate::provider::limits::{
    DRY_RUN_DENY_RULE_CHARS, DRY_RUN_DENY_RULES_SHOWN, MAX_DENY_RULE_CHARS, MAX_DENY_RULES,
    MAX_DENY_RULES_PER_FILE, SETTINGS_FILE_BYTES,
};
use crate::provider::{DryRun, dry_run};
use crate::test_spawn::Guarded;

/// Write `content` to `path`, creating the directory above it.
fn put(path: &Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn deny_file(rules: &[&str]) -> String {
    json!({ "permissions": { "deny": rules } }).to_string()
}

fn user_file(rig: &Rig) -> std::path::PathBuf {
    rig.config.join("settings.json")
}

fn project_file(rig: &Rig, name: &str) -> std::path::PathBuf {
    rig.work.join(".claude").join(name)
}

fn dry(rig: &Rig) -> DryRun {
    dry_run(
        &rig.request(Provider::Claude, "p"),
        &rig.options(Provider::Claude),
    )
    .expect("dry run")
}

fn settings_arg(args: &[String]) -> Value {
    let at = args.iter().position(|a| a == "--settings").unwrap();
    serde_json::from_str(&args[at + 1]).unwrap()
}

/// The notes that mention deny rules.
fn deny_notes(notes: &[String]) -> Vec<&String> {
    notes.iter().filter(|n| n.contains("deny rules")).collect()
}

// ---- location ----

#[test]
fn the_user_file_is_read_from_the_config_dir_the_worker_will_get_and_not_from_home() {
    let rig = Rig::new();
    put(&user_file(&rig), &deny_file(&["Bash(from-config-dir)"]));
    // This one would be the user file only if the config dir were not set.
    put(
        &rig.home.join(".claude/settings.json"),
        &deny_file(&["Bash(from-home)"]),
    );
    let plan = dry(&rig);
    assert_eq!(plan.deny_rules.user_deny_rules, ["Bash(from-config-dir)"]);
    assert_eq!(plan.deny_rules.files.len(), 1);
    assert_eq!(
        plan.deny_rules.files[0].path,
        user_file(&rig).display().to_string()
    );
}

#[test]
fn without_a_config_dir_the_user_file_is_dot_claude_under_the_workers_home() {
    let rig = Rig::new();
    let mut options = rig.options(Provider::Claude);
    options.env_set.retain(|(n, _)| n != "CLAUDE_CONFIG_DIR");
    put(
        &rig.home.join(".claude/settings.json"),
        &deny_file(&["Bash(from-home)"]),
    );
    put(&user_file(&rig), &deny_file(&["Bash(from-config-dir)"]));
    let plan = dry_run(&rig.request(Provider::Claude, "p"), &options).unwrap();
    assert_eq!(plan.deny_rules.user_deny_rules, ["Bash(from-home)"]);
}

#[test]
fn a_config_dir_in_the_parent_environment_counts_because_the_worker_receives_it() {
    let rig = Rig::new();
    let mut options = rig.options(Provider::Claude);
    options.env_set.retain(|(n, _)| n != "CLAUDE_CONFIG_DIR");
    options.parent_env.as_mut().unwrap().push((
        OsString::from("CLAUDE_CONFIG_DIR"),
        rig.config.clone().into(),
    ));
    put(&user_file(&rig), &deny_file(&["Bash(parent-config)"]));
    let plan = dry_run(&rig.request(Provider::Claude, "p"), &options).unwrap();
    assert_eq!(plan.deny_rules.user_deny_rules, ["Bash(parent-config)"]);
}

#[test]
fn the_home_comes_from_the_workers_environment_not_from_the_test_process() {
    let rig = Rig::new();
    let mut options = rig.options(Provider::Claude);
    options.env_set.retain(|(n, _)| n != "CLAUDE_CONFIG_DIR");
    // A parent environment without HOME: no user file can be located, whatever
    // HOME this process has.
    options
        .parent_env
        .as_mut()
        .unwrap()
        .retain(|(n, _)| n != "HOME");
    let plan = dry_run(&rig.request(Provider::Claude, "p"), &options).unwrap();
    assert!(plan.deny_rules.files.is_empty());
    assert!(
        plan.notes
            .iter()
            .any(|n| n.contains("neither CLAUDE_CONFIG_DIR nor HOME")),
        "{:?}",
        plan.notes
    );
}

#[test]
fn project_and_local_project_files_are_read_from_the_working_directory_in_order() {
    let rig = Rig::new();
    put(&user_file(&rig), &deny_file(&["Bash(u)"]));
    put(
        &project_file(&rig, "settings.json"),
        &deny_file(&["Bash(p)"]),
    );
    put(
        &project_file(&rig, "settings.local.json"),
        &deny_file(&["Bash(l)"]),
    );
    // A settings file in the parent of the working directory is not read.
    put(
        &rig.work.parent().unwrap().join(".claude/settings.json"),
        &deny_file(&["Bash(parent)"]),
    );
    let plan = dry(&rig);
    assert_eq!(
        plan.deny_rules.user_deny_rules,
        ["Bash(u)", "Bash(p)", "Bash(l)"]
    );
    let paths: Vec<&str> = plan
        .deny_rules
        .files
        .iter()
        .map(|f| f.path.as_str())
        .collect();
    assert_eq!(
        paths,
        [
            user_file(&rig).to_str().unwrap(),
            project_file(&rig, "settings.json").to_str().unwrap(),
            project_file(&rig, "settings.local.json").to_str().unwrap(),
        ]
    );
}

#[test]
fn a_symlinked_user_file_to_a_regular_file_is_followed() {
    let rig = Rig::new();
    let real = rig.home.join("dotfiles/claude-settings.json");
    put(&real, &deny_file(&["Bash(via-link)"]));
    std::os::unix::fs::symlink(&real, user_file(&rig)).unwrap();
    assert_eq!(dry(&rig).deny_rules.user_deny_rules, ["Bash(via-link)"]);
}

// ---- no files ----

#[test]
fn with_no_settings_files_nothing_is_added_and_the_note_says_none_were_found() {
    let rig = Rig::new();
    for mode in [Mode::Read, Mode::Write, Mode::Build, Mode::Full] {
        let mut request = rig.request(Provider::Claude, "p");
        request.permissions.mode = mode;
        let plan = dry_run(&request, &rig.options(Provider::Claude)).unwrap();
        assert!(plan.deny_rules.files.is_empty());
        assert_eq!(plan.deny_rules.applied, 0);
        assert!(settings_arg(&plan.args).get("permissions").is_none());
        assert!(plan.mapping.settings.contains(&"denyRules=0".to_string()));
        let notes = deny_notes(&plan.notes);
        assert!(
            notes
                .iter()
                .any(|n| n.contains("no deny rules were found") && n.contains("none were applied")),
            "{notes:?}"
        );
    }
}

// ---- parsing ----

#[test]
fn only_the_deny_list_is_taken_and_a_missing_key_gives_no_rules() {
    let rig = Rig::new();
    for body in [
        "{}",
        r#"{"permissions":{}}"#,
        r#"{"permissions":{"allow":["Bash(x)"],"ask":["Bash(y)"]}}"#,
        r#"{"model":"x","hooks":{}}"#,
    ] {
        put(&user_file(&rig), body);
        let plan = dry(&rig);
        assert_eq!(plan.deny_rules.applied, 0, "{body}");
        assert_eq!(plan.deny_rules.files.len(), 1, "{body}");
        assert_eq!(plan.deny_rules.files[0].rules, 0);
    }
}

#[test]
fn a_file_that_exists_and_cannot_be_used_refuses_naming_the_file_and_not_its_content() {
    const CANARY: &str = "CANARY-SETTINGS-CONTENT";
    let rig = Rig::new();
    let bad: Vec<(String, &str)> = vec![
        (format!("{{ {CANARY}"), "not valid JSON"),
        (format!("[\"{CANARY}\"]"), "top level"),
        (
            format!(r#"{{"permissions":"{CANARY}"}}"#),
            "`permissions` is not an object",
        ),
        (
            format!(r#"{{"permissions":["{CANARY}"]}}"#),
            "`permissions` is not an object",
        ),
        (
            format!(r#"{{"permissions":{{"deny":"{CANARY}"}}}}"#),
            "`permissions.deny` is not a list",
        ),
        (
            format!(r#"{{"permissions":{{"deny":null,"x":"{CANARY}"}}}}"#),
            "`permissions.deny` is not a list",
        ),
        (
            format!(r#"{{"permissions":{{"deny":["Bash(x)",7,"{CANARY}"]}}}}"#),
            "entry 2 is not a string",
        ),
        (
            format!(r#"{{"permissions":{{"deny":["  ","{CANARY}"]}}}}"#),
            "entry 1 is empty",
        ),
        (
            format!("{{\"permissions\":{{\"deny\":[\"a\\nb{CANARY}\"]}}}}"),
            "control character",
        ),
    ];
    for (body, reason) in &bad {
        for path in [
            user_file(&rig),
            project_file(&rig, "settings.json"),
            project_file(&rig, "settings.local.json"),
        ] {
            put(&path, body);
            let error = dry_run(
                &rig.request(Provider::Claude, "p"),
                &rig.options(Provider::Claude),
            )
            .unwrap_err();
            assert_eq!(error.kind, "settings_unreadable", "{body:?}");
            assert!(
                error.message.contains(path.to_str().unwrap()),
                "{}",
                error.message
            );
            assert!(
                error.message.contains(reason),
                "{reason}: {}",
                error.message
            );
            assert!(!error.message.contains(CANARY), "{}", error.message);
            std::fs::remove_file(&path).unwrap();
        }
    }
}

#[test]
fn a_settings_file_that_is_not_a_regular_file_is_refused_before_it_is_opened() {
    let rig = Rig::new();
    // A directory stands in for a device or pipe, which are refused by the same
    // check and would block or never end if opened.
    std::fs::create_dir_all(project_file(&rig, "settings.json")).unwrap();
    let error = dry_run(
        &rig.request(Provider::Claude, "p"),
        &rig.options(Provider::Claude),
    )
    .unwrap_err();
    assert_eq!(error.kind, "settings_unreadable");
    assert!(error.message.contains("not a regular file"), "{error}");
}

#[test]
fn a_settings_file_over_the_size_limit_is_refused_and_one_at_the_limit_is_read() {
    let rig = Rig::new();
    let path = user_file(&rig);
    let pad = |total: usize| {
        let head = r#"{"permissions":{"deny":["Bash(x)"]},"pad":""#;
        let tail = r#""}"#;
        format!(
            "{head}{}{tail}",
            " ".repeat(total - head.len() - tail.len())
        )
    };
    put(&path, &pad(SETTINGS_FILE_BYTES));
    assert_eq!(dry(&rig).deny_rules.user_deny_rules, ["Bash(x)"]);
    put(&path, &pad(SETTINGS_FILE_BYTES + 1));
    let error = dry_run(
        &rig.request(Provider::Claude, "p"),
        &rig.options(Provider::Claude),
    )
    .unwrap_err();
    assert_eq!(error.kind, "settings_unreadable");
    assert!(
        error.message.contains(&SETTINGS_FILE_BYTES.to_string()),
        "{error}"
    );
}

#[tokio::test]
async fn a_malformed_settings_file_refuses_the_run_before_the_provider_starts() {
    let rig = Rig::new();
    put(&project_file(&rig, "settings.json"), "{ not json");
    let result = run_checked(
        rig.request(Provider::Claude, "success"),
        rig.options(Provider::Claude),
    )
    .await;
    assert_eq!(result.outcome, Outcome::Refused, "{result:#?}");
    let error = result.error.expect("error");
    assert_eq!(error.kind, "settings_unreadable");
    assert!(result.timing.started_at.is_none());
    assert!(rig.pid("pid").is_none(), "the provider must not start");
}

// ---- anchoring, deduplication, limits ----

#[test]
fn every_rule_is_passed_as_written_from_each_file_and_a_one_slash_rule_gains_an_absolute_one() {
    let rig = Rig::new();
    let sub = rig.work.join("sub");
    std::fs::create_dir(&sub).unwrap();
    let same = [
        "Read(private/**)",
        "Read(./a/b)",
        "Read(**/x)",
        "Edit(~/x)",
        "Read(//abs/**)",
        "Bash(rm -rf private)",
        "Bash(/usr/bin/x)",
    ];
    let mut user = same.to_vec();
    user.push("Edit(/out/u)");
    let mut project = same.to_vec();
    project.push("Edit(/out/p)");
    let mut local = same.to_vec();
    local.push("Read(/out/l)");
    put(&user_file(&rig), &deny_file(&user));
    put(&sub.join(".claude/settings.json"), &deny_file(&project));
    put(&sub.join(".claude/settings.local.json"), &deny_file(&local));
    let mut request = rig.request(Provider::Claude, "p");
    request.cwd = Some(sub.clone());
    let plan = dry_run(&request, &rig.options(Provider::Claude)).unwrap();
    // The user file is in the config dir, which is where a one-slash rule in it
    // points; a project file's rules point at the directory holding `.claude`.
    let config = std::fs::canonicalize(&rig.config).unwrap();
    let mut want: Vec<String> = same.iter().map(ToString::to_string).collect();
    want.push("Edit(/out/u)".into());
    want.push(format!("Edit(/{}/out/u)", config.display()));
    want.push("Edit(/out/p)".into());
    want.push(format!("Edit(/{}/out/p)", sub.display()));
    want.push("Read(/out/l)".into());
    want.push(format!("Read(/{}/out/l)", sub.display()));
    assert_eq!(plan.deny_rules.user_deny_rules, want);
    assert!(plan.deny_rules.user_deny_rules[8].starts_with("Edit(//"));
}

#[test]
fn duplicate_rules_across_and_within_files_are_passed_once() {
    let rig = Rig::new();
    put(
        &user_file(&rig),
        &deny_file(&["Bash(a)", "Bash(a)", "Bash(b)"]),
    );
    put(
        &project_file(&rig, "settings.json"),
        &deny_file(&["Bash(b)", "Bash(c)"]),
    );
    let plan = dry(&rig);
    assert_eq!(
        plan.deny_rules.user_deny_rules,
        ["Bash(a)", "Bash(b)", "Bash(c)"]
    );
    assert_eq!(plan.deny_rules.applied, 3);
    // The file counts are what the files hold.
    assert_eq!(plan.deny_rules.files[0].rules, 3);
    assert_eq!(plan.deny_rules.files[1].rules, 2);
}

#[test]
fn counts_and_lengths_over_a_bound_refuse_instead_of_truncating() {
    let rig = Rig::new();
    let many = |n: usize, tag: &str| -> String {
        let rules: Vec<String> = (0..n).map(|i| format!("Bash({tag}{i})")).collect();
        json!({ "permissions": { "deny": rules } }).to_string()
    };
    let refuse = |rig: &Rig| {
        dry_run(
            &rig.request(Provider::Claude, "p"),
            &rig.options(Provider::Claude),
        )
        .unwrap_err()
    };

    put(&user_file(&rig), &many(MAX_DENY_RULES_PER_FILE, "u"));
    assert_eq!(dry(&rig).deny_rules.applied, MAX_DENY_RULES_PER_FILE);
    put(&user_file(&rig), &many(MAX_DENY_RULES_PER_FILE + 1, "u"));
    let error = refuse(&rig);
    assert_eq!(error.kind, "deny_rules_over_limit");
    assert!(
        error
            .message
            .contains(&user_file(&rig).display().to_string())
    );

    // Three files under the per-file bound that together pass the total.
    let each = MAX_DENY_RULES / 3 + 1;
    put(&user_file(&rig), &many(each, "u"));
    put(&project_file(&rig, "settings.json"), &many(each, "p"));
    put(&project_file(&rig, "settings.local.json"), &many(each, "l"));
    let error = refuse(&rig);
    assert_eq!(error.kind, "deny_rules_over_limit");
    assert!(
        error.message.contains(&MAX_DENY_RULES.to_string()),
        "{error}"
    );

    // The same lists with duplicates are under the total, so they are accepted.
    put(&project_file(&rig, "settings.json"), &many(each, "u"));
    put(&project_file(&rig, "settings.local.json"), &many(each, "u"));
    assert_eq!(dry(&rig).deny_rules.applied, each);
    for path in [
        project_file(&rig, "settings.json"),
        project_file(&rig, "settings.local.json"),
    ] {
        std::fs::remove_file(path).unwrap();
    }

    // One rule at the length bound is fine and one character more is refused.
    let rule = |len: usize| format!("Bash({})", "x".repeat(len - "Bash()".len()));
    put(
        &user_file(&rig),
        &json!({ "permissions": { "deny": [rule(MAX_DENY_RULE_CHARS)] } }).to_string(),
    );
    assert_eq!(dry(&rig).deny_rules.applied, 1);
    put(
        &user_file(&rig),
        &json!({ "permissions": { "deny": [rule(MAX_DENY_RULE_CHARS + 1)] } }).to_string(),
    );
    let error = refuse(&rig);
    assert_eq!(error.kind, "deny_rules_over_limit");
    assert!(!error.message.contains("xxxx"), "{error}");
}

// ---- notes and dry run ----

#[test]
fn notes_name_the_files_and_counts_and_say_what_else_is_not_loaded() {
    let rig = Rig::new();
    put(&user_file(&rig), &deny_file(&["Bash(a)", "Read(b)"]));
    put(
        &project_file(&rig, "settings.json"),
        &deny_file(&["Bash(c)"]),
    );
    for mode in [Mode::Read, Mode::Write, Mode::Build, Mode::Full] {
        let mut request = rig.request(Provider::Claude, "p");
        request.permissions.mode = mode;
        let plan = dry_run(&request, &rig.options(Provider::Claude)).unwrap();
        let other = plan
            .notes
            .iter()
            .find(|n| n.contains("other than their deny rules were not applied"))
            .expect("the not-loaded note");
        for what in [
            "hooks",
            "env",
            "apiKeyHelper",
            "plugins",
            "skills",
            "MCP servers",
            "instructions",
        ] {
            assert!(other.contains(what), "{what}: {other}");
        }
        let applied = plan
            .notes
            .iter()
            .find(|n| n.starts_with("3 deny rules from the user's and the project's"))
            .unwrap_or_else(|| panic!("{:?}", plan.notes));
        assert!(applied.contains(&format!("{} (2)", user_file(&rig).display())));
        assert!(applied.contains(&format!(
            "{} (1)",
            project_file(&rig, "settings.json").display()
        )));
        // The note names paths and counts, never a rule.
        for rule in ["Bash(a)", "Read(b)", "Bash(c)"] {
            assert!(!applied.contains(rule), "{applied}");
        }
        assert!(
            !plan
                .notes
                .iter()
                .any(|n| n.contains("including their deny rules, were not applied"))
        );
    }
}

#[test]
fn a_dry_run_lists_files_counts_and_the_rules_only_under_their_own_field() {
    let rig = Rig::new();
    put(&user_file(&rig), &deny_file(&["Bash(secret-looking-rule)"]));
    put(&project_file(&rig, "settings.json"), &deny_file(&[]));
    let plan = dry(&rig);
    assert_eq!(plan.deny_rules.applied, 1);
    assert_eq!(plan.deny_rules.omitted, 0);
    assert_eq!(plan.deny_rules.files.len(), 2);
    assert_eq!(plan.deny_rules.files[1].rules, 0);
    assert_eq!(
        plan.deny_rules.user_deny_rules,
        ["Bash(secret-looking-rule)"]
    );

    // The rule is not in the arguments, the notes or the mapping: the settings
    // argument carries a count in its place.
    let settings = settings_arg(&plan.args);
    assert_eq!(
        settings["permissions"]["deny"],
        json!(["<1 deny rules, listed under deny_rules>"])
    );
    for text in [
        plan.args.join("\n"),
        plan.notes.join("\n"),
        plan.mapping.settings.join("\n"),
        plan.before_start.join("\n"),
    ] {
        assert!(!text.contains("secret-looking-rule"), "{text}");
    }
    // And in the JSON of the report it appears once, under the deny field.
    let json = serde_json::to_string(&plan).unwrap();
    assert_eq!(json.matches("secret-looking-rule").count(), 1, "{json}");
}

#[test]
fn a_dry_run_bounds_how_many_rules_and_how_long_each_one_it_prints() {
    let rig = Rig::new();
    let total = DRY_RUN_DENY_RULES_SHOWN + 5;
    let mut rules: Vec<String> = (0..total).map(|i| format!("Bash(r{i})")).collect();
    rules[0] = format!("Bash({})", "y".repeat(DRY_RUN_DENY_RULE_CHARS + 50));
    put(
        &user_file(&rig),
        &json!({ "permissions": { "deny": rules } }).to_string(),
    );
    let plan = dry(&rig);
    assert_eq!(plan.deny_rules.applied, total);
    assert_eq!(
        plan.deny_rules.user_deny_rules.len(),
        DRY_RUN_DENY_RULES_SHOWN
    );
    assert_eq!(plan.deny_rules.omitted, 5);
    let first = &plan.deny_rules.user_deny_rules[0];
    assert_eq!(
        first.chars().count(),
        DRY_RUN_DENY_RULE_CHARS + 3,
        "{first}"
    );
    assert!(first.ends_with("..."));
    // The real launch still carries every rule in full.
    let launch_rules = settings_arg(&plan.args)["permissions"]["deny"].clone();
    assert_eq!(launch_rules.as_array().unwrap().len(), 1);
}

#[test]
fn codex_reads_no_settings_and_reports_no_deny_rules() {
    let rig = Rig::new();
    // A file Claude would refuse the run for. Codex must not look at it.
    put(&user_file(&rig), "{ not json");
    put(
        &project_file(&rig, "settings.json"),
        &deny_file(&["Bash(x)"]),
    );
    let plan = dry_run(
        &rig.request(Provider::Codex, "p"),
        &rig.options(Provider::Codex),
    )
    .unwrap();
    assert!(plan.deny_rules.files.is_empty());
    assert_eq!(plan.deny_rules.applied, 0);
    assert!(deny_notes(&plan.notes).is_empty(), "{:?}", plan.notes);
    assert!(!plan.args.iter().any(|a| a.contains("Bash(x)")));
}

// ---- nothing else from the files leaves them ----

#[tokio::test]
async fn nothing_but_deny_rules_from_a_settings_file_reaches_any_output() {
    const MARKS: [&str; 9] = [
        "CANARY-ENV-VALUE",
        "CANARY-HOOK-COMMAND",
        "CANARY-API-KEY-HELPER",
        "CANARY-ALLOW-RULE",
        "CANARY-ASK-RULE",
        "CANARY-MCP-SERVER",
        "CANARY-INSTRUCTIONS",
        "CANARY-PLUGIN",
        "CANARY-MODEL",
    ];
    let body = json!({
        "env": { "SOME_VARIABLE": MARKS[0] },
        "hooks": { "PreToolUse": [ { "matcher": "Bash", "hooks": [
            { "type": "command", "command": MARKS[1] } ] } ] },
        "apiKeyHelper": MARKS[2],
        "permissions": {
            "allow": [ format!("Bash({})", MARKS[3]) ],
            "ask": [ format!("Bash({})", MARKS[4]) ],
            "deny": [ "Bash(denied-thing)" ],
            "additionalDirectories": [ MARKS[3] ],
            "defaultMode": "bypassPermissions"
        },
        "mcpServers": { "s": { "command": MARKS[5] } },
        "systemPrompt": MARKS[6],
        "enabledPlugins": { MARKS[7]: true },
        "model": MARKS[8],
    })
    .to_string();
    let rig = Rig::new();
    put(&user_file(&rig), &body);
    put(&project_file(&rig, "settings.json"), &body);
    put(&project_file(&rig, "settings.local.json"), &body);

    let mut request = rig.request(Provider::Claude, "success");
    request.permissions.commands = vec!["make".into()];
    let mut options = rig.options(Provider::Claude);
    options.hook = Some(Rig::guard_hook());
    let plan = dry_run(&request, &options).unwrap();
    let result: RunResult = run_checked(request, options).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");

    let mut seen = vec![
        serde_json::to_string(&plan).unwrap(),
        format!("{plan:?}"),
        serde_json::to_string(&result).unwrap(),
        format!("{result:?}"),
        rig.argv().join("\n"),
        rig.stdin(),
        rig.env_names().join("\n"),
    ];
    seen.push(String::from_utf8_lossy(&std::fs::read(rig.record.join("argv")).unwrap()).into());
    for mark in MARKS {
        for text in &seen {
            assert!(!text.contains(mark), "{mark} leaked into: {text}");
        }
    }
    // The one rule was applied once, from three files.
    assert_eq!(plan.deny_rules.user_deny_rules, ["Bash(denied-thing)"]);
    assert!(rig.argv().iter().any(|a| a.contains("denied-thing")));
    assert_eq!(
        settings_arg(&rig.argv())["permissions"],
        json!({ "deny": ["Bash(denied-thing)"] })
    );
}

// ---- enforcement through the fake ----

async fn claude(
    rig: &Rig,
    mode: Mode,
    scenario: String,
    edit: impl FnOnce(&mut crate::contract::request::Request),
) -> RunResult {
    let mut request = rig.request(Provider::Claude, &scenario);
    request.permissions.mode = mode;
    edit(&mut request);
    let mut options = rig.options(Provider::Claude);
    options.hook = Some(Rig::guard_hook());
    let result = run_checked(request, options).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    result
}

fn denied(result: &RunResult) -> Vec<&str> {
    result
        .permissions
        .denied
        .iter()
        .map(|d| d.request.as_str())
        .collect()
}

#[tokio::test]
async fn a_denied_file_inside_the_working_directory_is_denied_and_recorded_in_read_mode() {
    let rig = Rig::new();
    let secret = rig.work.join("secret.txt");
    let other = rig.work.join("other.txt");

    // Without a settings file the read is allowed.
    let result = claude(
        &rig,
        Mode::Read,
        format!("tool-read:{}", secret.display()),
        |_| {},
    )
    .await;
    assert!(denied(&result).is_empty());

    put(
        &project_file(&rig, "settings.json"),
        &deny_file(&["Read(./secret.txt)"]),
    );
    let result = claude(
        &rig,
        Mode::Read,
        format!("tool-read:{}", secret.display()),
        |_| {},
    )
    .await;
    assert_eq!(denied(&result), [format!("Read: {}", secret.display())]);
    let result = claude(
        &rig,
        Mode::Read,
        format!("tool-read:{}", other.display()),
        |_| {},
    )
    .await;
    assert!(denied(&result).is_empty(), "{:?}", denied(&result));
    // A search is a read too.
    let result = claude(
        &rig,
        Mode::Read,
        format!("tool-grep:{}", secret.display()),
        |_| {},
    )
    .await;
    assert_eq!(denied(&result), [format!("Grep: {}", secret.display())]);
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn a_deny_rule_narrows_write_build_and_full_modes_too() {
    let rig = Rig::new();
    let target = rig.work.join("keep.txt");
    let free = rig.work.join("free.txt");
    put(
        &project_file(&rig, "settings.local.json"),
        &deny_file(&["Edit(./keep.txt)", "Read(./keep.txt)"]),
    );
    for mode in [Mode::Write, Mode::Build, Mode::Full] {
        let result = claude(
            &rig,
            mode,
            format!("tool-write:{}", target.display()),
            |_| {},
        )
        .await;
        assert!(!target.exists(), "{mode:?}: the write must not happen");
        assert_eq!(denied(&result), [format!("Write: {}", target.display())]);
        let result = claude(
            &rig,
            mode,
            format!("tool-read:{}", target.display()),
            |_| {},
        )
        .await;
        assert_eq!(denied(&result), [format!("Read: {}", target.display())]);
        let result = claude(&rig, mode, format!("tool-write:{}", free.display()), |_| {}).await;
        assert!(free.exists(), "{mode:?}");
        assert!(denied(&result).is_empty());
        std::fs::remove_file(&free).unwrap();
    }
}

/// A `make deploy` call with that command and `make test` both listed.
async fn listed_bash(rig: &Rig, command: &str) -> RunResult {
    let listed = command.to_string();
    claude(rig, Mode::Read, format!("tool-bash:{command}"), move |r| {
        r.permissions.commands = vec![listed, "make test".into()];
    })
    .await
}

#[tokio::test]
async fn a_denied_command_is_denied_even_when_allow_command_lists_it() {
    let rig = Rig::new();
    // Allowed without a deny rule.
    let result = listed_bash(&rig, "make deploy").await;
    assert!(denied(&result).is_empty(), "{:?}", denied(&result));
    assert_eq!(result.text, "ran: make deploy", "{result:#?}");

    put(&user_file(&rig), &deny_file(&["Bash(make deploy)"]));
    let result = listed_bash(&rig, "make deploy").await;
    assert_eq!(denied(&result), ["Bash: make deploy"]);
    // Another listed command is still allowed.
    let result = claude(&rig, Mode::Read, "tool-bash:make test".into(), |r| {
        r.permissions.commands = vec!["make deploy".into(), "make test".into()];
    })
    .await;
    assert!(denied(&result).is_empty());

    // In full mode every command is allowed except the denied one.
    let result = claude(&rig, Mode::Full, "tool-bash:make deploy".into(), |_| {}).await;
    assert_eq!(denied(&result), ["Bash: make deploy"]);
    let result = claude(&rig, Mode::Full, "tool-bash:make clean".into(), |_| {}).await;
    assert!(denied(&result).is_empty());
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn a_user_rule_and_a_project_rule_both_apply() {
    let rig = Rig::new();
    let user_secret = rig.work.join("user-secret.txt");
    let project_secret = rig.work.join("project-secret.txt");
    let fine = rig.work.join("fine.txt");
    put(
        &user_file(&rig),
        &deny_file(&[&format!("Read(/{})", user_secret.display())]),
    );
    put(
        &project_file(&rig, "settings.json"),
        &deny_file(&["Read(./project-secret.txt)"]),
    );
    for (path, want_denied) in [
        (&user_secret, true),
        (&project_secret, true),
        (&fine, false),
    ] {
        let result = claude(
            &rig,
            Mode::Read,
            format!("tool-read:{}", path.display()),
            |_| {},
        )
        .await;
        assert_eq!(
            !denied(&result).is_empty(),
            want_denied,
            "{}: {:?}",
            path.display(),
            denied(&result)
        );
    }
}

#[tokio::test]
async fn a_relative_rule_keeps_its_meaning_when_the_working_directory_is_a_subdirectory() {
    let rig = Rig::new();
    let sub = rig.work.join("sub");
    let deeper = sub.join("deeper");
    std::fs::create_dir_all(&deeper).unwrap();
    put(
        &sub.join(".claude/settings.json"),
        &deny_file(&["Read(./secret.txt)"]),
    );
    // The rule is in the subdirectory's settings, so it means the subdirectory.
    for (path, want_denied) in [
        (sub.join("secret.txt"), true),
        (deeper.join("secret.txt"), true),
        (sub.join("ok.txt"), false),
    ] {
        let result = claude(
            &rig,
            Mode::Read,
            format!("tool-read:{}", path.display()),
            |r| {
                r.cwd = Some(sub.clone());
            },
        )
        .await;
        assert_eq!(
            !denied(&result).is_empty(),
            want_denied,
            "{}",
            path.display()
        );
    }
    // A settings file in the parent is not the project's.
    put(
        &rig.work.join(".claude/settings.json"),
        &deny_file(&["Read(./other.txt)"]),
    );
    let result = claude(
        &rig,
        Mode::Read,
        format!("tool-read:{}", sub.join("other.txt").display()),
        |r| r.cwd = Some(sub.clone()),
    )
    .await;
    assert!(denied(&result).is_empty());

    // A relative rule in the user file resolves against the working directory,
    // like one in a project file, so it applies under it with the working
    // directory outside home.
    std::fs::remove_file(sub.join(".claude/settings.json")).unwrap();
    put(&user_file(&rig), &deny_file(&["Read(private/**)"]));
    for (path, want_denied) in [
        (sub.join("private/x"), true),
        (sub.join("deeper/private/x"), true),
        (sub.join("public/x"), false),
    ] {
        let result = claude(
            &rig,
            Mode::Full,
            format!("tool-read:{}", path.display()),
            |r| r.cwd = Some(sub.clone()),
        )
        .await;
        assert_eq!(
            !denied(&result).is_empty(),
            want_denied,
            "{}",
            path.display()
        );
    }
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn deny_rules_with_commas_brackets_quotes_and_spaces_reach_the_provider_intact() {
    let rig = Rig::new();
    let rules = [
        "Bash(echo \"a, b\")",
        "Bash(x],[y)",
        "Bash(deny me,Edit,Write)",
    ];
    put(&user_file(&rig), &deny_file(&rules));
    let result = claude(&rig, Mode::Full, "success".into(), |_| {}).await;
    // The fake records exactly what it received.
    let received = settings_arg(&rig.argv());
    assert_eq!(received["permissions"]["deny"], json!(rules));
    // The allow list holds only delega's own rules.
    let argv = rig.argv();
    let at = argv.iter().position(|a| a == "--allowedTools").unwrap();
    assert!(!argv[at + 1].contains("deny me"));
    assert!(!argv[at + 1].contains("x],[y"));
    assert!(denied(&result).is_empty());
}

// ---- repository roots ----

fn git_dir(path: &Path) {
    std::fs::create_dir_all(path.join(".git")).unwrap();
}

/// Make `linked` a linked worktree of `main` as git does: an administrative
/// directory `main/.git/worktrees/<name>` whose `gitdir` file names
/// `linked/.git`, and that `.git` file pointing back at the directory.
fn link_worktree(main: &Path, name: &str, linked: &Path) {
    let admin = main.join(".git/worktrees").join(name);
    std::fs::create_dir_all(&admin).unwrap();
    std::fs::write(
        admin.join("gitdir"),
        format!("{}\n", linked.join(".git").display()),
    )
    .unwrap();
    std::fs::create_dir_all(linked).unwrap();
    std::fs::write(
        linked.join(".git"),
        format!("gitdir: {}\n", admin.display()),
    )
    .unwrap();
}

#[test]
fn local_settings_are_also_read_from_the_repository_root_and_shared_settings_are_not() {
    let rig = Rig::new();
    git_dir(&rig.work);
    let sub = rig.work.join("a/b");
    std::fs::create_dir_all(&sub).unwrap();
    put(
        &project_file(&rig, "settings.local.json"),
        &deny_file(&["Bash(root-local)"]),
    );
    put(
        &project_file(&rig, "settings.json"),
        &deny_file(&["Bash(root-shared)"]),
    );
    put(
        &sub.join(".claude/settings.local.json"),
        &deny_file(&["Bash(sub-local)"]),
    );
    put(
        &sub.join(".claude/settings.json"),
        &deny_file(&["Bash(sub-shared)"]),
    );
    let mut request = rig.request(Provider::Claude, "p");
    request.cwd = Some(sub.clone());
    let plan = dry_run(&request, &rig.options(Provider::Claude)).unwrap();
    assert_eq!(
        plan.deny_rules.user_deny_rules,
        ["Bash(sub-shared)", "Bash(sub-local)", "Bash(root-local)"]
    );
    // Started at the root, the root's local file is read once.
    let plan = dry(&rig);
    assert_eq!(
        plan.deny_rules.user_deny_rules,
        ["Bash(root-shared)", "Bash(root-local)"]
    );
}

#[test]
fn without_a_git_repository_only_the_start_directory_is_read() {
    let rig = Rig::new();
    let sub = rig.work.join("sub");
    std::fs::create_dir(&sub).unwrap();
    put(
        &project_file(&rig, "settings.local.json"),
        &deny_file(&["Bash(parent)"]),
    );
    let mut request = rig.request(Provider::Claude, "p");
    request.cwd = Some(sub);
    let plan = dry_run(&request, &rig.options(Provider::Claude)).unwrap();
    assert_eq!(plan.deny_rules.applied, 0);
}

#[test]
fn a_linked_worktree_also_reads_the_main_checkouts_local_settings() {
    let rig = Rig::new();
    let main = rig.home.join("main");
    git_dir(&main);
    put(
        &main.join(".claude/settings.local.json"),
        &deny_file(&["Bash(main-local)"]),
    );
    let linked = rig.work.clone();
    link_worktree(&main, "w", &linked);
    put(
        &project_file(&rig, "settings.local.json"),
        &deny_file(&["Bash(linked-local)"]),
    );
    let sub = linked.join("sub");
    std::fs::create_dir(&sub).unwrap();
    let mut request = rig.request(Provider::Claude, "p");
    request.cwd = Some(sub);
    let plan = dry_run(&request, &rig.options(Provider::Claude)).unwrap();
    assert_eq!(
        plan.deny_rules.user_deny_rules,
        ["Bash(linked-local)", "Bash(main-local)"]
    );

    // A `.git` file that points nowhere useful leaves the nearest root only, and
    // a note says so.
    std::fs::write(linked.join(".git"), "gitdir: /nonexistent/x\n").unwrap();
    let plan = dry(&rig);
    assert_eq!(plan.deny_rules.user_deny_rules, ["Bash(linked-local)"]);
    assert!(
        plan.notes
            .iter()
            .any(|n| n.contains("could not be identified"))
    );
}

// ---- file handling ----

#[test]
fn an_empty_file_is_no_rules_and_a_byte_order_mark_is_stripped() {
    let rig = Rig::new();
    put(&user_file(&rig), "  \n");
    let plan = dry(&rig);
    assert_eq!(plan.deny_rules.applied, 0);
    assert_eq!(plan.deny_rules.files.len(), 1);
    put(&user_file(&rig), "");
    assert_eq!(dry(&rig).deny_rules.files.len(), 1);
    put(
        &user_file(&rig),
        &format!("\u{feff}{}", deny_file(&["Bash(bom)"])),
    );
    assert_eq!(dry(&rig).deny_rules.user_deny_rules, ["Bash(bom)"]);
}

#[test]
fn a_named_pipe_is_refused_without_blocking() {
    let rig = Rig::new();
    let fifo = user_file(&rig);
    let status = std::process::Command::new("/usr/bin/mkfifo")
        .arg(&fifo)
        .guarded_status()
        .expect("run mkfifo");
    assert!(status.success());
    let error = dry_run(
        &rig.request(Provider::Claude, "p"),
        &rig.options(Provider::Claude),
    )
    .unwrap_err();
    assert_eq!(error.kind, "settings_unreadable");
    assert!(error.message.contains("not a regular file"), "{error}");
}

#[test]
fn a_file_that_cannot_be_read_refuses_naming_it() {
    use std::os::unix::fs::PermissionsExt;
    let rig = Rig::new();
    let path = user_file(&rig);
    put(&path, &deny_file(&["Bash(x)"]));
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    // A process that may read it anyway (root) cannot show the refusal.
    if std::fs::File::open(&path).is_ok() {
        return;
    }
    let error = dry_run(
        &rig.request(Provider::Claude, "p"),
        &rig.options(Provider::Claude),
    )
    .unwrap_err();
    assert_eq!(error.kind, "settings_unreadable");
    assert!(error.message.contains(path.to_str().unwrap()), "{error}");
    assert!(error.message.contains("cannot be opened"), "{error}");
}

#[test]
fn a_home_reached_through_a_link_is_shown_by_its_real_path() {
    let rig = Rig::new();
    let real = rig.work.join("real-home");
    put(
        &real.join(".claude/settings.json"),
        &deny_file(&["Bash(x)"]),
    );
    let link = rig.work.join("link-home");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let mut options = rig.options(Provider::Claude);
    options.env_set.retain(|(n, _)| n != "CLAUDE_CONFIG_DIR");
    let env = options.parent_env.as_mut().unwrap();
    env.retain(|(n, _)| n != "HOME");
    env.push((OsString::from("HOME"), link.into_os_string()));
    let plan = dry_run(&rig.request(Provider::Claude, "p"), &options).unwrap();
    assert_eq!(
        plan.deny_rules.files[0].path,
        real.join(".claude/settings.json").display().to_string()
    );
}

#[tokio::test]
async fn what_a_dry_run_reports_is_what_a_run_passes() {
    let rig = Rig::new();
    put(
        &user_file(&rig),
        &deny_file(&["Bash(a)", "Read(/b)", "Read(c)"]),
    );
    let request = rig.request(Provider::Claude, "success");
    let plan = dry_run(&request, &rig.options(Provider::Claude)).unwrap();
    let result = run_checked(request, rig.options(Provider::Claude)).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    let passed = settings_arg(&rig.argv())["permissions"]["deny"].clone();
    assert_eq!(passed, json!(plan.deny_rules.user_deny_rules));
    assert_eq!(plan.deny_rules.applied, passed.as_array().unwrap().len());
}

// ---- one-slash rules in project files, and which roots are trusted ----

#[test]
fn a_one_slash_rule_in_the_root_of_a_worktree_gets_the_working_directory_and_the_files_directory() {
    let rig = Rig::new();
    let main = rig.home.join("main");
    git_dir(&main);
    put(
        &main.join(".claude/settings.local.json"),
        &deny_file(&["Edit(/out/main)"]),
    );
    link_worktree(&main, "w", &rig.work);
    put(
        &project_file(&rig, "settings.local.json"),
        &deny_file(&["Read(/out/root)"]),
    );
    let sub = rig.work.join("sub");
    put(
        &sub.join(".claude/settings.json"),
        &deny_file(&["Read(/out/start)"]),
    );
    let mut request = rig.request(Provider::Claude, "p");
    request.cwd = Some(sub.clone());
    let plan = dry_run(&request, &rig.options(Provider::Claude)).unwrap();
    let (sub, work, main) = (sub.display(), rig.work.display(), main.display());
    assert_eq!(
        plan.deny_rules.user_deny_rules,
        [
            "Read(/out/start)".to_string(),
            format!("Read(/{sub}/out/start)"),
            "Read(/out/root)".to_string(),
            format!("Read(/{sub}/out/root)"),
            format!("Read(/{work}/out/root)"),
            "Edit(/out/main)".to_string(),
            format!("Edit(/{sub}/out/main)"),
            format!("Edit(/{main}/out/main)"),
        ]
    );
}

#[test]
fn a_repository_root_that_is_the_home_directory_is_skipped_and_named() {
    let rig = Rig::new();
    git_dir(&rig.home);
    put(
        &rig.home.join(".claude/settings.local.json"),
        &deny_file(&["Bash(from-home-root)"]),
    );
    let sub = rig.home.join("proj");
    std::fs::create_dir(&sub).unwrap();
    let mut request = rig.request(Provider::Claude, "p");
    request.cwd = Some(sub);
    let plan = dry_run(&request, &rig.options(Provider::Claude)).unwrap();
    assert_eq!(plan.deny_rules.applied, 0);
    let skipped = &plan.deny_rules.skipped;
    assert_eq!(skipped.len(), 1, "{skipped:?}");
    assert_eq!(
        skipped[0].path,
        rig.home
            .join(".claude/settings.local.json")
            .display()
            .to_string()
    );
    assert!(skipped[0].reason.contains("home directory"));
    assert!(
        plan.notes
            .iter()
            .any(|n| n.contains("was ignored") && n.contains(&skipped[0].path)),
        "{:?}",
        plan.notes
    );
    assert!(!plan.notes.join("\n").contains("from-home-root"));
    // Started in the home directory itself, its own file is read as today.
    let plan = dry_run(
        &{
            let mut r = rig.request(Provider::Claude, "p");
            r.cwd = Some(rig.home.clone());
            r
        },
        &rig.options(Provider::Claude),
    )
    .unwrap();
    assert_eq!(plan.deny_rules.user_deny_rules, ["Bash(from-home-root)"]);
}

fn owner<'a>(euid: u32, foreign: &'a [std::path::PathBuf]) -> crate::provider::deny::Owner<'a> {
    crate::provider::deny::Owner { euid, foreign }
}

fn me() -> u32 {
    rustix::process::geteuid().as_raw()
}

#[test]
fn a_root_not_owned_by_the_current_user_is_not_read_and_is_named() {
    let rig = Rig::new();
    git_dir(&rig.work);
    let sub = rig.work.join("sub");
    put(
        &sub.join(".claude/settings.json"),
        &deny_file(&["Bash(start)"]),
    );
    let file = project_file(&rig, "settings.local.json");
    put(&file, &deny_file(&["Bash(root)"]));
    let home = rig.home.clone().into_os_string();
    let config = rig.config.clone().into_os_string();
    let deny = |euid: u32, foreign: &[std::path::PathBuf]| {
        crate::provider::deny::collect_as(Some(&home), Some(&config), &sub, owner(euid, foreign))
            .unwrap()
    };
    assert_eq!(deny(me(), &[]).rules, ["Bash(start)", "Bash(root)"]);
    // Another user's view: the files here belong to someone else.
    let other = deny(me().wrapping_add(1), &[]);
    assert_eq!(other.rules, ["Bash(start)"]);
    assert_eq!(other.skipped.len(), 1);
    assert!(other.skipped[0].reason.contains("not owned"));
    assert!(other.notes().iter().any(|n| n.contains("was ignored")));

    // Each entry on the way to the file is judged on its own: with only that
    // entry owned by someone else, the file is skipped for that reason.
    let entries = [
        (
            rig.work.clone(),
            "the repository root is not owned by the current user",
        ),
        (
            rig.work.join(".git"),
            "the .git entry of the repository root is not owned by the current user",
        ),
        (
            rig.work.join(".claude"),
            "the .claude directory is not owned by the current user",
        ),
        (
            file.clone(),
            "the settings file is not owned by the current user",
        ),
    ];
    for (entry, reason) in entries {
        let foreign = [entry.clone()];
        let got = deny(me(), &foreign);
        assert_eq!(got.rules, ["Bash(start)"], "{}", entry.display());
        assert_eq!(got.skipped.len(), 1, "{}", entry.display());
        assert_eq!(got.skipped[0].path, file.display().to_string());
        assert_eq!(got.skipped[0].reason, reason);
        let note = got
            .notes()
            .into_iter()
            .find(|n| n.contains("was ignored"))
            .expect("a note");
        assert!(note.contains(&file.display().to_string()) && note.contains(reason));
        assert!(!note.contains("Bash(root)"));
    }
}

#[test]
fn an_unowned_main_checkout_is_skipped_and_named_like_any_other_root() {
    let rig = Rig::new();
    let main = rig.home.join("main");
    git_dir(&main);
    let file = main.join(".claude/settings.local.json");
    put(&file, &deny_file(&["Bash(main-local)"]));
    link_worktree(&main, "w", &rig.work);
    put(
        &project_file(&rig, "settings.local.json"),
        &deny_file(&["Bash(linked-local)"]),
    );
    let home = rig.home.clone().into_os_string();
    let config = rig.config.clone().into_os_string();
    let deny = |foreign: &[std::path::PathBuf]| {
        crate::provider::deny::collect_as(
            Some(&home),
            Some(&config),
            &rig.work,
            owner(me(), foreign),
        )
        .unwrap()
    };
    assert_eq!(deny(&[]).rules, ["Bash(linked-local)", "Bash(main-local)"]);
    for (entry, reason) in [
        (
            main.clone(),
            "the repository root is not owned by the current user",
        ),
        (
            main.join(".git"),
            "the .git entry of the repository root is not owned by the current user",
        ),
    ] {
        let got = deny(std::slice::from_ref(&entry));
        assert_eq!(got.rules, ["Bash(linked-local)"], "{}", entry.display());
        assert_eq!(got.skipped.len(), 1, "{}", entry.display());
        assert_eq!(got.skipped[0].path, file.display().to_string());
        assert_eq!(got.skipped[0].reason, reason);
        assert!(
            got.notes()
                .iter()
                .any(|n| n.contains("was ignored") && n.contains(&file.display().to_string())),
            "{:?}",
            got.notes()
        );
    }
    // A `.git` file of the linked worktree that the user does not own is not
    // followed, and a note says why.
    let got = deny(&[rig.work.join(".git")]);
    assert_eq!(got.rules, ["Bash(linked-local)"]);
    assert!(
        got.notes()
            .iter()
            .any(|n| n.contains("could not be identified") && n.contains("not owned")),
        "{:?}",
        got.notes()
    );
}

#[test]
fn a_gitdir_line_is_followed_only_to_a_worktrees_directory_of_an_owned_checkout() {
    let rig = Rig::new();
    let sub = rig.work.join("sub");
    std::fs::create_dir(&sub).unwrap();
    let main = rig.home.join("main");
    git_dir(&main);
    put(
        &main.join(".claude/settings.local.json"),
        &deny_file(&["Bash(main)"]),
    );
    // The directory the `.git` file names, with a `gitdir` file naming this
    // worktree back, so only the shape of the path is in question.
    let read = |gitdir: &Path| {
        std::fs::create_dir_all(gitdir).unwrap();
        std::fs::write(
            gitdir.join("gitdir"),
            format!("{}\n", rig.work.join(".git").display()),
        )
        .unwrap();
        std::fs::write(
            rig.work.join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .unwrap();
        let mut request = rig.request(Provider::Claude, "p");
        request.cwd = Some(sub.clone());
        dry_run(&request, &rig.options(Provider::Claude)).unwrap()
    };
    // An unrelated directory is not followed.
    let unrelated = rig.home.join("unrelated");
    put(
        &unrelated.join(".claude/settings.local.json"),
        &deny_file(&["Bash(no)"]),
    );
    assert_eq!(read(&unrelated).deny_rules.applied, 0);
    // A directory in a `.git` directory that is not under `worktrees`: were the
    // `worktrees` name not required, `main` would be taken from it.
    assert_eq!(read(&main.join(".git/refs/w")).deny_rules.applied, 0);
    // A directory under `worktrees` that is not in a `.git` directory: were the
    // `.git` name not required, `outer` would be taken from it.
    let outer = rig.home.join("outer");
    put(
        &outer.join(".claude/settings.local.json"),
        &deny_file(&["Bash(outer)"]),
    );
    git_dir(&outer);
    let plan = read(&outer.join("repo/worktrees/w"));
    assert_eq!(plan.deny_rules.applied, 0);
    assert!(
        plan.notes
            .iter()
            .any(|n| n.contains("main checkout") && n.contains("could not be identified")),
        "{:?}",
        plan.notes
    );
    // A proper one is, including a worktree named `worktrees`.
    let plan = read(&main.join(".git/worktrees/worktrees"));
    assert_eq!(plan.deny_rules.user_deny_rules, ["Bash(main)"]);
    let plan = read(&main.join(".git/worktrees/w"));
    assert_eq!(plan.deny_rules.user_deny_rules, ["Bash(main)"]);
    assert!(
        !plan
            .notes
            .iter()
            .any(|n| n.contains("could not be identified")),
        "{:?}",
        plan.notes
    );
}

#[test]
fn a_git_file_planted_in_a_tree_cannot_borrow_another_checkouts_rules() {
    let rig = Rig::new();
    let main = rig.home.join("main");
    git_dir(&main);
    put(
        &main.join(".claude/settings.local.json"),
        &deny_file(&["Bash(main)"]),
    );
    // A real linked worktree of `main` is `rig.work`.
    link_worktree(&main, "w", &rig.work);
    let admin = main.join(".git/worktrees/w");
    // An unpacked tree whose `.git` file names that worktree's directory.
    let planted = rig.home.join("planted");
    std::fs::create_dir_all(&planted).unwrap();
    std::fs::write(
        planted.join(".git"),
        format!("gitdir: {}\n", admin.display()),
    )
    .unwrap();
    let plan_in = |dir: &Path| {
        let mut request = rig.request(Provider::Claude, "p");
        request.cwd = Some(dir.to_path_buf());
        dry_run(&request, &rig.options(Provider::Claude)).unwrap()
    };
    let plan = plan_in(&planted);
    assert_eq!(plan.deny_rules.applied, 0);
    assert!(
        plan.notes
            .iter()
            .any(|n| n.contains(&planted.display().to_string())
                && n.contains("does not name this worktree")),
        "{:?}",
        plan.notes
    );
    // The real worktree is still followed.
    assert_eq!(
        plan_in(&rig.work).deny_rules.user_deny_rules,
        ["Bash(main)"]
    );
    // With no `gitdir` file in the directory the tree is not a worktree of it.
    std::fs::remove_file(admin.join("gitdir")).unwrap();
    let plan = plan_in(&rig.work);
    assert_eq!(plan.deny_rules.applied, 0);
    assert!(
        plan.notes
            .iter()
            .any(|n| n.contains("could not be identified"))
    );
}

#[test]
fn a_main_checkout_that_cannot_be_identified_is_named_in_a_note() {
    let rig = Rig::new();
    let plan_in_work = || dry(&rig);
    let note = |plan: &DryRun| {
        plan.notes
            .iter()
            .find(|n| n.contains("could not be identified"))
            .cloned()
    };
    // A `.git` directory that is a symbolic link to a git directory elsewhere.
    let main = rig.home.join("main");
    std::fs::create_dir_all(&main).unwrap();
    let real_git = rig.home.join("elsewhere/real-git");
    std::fs::create_dir_all(&real_git).unwrap();
    std::os::unix::fs::symlink(&real_git, main.join(".git")).unwrap();
    put(
        &main.join(".claude/settings.local.json"),
        &deny_file(&["Bash(main)"]),
    );
    link_worktree(&main, "w", &rig.work);
    let plan = plan_in_work();
    assert_eq!(plan.deny_rules.applied, 0);
    let text = note(&plan).expect("a note");
    assert!(text.contains(&rig.work.display().to_string()), "{text}");
    // A git directory kept elsewhere under another name.
    std::fs::remove_file(main.join(".git")).unwrap();
    let separate = rig.home.join("repo.git");
    std::fs::create_dir_all(&separate).unwrap();
    git_dir(&main);
    let admin = separate.join("worktrees/w");
    std::fs::create_dir_all(&admin).unwrap();
    std::fs::write(
        admin.join("gitdir"),
        format!("{}\n", rig.work.join(".git").display()),
    )
    .unwrap();
    std::fs::write(
        rig.work.join(".git"),
        format!("gitdir: {}\n", admin.display()),
    )
    .unwrap();
    let plan = plan_in_work();
    assert_eq!(plan.deny_rules.applied, 0);
    assert!(note(&plan).is_some(), "{:?}", plan.notes);
    // A gitdir line that names nothing, and a `.git` file with no such line.
    std::fs::write(rig.work.join(".git"), "gitdir: /nonexistent/x\n").unwrap();
    assert!(
        note(&plan_in_work())
            .expect("a note")
            .contains("does not exist")
    );
    std::fs::write(rig.work.join(".git"), "not a gitdir line\n").unwrap();
    assert!(note(&plan_in_work()).is_some());
}

/// A repository root `rig.work` (with a `.git` directory, mode 0755) and a
/// subdirectory to start in, so the root is not the start directory.
struct LinkRig {
    rig: Rig,
    sub: std::path::PathBuf,
    /// A directory holding the targets of links.
    elsewhere: std::path::PathBuf,
}

impl LinkRig {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let rig = Rig::new();
        git_dir(&rig.work);
        std::fs::set_permissions(&rig.work, std::fs::Permissions::from_mode(0o755)).unwrap();
        let sub = rig.work.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let elsewhere = rig.home.join("elsewhere");
        put(
            &elsewhere.join(".claude/settings.local.json"),
            &deny_file(&["Bash(linked)"]),
        );
        Self {
            rig,
            sub,
            elsewhere,
        }
    }

    fn file(&self) -> std::path::PathBuf {
        self.rig.work.join(".claude/settings.local.json")
    }

    fn target_dir(&self) -> std::path::PathBuf {
        self.elsewhere.join(".claude")
    }

    fn target_file(&self) -> std::path::PathBuf {
        self.target_dir().join("settings.local.json")
    }

    /// `.claude` in the root is a link to the target directory.
    fn link_dir(&self) {
        std::os::unix::fs::symlink(self.target_dir(), self.rig.work.join(".claude")).unwrap();
    }

    /// `.claude` is a real directory and the settings file a link to `target`.
    fn link_file(&self, target: &Path) {
        std::fs::create_dir_all(self.rig.work.join(".claude")).unwrap();
        std::os::unix::fs::symlink(target, self.file()).unwrap();
    }

    fn chmod(&self, path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    fn run(&self) -> Result<DryRun, crate::provider::Refusal> {
        let mut request = self.rig.request(Provider::Claude, "p");
        request.cwd = Some(self.sub.clone());
        dry_run(&request, &self.rig.options(Provider::Claude))
    }

    fn plan(&self) -> DryRun {
        self.run().expect("dry run")
    }

    /// The one skipped file is the root's settings file, for `why`, with a note.
    fn assert_skipped(&self, why: &str) {
        let plan = self.plan();
        assert_eq!(plan.deny_rules.applied, 0, "{:?}", plan.notes);
        assert_eq!(plan.deny_rules.skipped.len(), 1, "{:?}", plan.notes);
        assert_eq!(
            plan.deny_rules.skipped[0].path,
            self.file().display().to_string()
        );
        assert!(
            plan.deny_rules.skipped[0].reason.contains(why),
            "{:?}",
            plan.deny_rules.skipped
        );
        assert!(
            plan.notes
                .iter()
                .any(|n| n.contains("was ignored") && n.contains(why))
        );
    }

    /// Run through `collect_as` with `foreign` standing in for other users' files.
    fn skipped_for_foreign(&self, foreign: &[std::path::PathBuf]) -> Vec<String> {
        let home = self.rig.home.clone().into_os_string();
        let config = self.rig.config.clone().into_os_string();
        let got = crate::provider::deny::collect_as(
            Some(&home),
            Some(&config),
            &self.sub,
            owner(me(), foreign),
        )
        .unwrap();
        assert!(got.rules.is_empty(), "{:?}", got.rules);
        got.skipped.into_iter().map(|f| f.reason).collect()
    }
}

#[test]
fn a_link_the_user_owns_in_a_directory_only_the_user_can_write_is_followed_in_a_repository_root() {
    // `.claude` as a link to a directory the user owns.
    let rig = LinkRig::new();
    rig.link_dir();
    let plan = rig.plan();
    assert_eq!(plan.deny_rules.user_deny_rules, ["Bash(linked)"]);
    assert!(plan.deny_rules.skipped.is_empty());
    // The settings file as a link to a file the user owns.
    let rig = LinkRig::new();
    rig.link_file(&rig.target_file());
    assert_eq!(rig.plan().deny_rules.user_deny_rules, ["Bash(linked)"]);
}

#[test]
fn a_link_in_a_directory_others_can_write_is_skipped_in_a_repository_root() {
    for mode in [0o775, 0o757, 0o777, 0o770 | 0o002] {
        // The link is `.claude`, in the root.
        let rig = LinkRig::new();
        rig.link_dir();
        rig.chmod(&rig.rig.work, mode);
        rig.assert_skipped("a directory that group or others can write");
        // The link is the file, in `.claude`.
        let rig = LinkRig::new();
        rig.link_file(&rig.target_file());
        rig.chmod(&rig.rig.work.join(".claude"), mode);
        rig.assert_skipped("a directory that group or others can write");
    }
    // Group write alone, and world write alone, are each enough.
    for mode in [0o720, 0o702] {
        let rig = LinkRig::new();
        rig.link_dir();
        rig.chmod(&rig.rig.work, mode);
        rig.assert_skipped("a directory that group or others can write");
    }
}

#[test]
fn a_link_is_skipped_unless_the_link_its_directory_and_its_target_are_the_users() {
    let rig = LinkRig::new();
    rig.link_dir();
    let link = rig.rig.work.join(".claude");
    // The link belongs to someone else.
    assert_eq!(
        rig.skipped_for_foreign(std::slice::from_ref(&link)),
        ["the .claude directory is a symbolic link that the current user does not own"]
    );
    // The directory holding the link belongs to someone else.
    assert_eq!(
        rig.skipped_for_foreign(std::slice::from_ref(&rig.rig.work)),
        ["the .claude directory is a symbolic link in a directory the current user does not own"]
    );
    // The target belongs to someone else.
    assert_eq!(
        rig.skipped_for_foreign(&[rig.target_dir()]),
        ["the .claude directory is a symbolic link whose target is not owned by the current user"]
    );
    // The same for the settings file.
    let rig = LinkRig::new();
    rig.link_file(&rig.target_file());
    assert_eq!(
        rig.skipped_for_foreign(&[rig.file()]),
        ["the settings file is a symbolic link that the current user does not own"]
    );
    assert_eq!(
        rig.skipped_for_foreign(&[rig.rig.work.join(".claude")]),
        ["the settings file is a symbolic link in a directory the current user does not own"]
    );
    assert_eq!(
        rig.skipped_for_foreign(&[rig.target_file()]),
        ["the settings file is a symbolic link whose target is not owned by the current user"]
    );
}

#[test]
fn a_dangling_link_in_a_repository_root_is_skipped_with_a_note() {
    let rig = LinkRig::new();
    std::os::unix::fs::symlink(rig.elsewhere.join("missing"), rig.rig.work.join(".claude"))
        .unwrap();
    rig.assert_skipped("a symbolic link to nothing");
    let rig = LinkRig::new();
    rig.link_file(&rig.elsewhere.join("missing"));
    rig.assert_skipped("a symbolic link to nothing");
}

#[test]
fn a_link_to_a_pipe_or_a_device_does_not_hang_and_is_handled_like_the_plain_file() {
    // A pipe the user owns is refused as not a regular file, as a pipe named
    // as the settings file itself is (a_named_pipe_is_refused_without_blocking).
    let rig = LinkRig::new();
    let fifo = rig.rig.home.join("fifo");
    assert!(
        std::process::Command::new("/usr/bin/mkfifo")
            .arg(&fifo)
            .guarded_status()
            .unwrap()
            .success()
    );
    rig.link_file(&fifo);
    let error = rig.run().unwrap_err();
    assert_eq!(error.kind, "settings_unreadable");
    assert!(error.message.contains("not a regular file"), "{error}");
    // A device owned by someone else is skipped as a foreign target and never
    // opened; run as the device's owner (root) it is refused as not a regular
    // file, like the pipe.
    if Path::new("/dev/zero").exists() {
        let rig = LinkRig::new();
        rig.link_file(Path::new("/dev/zero"));
        if me() == 0 {
            assert_eq!(rig.run().unwrap_err().kind, "settings_unreadable");
        } else {
            rig.assert_skipped("a symbolic link whose target is not owned by the current user");
        }
    }
}

#[test]
fn what_someone_else_owns_is_skipped_whatever_its_kind_or_mode() {
    // A pipe as the settings file, owned by someone else: skipped, not refused,
    // and not opened for reading.
    let rig = LinkRig::new();
    std::fs::create_dir_all(rig.rig.work.join(".claude")).unwrap();
    assert!(
        std::process::Command::new("/usr/bin/mkfifo")
            .arg(rig.file())
            .guarded_status()
            .unwrap()
            .success()
    );
    assert_eq!(
        rig.skipped_for_foreign(&[rig.file()]),
        ["the settings file is not owned by the current user"]
    );
    // The same pipe owned by the user is refused.
    assert_eq!(rig.run().unwrap_err().kind, "settings_unreadable");
    // A `.claude` directory with mode 000 owned by someone else.
    let rig = LinkRig::new();
    put(&rig.file(), &deny_file(&["Bash(root)"]));
    let dir = rig.rig.work.join(".claude");
    rig.chmod(&dir, 0o000);
    let skipped = rig.skipped_for_foreign(std::slice::from_ref(&dir));
    rig.chmod(&dir, 0o755);
    assert_eq!(
        skipped,
        ["the .claude directory is not owned by the current user"]
    );
    // A settings file with mode 000 owned by someone else.
    rig.chmod(&rig.file(), 0o000);
    let skipped = rig.skipped_for_foreign(&[rig.file()]);
    rig.chmod(&rig.file(), 0o600);
    assert_eq!(
        skipped,
        ["the settings file is not owned by the current user"]
    );
}

#[test]
fn a_claude_directory_that_cannot_be_searched_in_a_repository_root_refuses() {
    let rig = LinkRig::new();
    put(&rig.file(), &deny_file(&["Bash(root)"]));
    let dir = rig.rig.work.join(".claude");
    rig.chmod(&dir, 0o600);
    // A process that may search it anyway (root) cannot show the refusal.
    if std::fs::metadata(rig.file()).is_ok() {
        rig.chmod(&dir, 0o755);
        return;
    }
    let result = rig.run();
    rig.chmod(&dir, 0o755);
    let error = result.unwrap_err();
    assert_eq!(error.kind, "settings_unreadable");
    assert!(error.message.contains("cannot be opened"), "{error}");
    assert_eq!(rig.plan().deny_rules.user_deny_rules, ["Bash(root)"]);
}

#[test]
fn a_link_in_a_repository_root_never_reaches_the_root_itself_or_its_git_entry() {
    // `.git` as a link is skipped whoever owns it.
    let rig = LinkRig::new();
    put(&rig.file(), &deny_file(&["Bash(root)"]));
    std::fs::remove_dir(rig.rig.work.join(".git")).unwrap();
    std::os::unix::fs::symlink(&rig.elsewhere, rig.rig.work.join(".git")).unwrap();
    rig.assert_skipped(".git entry of the repository root is a symbolic link");
    std::fs::remove_file(rig.rig.work.join(".git")).unwrap();
    git_dir(&rig.rig.work);
    assert_eq!(rig.plan().deny_rules.user_deny_rules, ["Bash(root)"]);
}

#[test]
fn a_file_that_cannot_be_opened_in_a_repository_root_refuses_and_is_not_skipped() {
    // Mode 000 does not deny a process that may read anyway (root).
    let rig = LinkRig::new();
    put(&rig.file(), &deny_file(&["Bash(root)"]));
    rig.chmod(&rig.file(), 0o000);
    if std::fs::File::open(rig.file()).is_ok() {
        return;
    }
    let error = rig.run().unwrap_err();
    assert_eq!(error.kind, "settings_unreadable");
    assert!(error.message.contains("cannot be opened"), "{error}");
    assert!(error.message.contains("settings.local.json"), "{error}");
    rig.chmod(&rig.file(), 0o600);
    // The same for the directory that holds it.
    let dir = rig.rig.work.join(".claude");
    rig.chmod(&dir, 0o000);
    let error = rig.run().unwrap_err();
    assert_eq!(error.kind, "settings_unreadable");
    assert!(error.message.contains("cannot be opened"), "{error}");
    rig.chmod(&dir, 0o755);
    assert_eq!(rig.plan().deny_rules.user_deny_rules, ["Bash(root)"]);
}

#[test]
fn a_link_to_a_target_the_user_owns_but_cannot_open_refuses_and_is_not_skipped() {
    // The settings file as a link to the user's own file with mode 000. The
    // owner is the user, so ownership does not skip it; opening it fails, and the
    // run is refused as it is for the same file named directly. A process that
    // may read it anyway (root) cannot show the refusal.
    let rig = LinkRig::new();
    rig.link_file(&rig.target_file());
    rig.chmod(&rig.target_file(), 0o000);
    if std::fs::File::open(rig.target_file()).is_err() {
        let result = rig.run();
        rig.chmod(&rig.target_file(), 0o600);
        let error = result.unwrap_err();
        assert_eq!(error.kind, "settings_unreadable");
        assert!(
            error
                .message
                .contains("the settings file cannot be opened (permission denied)"),
            "{error}"
        );
        assert!(error.message.contains("settings.local.json"), "{error}");
        assert_eq!(rig.plan().deny_rules.user_deny_rules, ["Bash(linked)"]);
    } else {
        rig.chmod(&rig.target_file(), 0o600);
    }
    // `.claude` as a link to the user's own directory with mode 000.
    let rig = LinkRig::new();
    rig.link_dir();
    rig.chmod(&rig.target_dir(), 0o000);
    if std::fs::read_dir(rig.target_dir()).is_err() {
        let result = rig.run();
        rig.chmod(&rig.target_dir(), 0o755);
        let error = result.unwrap_err();
        assert_eq!(error.kind, "settings_unreadable");
        assert!(
            error
                .message
                .contains("the .claude directory cannot be opened (permission denied)"),
            "{error}"
        );
        assert_eq!(rig.plan().deny_rules.user_deny_rules, ["Bash(linked)"]);
    } else {
        rig.chmod(&rig.target_dir(), 0o755);
    }
}

#[test]
fn a_link_whose_target_cannot_be_examined_refuses_and_is_not_skipped() {
    // The owner of such a target cannot be known, so the rule about what someone
    // else owns does not apply. The link is the user's own, in a directory only
    // the user can write, so the broken target is the user's to fix.
    //
    // A link into a directory the user cannot search.
    let rig = LinkRig::new();
    let blocked = rig.rig.home.join("blocked");
    put(
        &blocked.join("settings.local.json"),
        &deny_file(&["Bash(blocked)"]),
    );
    put(
        &blocked.join(".claude/settings.local.json"),
        &deny_file(&["Bash(blocked)"]),
    );
    rig.link_file(&blocked.join("settings.local.json"));
    rig.chmod(&blocked, 0o000);
    if std::fs::metadata(blocked.join("settings.local.json")).is_err() {
        let result = rig.run();
        rig.chmod(&blocked, 0o755);
        let error = result.unwrap_err();
        assert_eq!(error.kind, "settings_unreadable");
        assert!(
            error
                .message
                .contains("the settings file cannot be opened (permission denied)"),
            "{error}"
        );
        assert_eq!(rig.plan().deny_rules.user_deny_rules, ["Bash(blocked)"]);
        // The same for `.claude` as the link.
        let rig = LinkRig::new();
        std::os::unix::fs::symlink(blocked.join(".claude"), rig.rig.work.join(".claude")).unwrap();
        rig.chmod(&blocked, 0o000);
        let result = rig.run();
        rig.chmod(&blocked, 0o755);
        let error = result.unwrap_err();
        assert_eq!(error.kind, "settings_unreadable");
        assert!(
            error
                .message
                .contains("the .claude directory cannot be opened (permission denied)"),
            "{error}"
        );
    } else {
        rig.chmod(&blocked, 0o755);
    }
    // A link whose path runs through a regular file.
    let rig = LinkRig::new();
    let plain = rig.rig.home.join("plain");
    std::fs::write(&plain, "not a directory").unwrap();
    rig.link_file(&plain.join("x.json"));
    let error = rig.run().unwrap_err();
    assert_eq!(error.kind, "settings_unreadable");
    assert!(
        error
            .message
            .contains("the settings file cannot be opened (not a directory)"),
        "{error}"
    );
    let rig = LinkRig::new();
    std::os::unix::fs::symlink(plain.join("x"), rig.rig.work.join(".claude")).unwrap();
    let error = rig.run().unwrap_err();
    assert_eq!(error.kind, "settings_unreadable");
    assert!(
        error
            .message
            .contains("the .claude directory cannot be opened (not a directory)"),
        "{error}"
    );
}
