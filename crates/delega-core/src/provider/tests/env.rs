//! The provider environment policy.

use std::ffi::OsString;

use super::{Rig, both, run_checked};
use crate::contract::request::Provider;
use crate::provider::env::{
    BASE_NAMES, EnvInputs, NETWORK_NAMES, check_names, is_calling_session, is_secret_name,
    is_valid_name, owned_by, resolve,
};
use crate::provider::{ProviderOptions, dry_run};

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
        "DATABASE_URL",
        "REDIS_CLOUD_URL",
        "MONGODB_URI",
        "SENTRY_DSN",
        "AZURE_STORAGE_CONNECTION_STRING",
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
        "HTTPS_PROXY",
        "CURLOPT",
    ] {
        assert!(!is_secret_name(name), "{name}");
    }
}

fn resolved(
    provider: Provider,
    parent: &[(OsString, OsString)],
    pass: &[&str],
) -> crate::provider::env::ResolvedEnv {
    let pass: Vec<String> = pass.iter().map(ToString::to_string).collect();
    resolve(&EnvInputs {
        provider,
        parent,
        pass: &pass,
        set: &[],
        adapter: &[],
    })
}

#[test]
fn only_the_allowlist_passes_and_each_group_is_reported_under_its_reason() {
    let env = parent(&[
        "PATH",
        "HOME",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "XDG_CONFIG_HOME",
        "NO_COLOR",
        "CI",
        "HTTPS_PROXY",
        "no_proxy",
        "SSL_CERT_FILE",
        "NODE_EXTRA_CA_CERTS",
        "ANTHROPIC_API_KEY",
        "CLAUDE_CONFIG_DIR",
        "EDITOR",
        "JAVA_HOME",
        "DATABASE_URL",
        "MY_SERVICE_API_TOKEN",
    ]);
    let resolved = resolved(Provider::Claude, &env, &[]);
    let report = resolved.report();
    assert_eq!(
        report.base,
        [
            "CI",
            "HOME",
            "LANG",
            "LC_ALL",
            "LC_CTYPE",
            "NO_COLOR",
            "PATH",
            "XDG_CONFIG_HOME"
        ]
    );
    assert_eq!(
        report.network,
        [
            "HTTPS_PROXY",
            "NODE_EXTRA_CA_CERTS",
            "SSL_CERT_FILE",
            "no_proxy"
        ]
    );
    assert_eq!(report.provider, ["ANTHROPIC_API_KEY", "CLAUDE_CONFIG_DIR"]);
    assert!(report.named.is_empty() && report.set_by_caller.is_empty());
    assert!(report.set_by_delega.is_empty());
    assert_eq!(report.withheld_count, 4);
    let names = resolved.names();
    for withheld in [
        "EDITOR",
        "JAVA_HOME",
        "DATABASE_URL",
        "MY_SERVICE_API_TOKEN",
    ] {
        assert!(!names.contains(&withheld.to_string()), "{withheld}");
    }
}

#[test]
fn every_listed_base_and_network_name_passes_for_both_providers() {
    let mut all: Vec<&str> = BASE_NAMES.to_vec();
    all.extend(NETWORK_NAMES);
    let env = parent(&all);
    for provider in [Provider::Claude, Provider::Codex] {
        let mut expected: Vec<String> = all.iter().map(ToString::to_string).collect();
        expected.sort();
        assert_eq!(names_for(provider, &env, &[]), expected, "{provider:?}");
    }
}

