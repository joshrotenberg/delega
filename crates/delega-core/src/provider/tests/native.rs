//! The `native_args` allowlist.

use super::{Rig, both, run_checked};
use crate::contract::outcome::Outcome;
use crate::contract::request::Provider;
use crate::provider::native_args::{allowlist, check};

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(ToString::to_string).collect()
}

#[test]
fn claude_allows_its_listed_options_in_both_forms() {
    assert_eq!(
        check(
            Provider::Claude,
            &args(&["--max-turns", "5", "--max-budget-usd=0.5"])
        )
        .unwrap(),
        ["--max-turns", "5", "--max-budget-usd", "0.5"]
    );
    assert_eq!(
        check(
            Provider::Claude,
            &args(&["--fallback-model", "claude-haiku-4.5"])
        )
        .unwrap(),
        ["--fallback-model", "claude-haiku-4.5"]
    );
    assert_eq!(check(Provider::Claude, &[]).unwrap(), Vec::<String>::new());
}

#[test]
fn codex_allows_only_the_listed_config_keys_and_values() {
    assert_eq!(
        check(Provider::Codex, &args(&["-c", "model_verbosity=low"])).unwrap(),
        ["-c", "model_verbosity=low"]
    );
    assert_eq!(
        check(
            Provider::Codex,
            &args(&["--config=model_reasoning_summary=concise"])
        )
        .unwrap(),
        ["--config", "model_reasoning_summary=concise"]
    );
    for bad in [
        "sandbox_mode=danger-full-access",
        "approval_policy=never",
        "model_verbosity=loud",
        "model_verbosity",
        "cwd=/",
    ] {
        let error = check(Provider::Codex, &args(&["-c", bad])).unwrap_err();
        assert_eq!(error.kind, "native_arg_rejected", "{bad}");
        assert!(
            !error.message.contains(bad),
            "the value is not echoed: {}",
            error.message
        );
    }
}

#[test]
fn options_that_change_permissions_sandbox_directory_config_or_output_are_rejected() {
    let claude = [
        "--permission-mode",
        "--dangerously-skip-permissions",
        "--allowedTools",
        "--tools",
        "--settings",
        "--setting-sources",
        "--add-dir",
        "--mcp-config",
        "--output-format",
        "--append-system-prompt",
        "--resume",
    ];
    for flag in claude {
        let error = check(Provider::Claude, &args(&[flag, "x"])).unwrap_err();
        assert_eq!(error.kind, "native_arg_rejected", "{flag}");
        assert!(error.message.contains(flag), "{flag}: {}", error.message);
        assert!(
            error.message.contains("permissions, sandboxing"),
            "{flag}: {}",
            error.message
        );
    }
    let codex = [
        "--sandbox",
        "-s",
        "--ask-for-approval",
        "--full-auto",
        "--dangerously-bypass-approvals-and-sandbox",
        "--cd",
        "-C",
        "--json",
        "--output-schema",
        "--profile",
        "--ignore-user-config",
        "--model",
    ];
    for flag in codex {
        let error = check(Provider::Codex, &args(&[flag, "x"])).unwrap_err();
        assert!(
            error.message.contains("permissions, sandboxing"),
            "{flag}: {}",
            error.message
        );
    }
}

#[test]
fn unknown_options_values_without_options_and_bad_values_are_rejected() {
    let cases: [(&[&str], &str); 8] = [
        (&["--unknown-flag"], "not in the allowlist"),
        (&["stray"], "without an option"),
        (&["--max-turns"], "needs a value"),
        (&["--max-turns", "abc"], "not one this option allows"),
        (&["--max-turns", "0"], "not one this option allows"),
        (&["--max-turns", "-1"], "not one this option allows"),
        (&["--max-budget-usd", "1e9"], "not one this option allows"),
        (
            &["--fallback-model", "--evil"],
            "not one this option allows",
        ),
    ];
    for (list, why) in cases {
        let error = check(Provider::Claude, &args(list)).unwrap_err();
        assert!(error.message.contains(why), "{list:?}: {}", error.message);
    }
    assert!(check(Provider::Claude, &args(&["--max-turns=3", "--max-turns"])).is_err());
}

#[test]
fn the_allowlist_is_described_for_each_provider() {
    assert!(
        allowlist(Provider::Claude)
            .iter()
            .any(|l| l.contains("--max-turns"))
    );
    assert!(
        allowlist(Provider::Codex)
            .iter()
            .any(|l| l.contains("model_verbosity"))
    );
}

async fn allowed_native_args_reach_the_provider_last(provider: Provider) {
    let rig = Rig::new();
    let mut request = rig.request(provider, "success");
    request.native_args = match provider {
        Provider::Claude => args(&["--max-turns", "4"]),
        Provider::Codex => args(&["-c", "model_verbosity=high"]),
    };
    let result = run_checked(request, rig.options(provider)).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    let argv = rig.argv();
    let expected: &[&str] = match provider {
        Provider::Claude => &["--max-turns", "4"],
        Provider::Codex => &["-c", "model_verbosity=high"],
    };
    assert!(argv.windows(2).any(|w| w == expected), "{argv:?}");
    rig.assert_nothing_left().await;
}
both!(
    passes_allowed_native_args_through,
    allowed_native_args_reach_the_provider_last
);

async fn rejected_native_arg_starts_nothing(provider: Provider) {
    let rig = Rig::new();
    let mut request = rig.request(provider, "success");
    request.native_args = args(&["--sandbox", "danger-full-access"]);
    let result = run_checked(request, rig.options(provider)).await;
    assert_eq!(result.outcome, Outcome::Refused, "{result:#?}");
    assert_eq!(result.error.expect("error").kind, "native_arg_rejected");
    assert!(rig.pid("pid").is_none());
}
both!(
    a_rejected_native_arg_refuses_the_run,
    rejected_native_arg_starts_nothing
);
