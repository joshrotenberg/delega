//! The provider environment policy.

use std::ffi::OsString;

use super::{Rig, both, run_checked};
use crate::contract::request::Provider;
use crate::provider::env::{
    EnvInputs, is_calling_session, is_other_providers, is_secret_name, owned_by, resolve,
};

const CANARY: &str = "canary-value-must-not-appear";

fn parent(names: &[&str]) -> Vec<(OsString, OsString)> {
    names
        .iter()
        .map(|n| (OsString::from(n), OsString::from(format!("{CANARY}-{n}"))))
        .collect()
}

fn names_for(provider: Provider, parent: &[(OsString, OsString)], pass: &[&str]) -> Vec<String> {
    let pass: Vec<String> = pass.iter().map(ToString::to_string).collect();
    resolve(&EnvInputs {
        provider,
        parent,
        pass: &pass,
        set: &[],
        adapter: &[],
    })
    .names()
}

#[test]
fn secret_looking_names_are_recognised_and_ordinary_ones_are_not() {
    for name in [
        "MY_SERVICE_API_TOKEN",
        "GITHUB_TOKEN",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_ACCESS_KEY_ID",
        "DB_PASSWORD",
        "STRIPE_KEY",
        "GOOGLE_APPLICATION_CREDENTIALS",
        "SSH_AUTH_SOCK",
        "my_token",
        "SOME_APIKEY",
        "DEPLOY_PRIVATE_KEY",
    ] {
        assert!(is_secret_name(name), "{name}");
    }
    for name in [
        "PATH",
        "HOME",
        "LANG",
        "TERM",
        "TMPDIR",
        "KEYBOARD_LAYOUT",
        "MONKEY",
        "USER",
    ] {
        assert!(!is_secret_name(name), "{name}");
    }
}

#[test]
fn each_provider_keeps_its_own_variables_and_loses_the_other_providers() {
    let env = parent(&[
        "PATH",
        "HOME",
        "ANTHROPIC_API_KEY",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "CLAUDECODE",
        "CLAUDE_CODE_MESSAGING_TOKEN",
        "OPENAI_API_KEY",
        "CODEX_HOME",
        "UNRELATED_SECRET",
        "UNRELATED_VALUE",
        "DELEGA_CLAUDE_BIN",
    ]);
    assert_eq!(
        names_for(Provider::Claude, &env, &[]),
        [
            "ANTHROPIC_API_KEY",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "HOME",
            "PATH",
            "UNRELATED_VALUE"
        ]
    );
    assert_eq!(
        names_for(Provider::Codex, &env, &[]),
        [
            "CODEX_HOME",
            "HOME",
            "OPENAI_API_KEY",
            "PATH",
            "UNRELATED_VALUE"
        ]
    );
}

#[test]
fn a_named_pass_overrides_the_policy_and_only_for_that_name() {
    let env = parent(&["PATH", "UNRELATED_SECRET", "OTHER_SECRET", "OPENAI_API_KEY"]);
    assert_eq!(
        names_for(
            Provider::Claude,
            &env,
            &["UNRELATED_SECRET", "OPENAI_API_KEY"]
        ),
        ["OPENAI_API_KEY", "PATH", "UNRELATED_SECRET"]
    );
}

#[test]
fn set_variables_apply_and_adapter_variables_cannot_be_overridden() {
    let env = parent(&["PATH"]);
    let resolved = resolve(&EnvInputs {
        provider: Provider::Claude,
        parent: &env,
        pass: &[],
        set: &[
            ("MY_TOKEN".into(), "explicit".into()),
            ("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS".into(), "0".into()),
        ],
        adapter: &[("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS".into(), "1".into())],
    });
    assert_eq!(
        resolved.names(),
        ["CLAUDE_CODE_DISABLE_BACKGROUND_TASKS", "MY_TOKEN", "PATH"]
    );
    let policy = format!("{:?}", resolved.to_policy());
    assert!(policy.contains("\"1\""), "adapter value wins: {policy}");
    assert!(
        !policy.contains("\"0\""),
        "overridden value is gone: {policy}"
    );
}

#[test]
fn ownership_and_stripping_rules_do_not_overlap_by_accident() {
    assert!(owned_by(Provider::Claude, "ANTHROPIC_API_KEY"));
    assert!(!owned_by(Provider::Claude, "OPENAI_API_KEY"));
    assert!(is_other_providers(Provider::Codex, "ANTHROPIC_API_KEY"));
    assert!(is_other_providers(Provider::Claude, "CODEX_HOME"));
    assert!(!is_other_providers(
        Provider::Claude,
        "CLAUDE_CODE_OAUTH_TOKEN"
    ));
    assert!(is_calling_session("CLAUDE_CODE_MESSAGING_SOCKET"));
    assert!(is_calling_session("CLAUDE_CODE_MESSAGING_TOKEN"));
    assert!(is_calling_session("CLAUDE_CODE_BRIDGE_SESSION_ID"));
    assert!(!is_calling_session("CLAUDE_CODE_OAUTH_TOKEN"));
}

const SESSION: [&str; 9] = [
    "CLAUDECODE",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_EFFORT",
    "CLAUDE_PID",
    "AI_AGENT",
];
const CONFIG: [&str; 5] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_BASE_URL",
    "CLAUDE_CONFIG_DIR",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "OPENAI_API_KEY",
];

