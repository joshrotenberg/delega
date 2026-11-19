//! Attribution of a stop to a denial, terminal events, decoding, redaction and
//! the smaller notes.

use std::os::unix::fs::PermissionsExt;

use super::{Rig, both, run_checked};
use crate::contract::outcome::Outcome;
use crate::contract::request::Provider;
use crate::provider::dry_run;
use crate::provider::redact::Redactor;

const CANARY: &str = "canary-secret-value-123456";

async fn denial_that_stopped_the_run_is_refused(provider: Provider) {
    let rig = Rig::new();
    let result = run_checked(
        rig.request(provider, "denied-refused"),
        rig.options(provider),
    )
    .await;
    assert_eq!(result.outcome, Outcome::Refused, "{result:#?}");
    let error = result.error.expect("error");
    assert_eq!(error.kind, "permission_denied");
    assert_eq!(error.provider_exit_code, Some(0), "the exit code is kept");
    rig.assert_nothing_left().await;
}
both!(
    refuses_after_start_when_the_failure_follows_a_denial,
    denial_that_stopped_the_run_is_refused
);

#[tokio::test]
async fn claude_turn_limit_with_a_denial_is_failed_with_the_limit_kind_and_the_denial_listed() {
    let rig = Rig::new();
    let request = rig.request(Provider::Claude, "error-max-turns-with-denial");
    let result = run_checked(request, rig.options(Provider::Claude)).await;
    assert_eq!(result.outcome, Outcome::Failed, "{result:#?}");
    let error = result.error.expect("error");
    assert_eq!(error.kind, "provider_turn_limit");
    assert_eq!(error.provider_exit_code, Some(0));
    assert_eq!(result.permissions.denied.len(), 1);
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn claude_connection_error_with_a_denial_is_failed_not_refused() {
    let rig = Rig::new();
    let request = rig.request(Provider::Claude, "denied-with-connection-error");
    let result = run_checked(request, rig.options(Provider::Claude)).await;
    assert_eq!(result.outcome, Outcome::Failed, "{result:#?}");
    assert_eq!(result.error.expect("error").kind, "provider_error");
    assert_eq!(result.permissions.denied.len(), 1);
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn codex_error_after_a_denial_is_failed_not_refused() {
    let rig = Rig::new();
    let request = rig.request(Provider::Codex, "denied-then-error");
    let result = run_checked(request, rig.options(Provider::Codex)).await;
    assert_eq!(result.outcome, Outcome::Failed, "{result:#?}");
    assert_eq!(result.error.expect("error").provider_exit_code, Some(0));
    assert_eq!(result.permissions.denied.len(), 1);
    rig.assert_nothing_left().await;
}

#[tokio::test]
async fn a_failed_terminal_event_followed_by_success_stays_failed_and_says_so() {
    let rig = Rig::new();
    let result = run_checked(
        rig.request(Provider::Claude, "failed-then-success"),
        rig.options(Provider::Claude),
    )
    .await;
    assert_eq!(result.outcome, Outcome::Failed, "{result:#?}");
    assert!(
        result
            .error
            .expect("error")
            .message
            .contains("First failure")
    );
    assert!(
        result
            .provider_notes
            .iter()
            .any(|n| n.contains("second terminal event"))
    );
    rig.assert_nothing_left().await;

    let rig = Rig::new();
    let result = run_checked(
        rig.request(Provider::Codex, "failed-then-completed"),
        rig.options(Provider::Codex),
    )
    .await;
    assert_eq!(result.outcome, Outcome::Failed, "{result:#?}");
    assert_eq!(result.text, "", "the answer after the failure is not used");
    assert!(
        result
            .provider_notes
            .iter()
            .any(|n| n.contains("second terminal event"))
    );
    rig.assert_nothing_left().await;
}

async fn invalid_utf8(provider: Provider) {
    let rig = Rig::new();
    let result = run_checked(rig.request(provider, "invalid-utf8"), rig.options(provider)).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    assert_eq!(result.text, "bad \u{FFFD} byte");
    assert!(
        result
            .provider_notes
            .iter()
            .any(|n| n.contains("invalid UTF-8"))
    );
    rig.assert_nothing_left().await;
}
both!(
    decodes_invalid_utf8_with_replacement_and_says_so,
    invalid_utf8
);

async fn stderr_is_redacted(provider: Provider) {
    let rig = Rig::new();
    let mut options = rig.options(provider);
    options
        .env_set
        .push(("MY_SERVICE_API_TOKEN".into(), CANARY.into()));
    let result = run_checked(rig.request(provider, "stderr-secret"), options).await;
    assert_eq!(result.outcome, Outcome::Failed, "{result:#?}");
    let error = result.error.as_ref().expect("error");
    assert_eq!(error.kind, "provider_exit");
    assert!(
        error.message.contains("boom token=[redacted]"),
        "{}",
        error.message
    );
    for secret in [
        CANARY,
        "sk-ant-abcdefghijklmnopqrstuvwx",
        "abcdefghijklmnop123",
    ] {
        assert!(!error.message.contains(secret), "{}", error.message);
        assert!(!format!("{result:?}").contains(secret));
    }
    rig.assert_nothing_left().await;
}
both!(
    redacts_secrets_from_provider_stderr_in_the_error,
    stderr_is_redacted
);

#[test]
fn the_redactor_removes_values_and_credential_shapes_and_nothing_else() {
    let redactor = Redactor::new([CANARY.to_string(), "short".to_string()]);
    let text = format!(
        "key={CANARY} short word; sk-ant-abcdefghijklmnopqrstuv; sk-proj-abcdefghijklmnopqrstuv; \
         ghp_abcdefghijklmnopqrstuv; github_pat_abcdefghijklmnopqrstuv; xoxb-1234567890-abc; \
         AKIAABCDEFGHIJKLMNOP; eyJhbGciOi.eyJzdWIiOiIx.c2lnbmF0dXJl; Authorization: Bearer abcdefghijkl1234; \
         -----BEGIN RSA PRIVATE KEY-----\nMIIB\n-----END RSA PRIVATE KEY-----\nafter"
    );
    let out = redactor.apply(&text);
    for gone in [
        CANARY,
        "sk-ant-abc",
        "sk-proj-abc",
        "ghp_abc",
        "github_pat_abc",
        "xoxb-1234",
        "AKIAABCD",
        "eyJhbGci",
        "abcdefghijkl1234",
        "MIIB",
    ] {
        assert!(!out.contains(gone), "{gone}: {out}");
    }
    assert!(
        out.contains("short word"),
        "short values are not removed: {out}"
    );
    assert!(out.contains("after"), "{out}");
    assert!(out.contains("Authorization: Bearer [redacted]"), "{out}");
    let plain = "an ordinary sentence with sk-short and Bearer tiny and a-token";
    assert_eq!(redactor.apply(plain), plain);
}

#[test]
fn only_secret_looking_and_provider_owned_values_are_collected_for_redaction() {
    use crate::provider::env::{EnvInputs, resolve};
    use std::ffi::OsString;
    let parent: Vec<(OsString, OsString)> = [
        ("PATH", "/usr/bin"),
        ("MY_PLAIN_SETTING", "plain-value-here"),
        ("ANTHROPIC_API_KEY", "anthropic-secret-value"),
    ]
    .iter()
    .map(|(n, v)| (OsString::from(n), OsString::from(v)))
    .collect();
    let env = resolve(&EnvInputs {
        provider: Provider::Claude,
        parent: &parent,
        pass: &[],
        set: &[("MY_EXPLICIT_TOKEN".into(), "explicit-token-value".into())],
        adapter: &[],
    });
    let mut values = env.secret_values(Provider::Claude);
    values.sort();
    assert_eq!(values, ["anthropic-secret-value", "explicit-token-value"]);
}

#[tokio::test]
async fn claude_denial_reasons_from_tool_errors_are_kept() {
    let rig = Rig::new();
    let request = rig.request(Provider::Claude, "denied-completed");
    let result = run_checked(request, rig.options(Provider::Claude)).await;
    assert_eq!(
        result.permissions.denied[0].request,
        "Bash: rm -rf /tmp/never (Permission to use Bash has been denied.)"
    );
    rig.assert_nothing_left().await;
}

#[test]
fn every_claude_run_says_settings_and_deny_rules_were_not_applied() {
    let rig = Rig::new();
    for mode in [
        crate::contract::permissions::Mode::Read,
        crate::contract::permissions::Mode::Write,
        crate::contract::permissions::Mode::Full,
    ] {
        let mut request = rig.request(Provider::Claude, "p");
        request.permissions.mode = mode;
        let plan = dry_run(&request, &rig.options(Provider::Claude)).unwrap();
        assert!(
            plan.notes
                .iter()
                .any(|n| n.contains("including their deny rules, were not applied")),
            "{mode:?}"
        );
        assert!(
            plan.notes
                .iter()
                .any(|n| n.contains("reference directories do not contribute"))
        );
        assert!(
            !plan
                .env_names
                .contains(&"CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD".to_string())
        );
    }
}

#[test]
fn every_codex_read_and_write_run_says_what_the_sandbox_allows_beyond_the_request() {
    let rig = Rig::new();
    for mode in [
        crate::contract::permissions::Mode::Read,
        crate::contract::permissions::Mode::Write,
    ] {
        let mut request = rig.request(Provider::Codex, "p");
        request.permissions.mode = mode;
        let plan = dry_run(&request, &rig.options(Provider::Codex)).unwrap();
        let note = plan
            .notes
            .iter()
            .find(|n| n.starts_with("wider than requested"))
            .unwrap();
        assert!(note.contains("read any file this user can read"), "{note}");
        assert!(
            note.contains("permissions.commands is not enforced as a list"),
            "{note}"
        );
    }
}

#[test]
fn a_working_directory_of_root_or_home_gets_a_warning_and_is_not_refused() {
    let rig = Rig::new();
    let mut request = rig.request(Provider::Claude, "p");
    request.cwd = Some("/".into());
    let plan = dry_run(&request, &rig.options(Provider::Claude)).unwrap();
    assert!(plan.notes.iter().any(|n| n.contains("filesystem root")));

    let mut request = rig.request(Provider::Codex, "p");
    request.cwd = Some(rig.home.clone());
    let plan = dry_run(&request, &rig.options(Provider::Codex)).unwrap();
    assert!(plan.notes.iter().any(|n| n.contains("the home directory")));

    let plan = dry_run(
        &rig.request(Provider::Codex, "p"),
        &rig.options(Provider::Codex),
    )
    .unwrap();
    assert!(
        !plan
            .notes
            .iter()
            .any(|n| n.contains("covers everything below it"))
    );
}

#[test]
fn a_file_without_execute_permission_is_not_an_executable() {
    let rig = Rig::new();
    let path = rig.home.join("claude");
    std::fs::write(&path, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let mut options = rig.options(Provider::Claude);
    options.executable = Some(path.clone());
    let error = dry_run(&rig.request(Provider::Claude, "p"), &options).unwrap_err();
    assert_eq!(error.kind, "provider_not_found");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(dry_run(&rig.request(Provider::Claude, "p"), &options).is_ok());
}

#[test]
fn a_long_rejected_option_is_named_only_in_part() {
    let long = format!("--{}", "x".repeat(300));
    let error = crate::provider::native_args::check(Provider::Claude, std::slice::from_ref(&long))
        .unwrap_err();
    assert!(!error.message.contains(&long));
    assert!(error.message.len() < 200, "{}", error.message);
}

#[tokio::test]
async fn claude_denial_with_an_unrelated_error_or_further_work_is_failed_not_refused() {
    for mode in ["denial-api-error", "denial-then-work"] {
        let rig = Rig::new();
        let result = run_checked(
            rig.request(Provider::Claude, mode),
            rig.options(Provider::Claude),
        )
        .await;
        assert_eq!(result.outcome, Outcome::Failed, "{mode}: {result:#?}");
        assert_eq!(
            result.error.expect("error").kind,
            "provider_error",
            "{mode}"
        );
        assert_eq!(result.permissions.denied.len(), 1, "{mode}");
        rig.assert_nothing_left().await;
    }
}

#[tokio::test]
async fn codex_work_after_a_declined_command_or_a_named_limit_is_failed_not_refused() {
    let rig = Rig::new();
    let request = rig.request(Provider::Codex, "denied-then-file-change");
    let result = run_checked(request, rig.options(Provider::Codex)).await;
    assert_eq!(result.outcome, Outcome::Failed, "{result:#?}");
    assert_eq!(result.error.expect("error").kind, "provider_error");
    assert_eq!(result.permissions.denied.len(), 1);
    rig.assert_nothing_left().await;

    let rig = Rig::new();
    let request = rig.request(Provider::Codex, "denied-then-usage-limit");
    let result = run_checked(request, rig.options(Provider::Codex)).await;
    assert_eq!(result.outcome, Outcome::Failed, "{result:#?}");
    assert_eq!(result.error.expect("error").kind, "provider_usage_limit");
    assert_eq!(result.permissions.denied.len(), 1);
    rig.assert_nothing_left().await;
}

#[test]
fn credentials_are_redacted_after_equals_colon_quotes_commas_and_in_urls_and_json() {
    let redactor = Redactor::default();
    let ghp = "ghp_abcdefghijklmnopqrstuvwx";
    let ant = "sk-ant-abcdefghijklmnopqrstuvwx";
    for text in [
        format!("token={ghp}"),
        format!("key={ant}"),
        format!("token:{ghp}"),
        format!("token: \"{ghp}\""),
        format!("a,{ghp},b"),
        format!("https://user:{ghp}@example.com/path"),
        format!("https://example.com/{ghp}"),
        format!("{{\"token\":\"{ghp}\",\"key\":\"{ant}\"}}"),
        format!("export KEY='{ant}'"),
        format!("Authorization: Bearer {ghp}=="),
        "Bearer abc/def+ghi=jkl".to_string(),
    ] {
        let out = redactor.apply(&text);
        assert!(out.contains("[redacted]"), "{text} -> {out}");
        assert!(!out.contains(ghp) && !out.contains(ant), "{text} -> {out}");
        assert!(!out.contains("def+ghi"), "{text} -> {out}");
    }
    assert_eq!(
        redactor.apply("token=short key=value"),
        "token=short key=value"
    );
}

/// CPU time used so far by the calling thread. Unlike a clock on the wall, it
/// does not grow while the thread waits for a core, so a busy machine does not
/// stretch one measurement against another.
#[cfg(unix)]
fn thread_cpu_time() -> std::time::Duration {
    let t = rustix::time::clock_gettime(rustix::time::ClockId::ThreadCPUTime);
    std::time::Duration::new(
        u64::try_from(t.tv_sec).unwrap_or(0),
        u32::try_from(t.tv_nsec).unwrap_or(0),
    )
}

/// Proves the scan is linear without a wall-clock bound: the same input at
/// eight times the size must cost far less than eight times eight (quadratic
/// growth), measured in CPU time of the thread that runs the scan, so waiting
/// for a core under load is not counted. The best of three runs per size drops
/// a disturbance that hit only one run. Linear growth gives a ratio near 8 and
/// quadratic growth near 64; the limit of 24 sits between them.
#[cfg(unix)]
#[test]
fn private_key_removal_is_linear_in_the_input() {
    let redactor = Redactor::default();
    let best = |repeats: usize| {
        // Many begin markers that are not keys, and many unterminated labels.
        let noise = "-----BEGIN CERTIFICATE".repeat(repeats);
        let mut best = std::time::Duration::MAX;
        for _ in 0..3 {
            let started = thread_cpu_time();
            let out = redactor.apply(&noise);
            best = best.min(thread_cpu_time().saturating_sub(started));
            assert_eq!(out.len(), noise.len());
        }
        best
    };
    let small = best(12_500).max(std::time::Duration::from_micros(1));
    let large = best(100_000);
    let ratio = large.as_secs_f64() / small.as_secs_f64();
    assert!(
        ratio < 24.0,
        "8x the input took {ratio:.1}x the CPU time ({small:?} vs {large:?}); linear is about 8, quadratic about 64"
    );
    let keys = "-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----\n".repeat(50_000);
    let out = redactor.apply(&keys);
    assert_eq!(out, "[redacted]\n".repeat(50_000));
}