#[test]
fn each_provider_receives_its_own_variables_and_not_the_other_providers() {
    let env = parent(&[
        "PATH",
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
        ["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN", "PATH"]
    );
    assert_eq!(
        names_for(Provider::Codex, &env, &[]),
        ["CODEX_HOME", "OPENAI_API_KEY", "PATH"]
    );
}

#[test]
fn a_named_variable_passes_only_that_name_and_may_belong_to_the_other_provider() {
    let env = parent(&["PATH", "UNRELATED_SECRET", "OTHER_SECRET", "OPENAI_API_KEY"]);
    let resolved = resolved(
        Provider::Claude,
        &env,
        &["UNRELATED_SECRET", "OPENAI_API_KEY"],
    );
    assert_eq!(
        resolved.names(),
        ["OPENAI_API_KEY", "PATH", "UNRELATED_SECRET"]
    );
    assert_eq!(
        resolved.report().named,
        ["OPENAI_API_KEY", "UNRELATED_SECRET"]
    );
    assert_eq!(resolved.report().withheld_count, 1);
}

#[test]
fn a_named_variable_that_is_not_set_is_ignored_and_listed_as_not_set() {
    let env = parent(&["PATH"]);
    let resolved = resolved(Provider::Codex, &env, &["JAVA_HOME", "PATH"]);
    assert_eq!(resolved.names(), ["PATH"]);
    assert_eq!(resolved.report().named_not_set, ["JAVA_HOME"]);
    assert!(
        resolved.notes().iter().all(|n| !n.contains("JAVA_HOME")),
        "{:?}",
        resolved.notes()
    );
}

#[test]
fn the_notes_give_counts_and_name_each_explicit_secret_looking_variable() {
    let env = parent(&[
        "PATH",
        "HOME",
        "EDITOR",
        "JAVA_HOME",
        "AWS_SECRET_ACCESS_KEY",
    ]);
    let plain = resolved(Provider::Claude, &env, &[]);
    assert_eq!(
        plain.notes(),
        ["3 environment variables of the caller were withheld; 2 were passed"]
    );
    let named = resolved(
        Provider::Claude,
        &env,
        &["AWS_SECRET_ACCESS_KEY", "JAVA_HOME", "NOT_SET_TOKEN"],
    );
    assert_eq!(
        named.notes(),
        [
            "environment variable AWS_SECRET_ACCESS_KEY was passed because the caller named \
             it; its name looks like a secret",
            "1 environment variables of the caller were withheld; 4 were passed"
        ]
    );
    let shown = format!("{:?}", named.notes());
    assert!(!shown.contains(CANARY), "{shown}");
}

#[test]
fn set_variables_are_reported_by_who_set_them_and_not_under_another_reason() {
    let env = parent(&["PATH", "XDG_CONFIG_HOME"]);
    let resolved = resolve(&EnvInputs {
        provider: Provider::Claude,
        parent: &env,
        pass: &[],
        set: &[
            ("XDG_CONFIG_HOME".into(), "elsewhere".into()),
            ("MY_DEPLOY_TOKEN".into(), "v".into()),
            ("EMPTY_ONE".into(), String::new()),
        ],
        adapter: &[("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS".into(), "1".into())],
    });
    let report = resolved.report();
    assert_eq!(report.base, ["PATH"]);
    assert_eq!(
        report.set_by_caller,
        ["EMPTY_ONE", "MY_DEPLOY_TOKEN", "XDG_CONFIG_HOME"]
    );
    assert_eq!(
        report.set_by_delega,
        ["CLAUDE_CODE_DISABLE_BACKGROUND_TASKS"]
    );
    assert_eq!(report.withheld_count, 0);
    assert_eq!(
        resolved.notes(),
        [
            "environment variable MY_DEPLOY_TOKEN was set to a value the caller gave; its name \
             looks like a secret",
            "0 environment variables of the caller were withheld; 5 were passed"
        ]
    );
}

#[test]
fn names_to_pass_or_set_are_validated_before_a_run_starts() {
    for bad in [
        "",
        "1ABC",
        "A-B",
        "A B",
        "A=b",
        "NAME=secret-value",
        "A.B",
        "\u{e9}",
    ] {
        let error = check_names(&[bad.to_string()], &[], &[]).unwrap_err();
        assert_eq!(error.kind, "invalid_env_name", "{bad:?}");
        assert!(
            bad.is_empty() || !error.message.contains(bad),
            "the message does not echo the text: {}",
            error.message
        );
        assert!(!is_valid_name(bad), "{bad:?}");
        let error = check_names(&[], &[(bad.to_string(), "v".into())], &[]).unwrap_err();
        assert_eq!(error.kind, "invalid_env_name", "set {bad:?}");
        assert!(error.message.contains("to set"), "{}", error.message);
    }
    for good in ["A", "_A", "a1_B2", "JAVA_HOME"] {
        assert!(is_valid_name(good), "{good}");
        check_names(
            &[good.to_string()],
            &[(good.to_string(), "x=y".into())],
            &[],
        )
        .unwrap();
    }
    for forbidden in [
        "CLAUDECODE",
        "CLAUDE_CODE_SESSION_ID",
        "CLAUDE_CODE_MESSAGING_TOKEN",
        "AI_AGENT",
        "DELEGA_CLAUDE_BIN",
        "DELEGA_ANYTHING",
        "CLAUDE_CODE_SSE_PORT",
        "CODEX_THREAD_ID",
        "CODEX_SANDBOX",
    ] {
        let error =
            check_names(&["PATH".to_string(), forbidden.to_string()], &[], &[]).unwrap_err();
        assert_eq!(error.kind, "forbidden_env_name", "{forbidden}");
        assert!(error.message.contains(forbidden), "{}", error.message);
        let error = check_names(&[], &[(forbidden.to_string(), "v".into())], &[]).unwrap_err();
        assert_eq!(error.kind, "forbidden_env_name", "set {forbidden}");
    }
}

#[test]
fn a_name_delega_itself_sets_is_refused_in_either_form() {
    let adapter = [(
        "CLAUDE_CODE_DISABLE_BACKGROUND_TASKS".to_string(),
        "1".to_string(),
    )];
    let name = "CLAUDE_CODE_DISABLE_BACKGROUND_TASKS";
    for (pass, set) in [
        (vec![name.to_string()], vec![]),
        (vec![], vec![(name.to_string(), "0".to_string())]),
    ] {
        let error = check_names(&pass, &set, &adapter).unwrap_err();
        assert_eq!(error.kind, "forbidden_env_name");
        assert!(
            error.message.contains(name) && error.message.contains("set by delega"),
            "{}",
            error.message
        );
    }
}

#[test]
fn a_locale_category_passes_but_a_name_that_only_starts_with_lc_does_not() {
    let env = parent(&[
        "LC_ALL",
        "LC_TIME",
        "LANGUAGE",
        "LC_SECRET_TOKEN",
        "LC_ANYTHING",
    ]);
    for provider in [Provider::Claude, Provider::Codex] {
        assert_eq!(
            names_for(provider, &env, &[]),
            ["LANGUAGE", "LC_ALL", "LC_TIME"]
        );
    }
}

#[test]
fn a_name_given_in_both_forms_is_set_and_noted_only_as_set() {
    let env = parent(&["PATH", "MY_DEPLOY_TOKEN"]);
    let resolved = resolve(&EnvInputs {
        provider: Provider::Claude,
        parent: &env,
        pass: &["MY_DEPLOY_TOKEN".to_string(), "ONLY_SET_TOKEN".to_string()],
        set: &[
            ("MY_DEPLOY_TOKEN".into(), "v".into()),
            ("ONLY_SET_TOKEN".into(), "v".into()),
        ],
        adapter: &[],
    });
    let report = resolved.report();
    assert!(report.named.is_empty(), "{report:?}");
    assert!(report.named_not_set.is_empty(), "{report:?}");
    assert_eq!(report.set_by_caller, ["MY_DEPLOY_TOKEN", "ONLY_SET_TOKEN"]);
    let notes = resolved.notes();
    assert!(notes.iter().all(|n| !n.contains("was passed")), "{notes:?}");
    assert_eq!(
        notes
            .iter()
            .filter(|n| n.contains("MY_DEPLOY_TOKEN"))
            .count(),
        1
    );
    assert!(
        notes
            .iter()
            .any(|n| n.contains("MY_DEPLOY_TOKEN was set to a value"))
    );
}

#[test]
fn a_provider_owned_name_given_with_env_is_described_as_the_providers() {
    let env = parent(&["PATH", "ANTHROPIC_API_KEY", "HOME"]);
    let resolved = resolved(Provider::Claude, &env, &["ANTHROPIC_API_KEY", "HOME"]);
    let report = resolved.report();
    assert_eq!(report.provider, ["ANTHROPIC_API_KEY"]);
    assert!(report.named.is_empty(), "{report:?}");
    assert!(
        resolved
            .notes()
            .iter()
            .all(|n| !n.contains("ANTHROPIC_API_KEY")),
        "{:?}",
        resolved.notes()
    );
}

#[test]
fn the_values_of_passed_secret_looking_variables_are_redacted_even_when_not_utf8() {
    use std::os::unix::ffi::OsStringExt;
    let bad = OsString::from_vec(b"tok\xffen-value-123".to_vec());
    let env = vec![(OsString::from("MY_SERVICE_TOKEN"), bad.clone())];
    let resolved = resolved(Provider::Claude, &env, &["MY_SERVICE_TOKEN"]);
    assert_eq!(
        resolved.secret_values(Provider::Claude),
        [bad.to_string_lossy().into_owned()]
    );
}

#[test]
fn a_refused_pass_stops_a_dry_run_with_the_refusal() {
    let rig = Rig::new();
    for (name, kind) in [
        ("1BAD", "invalid_env_name"),
        ("DELEGA_CODEX_BIN", "forbidden_env_name"),
    ] {
        let mut options: ProviderOptions = rig.options(Provider::Codex);
        options.env_pass = vec![name.to_string()];
        let error = dry_run(&rig.request(Provider::Codex, "p"), &options).unwrap_err();
        assert_eq!(error.kind, kind);
    }
    let rig = Rig::new();
    let mut options: ProviderOptions = rig.options(Provider::Claude);
    options.env_set = vec![("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS".into(), "0".into())];
    let error = dry_run(&rig.request(Provider::Claude, "p"), &options).unwrap_err();
    assert_eq!(error.kind, "forbidden_env_name");
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
fn ownership_and_session_rules_are_stated_by_name() {
    assert!(owned_by(Provider::Claude, "ANTHROPIC_API_KEY"));
    assert!(!owned_by(Provider::Claude, "OPENAI_API_KEY"));
    assert!(owned_by(Provider::Codex, "OPENAI_API_KEY"));
    assert!(!owned_by(Provider::Codex, "CLAUDE_CONFIG_DIR"));
    assert!(is_calling_session("CLAUDE_CODE_MESSAGING_SOCKET"));
    assert!(is_calling_session("CLAUDE_CODE_MESSAGING_TOKEN"));
    assert!(is_calling_session("CLAUDE_CODE_BRIDGE_SESSION_ID"));
    assert!(is_calling_session("CLAUDE_CODE_SSE_PORT"));
    assert!(is_calling_session("CLAUDE_CODE_IDE_SOCKET"));
    assert!(is_calling_session("CODEX_THREAD_ID"));
    assert!(!is_calling_session(
        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"
    ));
    assert!(!is_calling_session("CODEX_HOME"));
    assert!(!is_calling_session("OPENAI_BASE_URL"));
    assert!(!is_calling_session("CLAUDE_CODE_OAUTH_TOKEN"));
}

const SESSION: [&str; 18] = [
    "CLAUDECODE",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_EFFORT",
    "CLAUDE_PID",
    "AI_AGENT",
    "CLAUDE_CODE_SSE_PORT",
    "CODEX_SANDBOX",
    "CODEX_SANDBOX_NETWORK_DISABLED",
    "CODEX_SESSION_ID",
    "CODEX_THREAD_ID",
    "CODEX_TOOL_CALL_ID",
    "CODEX_CI",
    "CODEX_VERSION",
    "CODEX_APPLY_PATCH_PRESERVE_LINE_ENDINGS",
];
const CONFIG: [&str; 8] = [
    "CODEX_HOME",
    "OPENAI_BASE_URL",
    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
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
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_CONFIG_DIR",
            "PATH"
        ]
    );
    let codex = names_for(Provider::Codex, &env, &[]);
    for name in SESSION {
        assert!(!codex.contains(&name.to_string()), "codex {name}");
    }
    assert_eq!(
        codex,
        ["CODEX_HOME", "OPENAI_API_KEY", "OPENAI_BASE_URL", "PATH"]
    );
}