#[test]
fn session_variables_are_withheld_from_both_providers_and_configuration_passes() {
    let mut names: Vec<&str> = SESSION.to_vec();
    names.extend(CONFIG);
    names.push("PATH");
    let env = parent(&names);
    let claude = names_for(Provider::Claude, &env, &[]);
    for name in SESSION {
        assert!(!claude.contains(&name.to_string()), "claude {name}");
    }
    assert_eq!(
        claude,
        [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_BASE_URL",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_CONFIG_DIR",
            "PATH"
        ]
    );
    let codex = names_for(Provider::Codex, &env, &[]);
    for name in SESSION {
        assert!(!codex.contains(&name.to_string()), "codex {name}");
    }
    assert_eq!(codex, ["OPENAI_API_KEY", "PATH"]);
}

#[test]
fn an_explicit_pass_overrides_the_session_variable_rule() {
    let env = parent(&["PATH", "CLAUDE_CODE_SESSION_ID", "AI_AGENT"]);
    assert_eq!(
        names_for(Provider::Claude, &env, &["AI_AGENT"]),
        ["AI_AGENT", "PATH"]
    );
}

async fn session_variables_do_not_reach_the_child(provider: Provider) {
    let rig = Rig::new();
    let mut options = rig.options(provider);
    let mut env = options.parent_env.take().unwrap();
    for name in SESSION
        .iter()
        .chain(&["CLAUDE_CONFIG_DIR_EXTRA_NOT_SESSION"])
    {
        env.push((OsString::from(name), OsString::from(CANARY)));
    }
    options.parent_env = Some(env);
    run_checked(rig.request(provider, "success"), options).await;
    let seen = rig.env_names();
    for name in SESSION {
        assert!(!seen.contains(&name.to_string()), "{name}: {seen:?}");
    }
    assert!(seen.contains(&"HOME".to_string()), "{seen:?}");
    rig.assert_nothing_left().await;
}
both!(
    withholds_the_calling_sessions_variables_from_the_child,
    session_variables_do_not_reach_the_child
);

async fn secret_is_not_passed_unless_allowed(provider: Provider) {
    let canary_name = "MY_SERVICE_API_TOKEN";
    let other_name = match provider {
        Provider::Claude => "OPENAI_API_KEY",
        Provider::Codex => "ANTHROPIC_API_KEY",
    };
    let own_name = match provider {
        Provider::Claude => "ANTHROPIC_API_KEY",
        Provider::Codex => "OPENAI_API_KEY",
    };

    let rig = Rig::new();
    let mut options = rig.options(provider);
    let mut env = options.parent_env.take().unwrap();
    for name in [canary_name, other_name, own_name, "DELEGA_CLAUDE_BIN"] {
        env.push((OsString::from(name), OsString::from(CANARY)));
    }
    options.parent_env = Some(env.clone());
    let result = run_checked(rig.request(provider, "success"), options.clone()).await;
    let seen = rig.env_names();
    assert!(!seen.contains(&canary_name.to_string()), "{seen:?}");
    assert!(!seen.contains(&other_name.to_string()), "{seen:?}");
    assert!(!seen.contains(&"DELEGA_CLAUDE_BIN".to_string()), "{seen:?}");
    assert!(seen.contains(&own_name.to_string()), "{seen:?}");
    assert!(seen.contains(&"HOME".to_string()), "{seen:?}");
    let shown = format!("{result:?} {options:?}");
    assert!(
        !shown.contains(CANARY),
        "no environment value in diagnostics"
    );
    rig.assert_nothing_left().await;

    let rig = Rig::new();
    let mut options = rig.options(provider);
    options.parent_env = Some(env);
    options.env_pass = vec![canary_name.to_string()];
    run_checked(rig.request(provider, "success"), options).await;
    assert!(rig.env_names().contains(&canary_name.to_string()));
    rig.assert_nothing_left().await;
}
both!(
    withholds_a_secret_looking_variable_unless_it_is_allowed,
    secret_is_not_passed_unless_allowed
);

async fn explicit_set_reaches_the_provider(provider: Provider) {
    let rig = Rig::new();
    let mut options = rig.options(provider);
    options
        .env_set
        .push(("MY_EXPLICIT_TOKEN".into(), CANARY.into()));
    run_checked(rig.request(provider, "success"), options).await;
    let seen = rig.env_names();
    assert!(seen.contains(&"MY_EXPLICIT_TOKEN".to_string()), "{seen:?}");
    assert!(seen.contains(&"FAKE_RECORD_DIR".to_string()), "{seen:?}");
    rig.assert_nothing_left().await;
}
both!(
    passes_an_explicitly_set_variable,
    explicit_set_reaches_the_provider
);

async fn only_the_policy_environment_is_visible(provider: Provider) {
    let rig = Rig::new();
    run_checked(rig.request(provider, "success"), rig.options(provider)).await;
    let mut seen = rig.env_names();
    // The shell adds PWD, OLDPWD, SHLVL and `_` of its own.
    seen.retain(|n| !matches!(n.as_str(), "PWD" | "OLDPWD" | "SHLVL" | "_"));
    let mut expected: Vec<&str> = vec![
        "CLAUDE_CONFIG_DIR",
        "CODEX_HOME",
        "FAKE_RECORD_DIR",
        "HOME",
        "PATH",
        "XDG_CONFIG_HOME",
    ];
    if provider == Provider::Claude {
        expected.push("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS");
    }
    expected.sort_unstable();
    assert_eq!(seen, expected);
    rig.assert_nothing_left().await;
}
both!(
    gives_the_provider_nothing_beyond_the_policy,
    only_the_policy_environment_is_visible
);
