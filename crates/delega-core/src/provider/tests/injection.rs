//! Rule injection through `--allowedTools`: text that could end or split a rule
//! must be refused before the provider starts.

use std::path::PathBuf;

use super::Rig;
use crate::contract::permissions::Mode;
use crate::contract::request::{Provider, Request};
use crate::provider::launch::Inputs;
use crate::provider::permissions::Scope;
use crate::provider::{HookCommand, claude, dry_run};

fn hook() -> HookCommand {
    HookCommand::new("/opt/delega/hook", ["permission-hook"])
}

/// Split rule text on commas that are not inside parentheses.
fn split_rules(text: &str) -> Vec<String> {
    let (mut rules, mut current, mut depth) = (Vec::new(), String::new(), 0i32);
    for c in text.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                rules.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    rules.push(current);
    rules
}

const BAD_COMMANDS: [&str; 12] = [
    "x),Edit,Bash(y",
    "x)",
    "x(",
    "a,b",
    "ls *.rs",
    "echo \\",
    "a\nb",
    "a\rb",
    "a\tb",
    "a\u{7f}b",
    "a\u{0}b",
    "git status),Write(//**",
];

#[test]
fn commands_that_could_end_or_split_a_rule_are_refused_for_each_field_value() {
    let rig = Rig::new();
    for command in BAD_COMMANDS {
        let mut request = rig.request(Provider::Claude, "p");
        request.permissions.commands = vec!["ok".into(), command.into()];
        let mut options = rig.options(Provider::Claude);
        options.hook = Some(hook());
        let error = dry_run(&request, &options).unwrap_err();
        // A NUL byte is stopped earlier, by request validation.
        let kind = if command.contains('\0') {
            "invalid_request"
        } else {
            "unsupported_command"
        };
        assert_eq!(error.kind, kind, "{command:?}");
        assert!(!error.message.is_empty());
    }
}

#[test]
fn a_working_directory_or_extra_directory_with_rule_syntax_in_its_name_is_refused() {
    let rig = Rig::new();
    for name in [
        "a),Edit,Read(",
        "a,b",
        "a(b",
        "a)b",
        "a*b",
        "a?b",
        "a[b",
        "a]b",
        "a{b}",
        "a\\b",
        "a\nb",
        "a'b",
        "a\"b",
        "a;b",
        "a$b",
        "a#b",
    ] {
        let dir = rig.work.join(name);
        std::fs::create_dir(&dir).unwrap();
        for in_cwd in [true, false] {
            let mut request = rig.request(Provider::Claude, "p");
            let kind = if in_cwd {
                request.cwd = Some(dir.clone());
                "cwd_unusable"
            } else {
                request.permissions.directories = vec![dir.clone()];
                "directory_unusable"
            };
            let error = dry_run(&request, &rig.options(Provider::Claude)).unwrap_err();
            assert_eq!(error.kind, kind, "{name:?} in_cwd={in_cwd}");
            assert!(
                error
                    .message
                    .contains("cannot be written as a Claude Code rule"),
                "{}",
                error.message
            );
        }
    }
}

#[test]
fn full_mode_writes_no_path_rules_so_a_special_name_is_not_refused() {
    let rig = Rig::new();
    let dir = rig.work.join("a),Edit,Read(");
    std::fs::create_dir(&dir).unwrap();
    let mut request = rig.request(Provider::Claude, "p");
    request.cwd = Some(dir);
    request.permissions.mode = Mode::Full;
    assert!(dry_run(&request, &rig.options(Provider::Claude)).is_ok());
}

#[test]
fn codex_has_no_rule_text_and_accepts_the_same_names() {
    let rig = Rig::new();
    let dir = rig.work.join("a),Edit,Read(");
    std::fs::create_dir(&dir).unwrap();
    let mut request = rig.request(Provider::Codex, "p");
    request.cwd = Some(dir);
    request.permissions.commands = vec!["x),Edit,Bash(y".into()];
    assert!(dry_run(&request, &rig.options(Provider::Codex)).is_ok());
}

/// A small deterministic generator, so the test needs no extra crate.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn text(&mut self, alphabet: &[char], max: usize) -> String {
        let len = 1 + (self.next() as usize) % max;
        (0..len)
            .map(|_| alphabet[(self.next() as usize) % alphabet.len()])
            .collect()
    }
}

#[test]
fn for_any_accepted_input_the_rules_split_into_exactly_the_expected_rules() {
    let alphabet: Vec<char> = "ab Z9/._-+@=:%~(),*?[]{}\\\n\t'\"$#;|&<>!`ü"
        .chars()
        .collect();
    let mut rng = Lcg(7);
    let (mut accepted, mut refused) = (0, 0);
    for _ in 0..30000 {
        let mode = [Mode::Read, Mode::Write][(rng.next() % 2) as usize];
        let cwd = format!("/{}", rng.text(&alphabet, 12));
        let dirs: Vec<PathBuf> = (0..(rng.next() % 3))
            .map(|_| PathBuf::from(format!("/{}", rng.text(&alphabet, 12))))
            .collect();
        // `permissions::scope` removes duplicates before `build`; do the same.
        let mut commands: Vec<String> = Vec::new();
        for _ in 0..(rng.next() % 3) {
            let command = rng.text(&alphabet, 12);
            if !commands.contains(&command) {
                commands.push(command);
            }
        }
        let scope = Scope {
            mode,
            cwd: PathBuf::from(&cwd),
            directories: dirs.clone(),
            commands: commands.clone(),
        };
        let mut request = Request::new(Provider::Claude, "p");
        request.permissions.mode = mode;
        let hook = hook();
        let built = claude::build(&Inputs {
            request: &request,
            scope: &scope,
            hook: Some(&hook),
            system: "s",
            native: &[],
        });
        let Ok(built) = built else {
            refused += 1;
            continue;
        };
        accepted += 1;
        let at = built
            .args
            .iter()
            .position(|a| a == "--allowedTools")
            .unwrap();
        let rules = split_rules(&built.args[at + 1]);
        let rule = |tool: &str, path: &str| {
            let inner = path.trim_matches('/');
            if inner.is_empty() {
                format!("{tool}(//**)")
            } else {
                format!("{tool}(//{inner}/**)")
            }
        };
        let mut expected = vec![rule("Read", &cwd)];
        expected.extend(dirs.iter().map(|d| rule("Read", &d.to_string_lossy())));
        expected.extend(["Glob".to_string(), "Grep".to_string()]);
        if mode == Mode::Write {
            expected.push(rule("Edit", &cwd));
            expected.push(rule("Write", &cwd));
        }
        expected.extend(commands.iter().map(|c| format!("Bash({c})")));
        assert_eq!(
            rules, expected,
            "cwd={cwd:?} dirs={dirs:?} commands={commands:?}"
        );
    }
    assert!(accepted > 50, "accepted {accepted}");
    assert!(refused > 50, "refused {refused}");
}