#[test]
fn a_session_or_delega_variable_is_withheld_even_when_named() {
    let env = parent(&[
        "PATH",
        "CLAUDE_CODE_SESSION_ID",
        "AI_AGENT",
        "DELEGA_CLAUDE_BIN",
    ]);
    assert_eq!(
        names_for(
            Provider::Claude,
            &env,
            &["AI_AGENT", "CLAUDE_CODE_SESSION_ID", "DELEGA_CLAUDE_BIN"]
        ),
        ["PATH"]
    );
}

async fn session_variables_do_not_reach_the_child(provider: Provider) {
    let rig = Rig::new();
    let mut options = rig.options(provider);
    let mut env = options.parent_env.take().unwrap();
    for name in SESSION.iter().chain(&["LC_ALL"]) {
        env.push((OsString::from(name), OsString::from(CANARY)));
    }
    options.parent_env = Some(env);
    run_checked(rig.request(provider, "success"), options).await;
    let seen = rig.env_names();
    for name in SESSION {
        assert!(!seen.contains(&name.to_string()), "{name}: {seen:?}");
    }
    assert!(seen.contains(&"HOME".to_string()), "{seen:?}");
    // The control is on the allowlist, so a rule that withholds too much
    // fails here.
    assert!(seen.contains(&"LC_ALL".to_string()), "{seen:?}");
    rig.assert_nothing_left().await;
}
both!(
    withholds_the_calling_sessions_variables_from_the_child,
    session_variables_do_not_reach_the_child
);

/// The names a shell adds to its own environment before the fake records it.
const SHELL_ADDS: [&str; 4] = ["PWD", "OLDPWD", "SHLVL", "_"];

/// Names the fake received, less what the shell itself adds.
fn child_names(rig: &Rig) -> Vec<String> {
    let mut seen = rig.env_names();
    seen.retain(|n| !SHELL_ADDS.contains(&n.as_str()));
    seen.sort();
    seen
}

fn sorted(mut names: Vec<&str>) -> Vec<String> {
    names.sort_unstable();
    names.dedup();
    names.into_iter().map(String::from).collect()
}

async fn only_allowlisted_names_reach_the_child(provider: Provider) {
    let own_name = match provider {
        Provider::Claude => "ANTHROPIC_API_KEY",
        Provider::Codex => "OPENAI_API_KEY",
    };
    let other_name = match provider {
        Provider::Claude => "OPENAI_API_KEY",
        Provider::Codex => "ANTHROPIC_API_KEY",
    };
    let allowed = ["LANG", "LC_ALL", "HTTPS_PROXY", "no_proxy", own_name];
    let withheld = [
        "MY_SERVICE_API_TOKEN",
        "EDITOR",
        "JAVA_HOME",
        "DATABASE_URL",
        "SSH_AUTH_SOCK",
        "GH_TOKEN",
        "GIT_AUTHOR_NAME",
        "NODE_OPTIONS",
        "LC_SECRET_TOKEN",
        other_name,
        "DELEGA_CLAUDE_BIN",
        "CLAUDECODE",
        "CLAUDE_CODE_SSE_PORT",
        "CODEX_THREAD_ID",
        "CODEX_SANDBOX",
    ];

    let rig = Rig::new();
    let mut options = rig.options(provider);
    let mut env = options.parent_env.take().unwrap();
    for name in allowed.iter().chain(&withheld) {
        env.push((OsString::from(name), OsString::from(CANARY)));
    }
    options.parent_env = Some(env.clone());
    let result = run_checked(rig.request(provider, "success"), options.clone()).await;
    // The exact set: the rig's own variables, what the rig sets, the adapter's
    // and the allowed names above. Anything else is a leak.
    let mut expected = vec![
        "HOME",
        "PATH",
        "FAKE_RECORD_DIR",
        "XDG_CONFIG_HOME",
        "CLAUDE_CONFIG_DIR",
        "CODEX_HOME",
    ];
    if provider == Provider::Claude {
        expected.push("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS");
    }
    expected.extend(allowed);
    let seen = child_names(&rig);
    assert_eq!(seen, sorted(expected));
    let note = format!(
        "{} environment variables of the caller were withheld; {} were passed",
        withheld.len(),
        seen.len()
    );
    assert!(result.provider_notes.contains(&note), "{note}");
    let shown = format!("{result:?} {options:?}");
    assert!(
        !shown.contains(CANARY),
        "no environment value in diagnostics"
    );
    rig.assert_nothing_left().await;

    // Naming a withheld variable passes it, and the result says its name
    // looks like a secret.
    let rig = Rig::new();
    let mut options = rig.options(provider);
    options.parent_env = Some(env);
    options.env_pass = vec!["MY_SERVICE_API_TOKEN".into(), "JAVA_HOME".into()];
    let result = run_checked(rig.request(provider, "success"), options).await;
    let seen = child_names(&rig);
    assert!(seen.contains(&"MY_SERVICE_API_TOKEN".to_string()));
    assert!(seen.contains(&"JAVA_HOME".to_string()));
    assert!(!seen.contains(&"EDITOR".to_string()));
    assert!(
        result
            .provider_notes
            .iter()
            .any(|n| { n.contains("MY_SERVICE_API_TOKEN") && n.contains("looks like a secret") })
    );
    assert!(
        !result
            .provider_notes
            .iter()
            .any(|n| n.contains("JAVA_HOME"))
    );
    assert!(!format!("{result:?}").contains(CANARY));
    rig.assert_nothing_left().await;
}
both!(
    passes_only_allowlisted_and_named_variables_to_the_child,
    only_allowlisted_names_reach_the_child
);

async fn a_refused_pass_starts_nothing(provider: Provider) {
    let rig = Rig::new();
    let mut options = rig.options(provider);
    options.env_pass = vec!["BAD=value".into()];
    let result = run_checked(rig.request(provider, "success"), options).await;
    assert_eq!(result.outcome, crate::contract::outcome::Outcome::Refused);
    assert_eq!(result.error.expect("error").kind, "invalid_env_name");
    assert!(!rig.record.join("pid").exists());
}
both!(
    refuses_a_bad_pass_name_before_starting,
    a_refused_pass_starts_nothing
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
