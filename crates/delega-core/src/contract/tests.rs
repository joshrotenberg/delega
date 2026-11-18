use std::path::PathBuf;

use serde_json::{Value, json};

use super::SCHEMA_VERSION;
use super::limits::*;
use super::outcome::Outcome;
use super::permissions::{Mode, OnRequest, Permissions};
use super::request::{Effort, Provider, Request, Session};
use super::result::*;

fn full_request() -> Request {
    Request {
        schema_version: SCHEMA_VERSION,
        provider: Provider::Codex,
        prompt: "Summarize this repository".into(),
        cwd: Some(PathBuf::from("/work/repo")),
        name: Some("summarizer".into()),
        instructions: Some("Be brief.".into()),
        model: Some("gpt-5".into()),
        effort: Some(Effort::High),
        timeout: 1200,
        permissions: Permissions {
            mode: Mode::Write,
            commands: vec!["cargo test".into()],
            directories: vec![PathBuf::from("/work/other")],
            on_request: OnRequest::Deny,
        },
        answer_schema: Some(json!({"type": "object"})),
        session: Session {
            ephemeral: false,
            resume: Some("sess-1".into()),
        },
        native_args: vec!["--flag".into()],
    }
}

fn full_result() -> RunResult {
    RunResult {
        schema_version: SCHEMA_VERSION,
        run: "run-1".into(),
        worker: WorkerRef {
            id: "w-1".into(),
            name: Some("summarizer".into()),
        },
        provider: Provider::Claude,
        outcome: Outcome::Failed,
        text: "partial".into(),
        structured: Some(json!({"a": 1})),
        structured_valid: Some(false),
        error: Some(ErrorDetail {
            kind: "provider_exit".into(),
            message: "exited".into(),
            provider_exit_code: Some(3),
        }),
        cleanup: Cleanup {
            confirmed: false,
            pids: vec![10, 11],
            process_groups: vec![10],
            reason: Some("permission denied".into()),
        },
        permissions: AppliedPermissions {
            mode: Mode::Read,
            denied: vec![DeniedRequest {
                request: "run ls".into(),
            }],
        },
        provider_notes: vec!["no sandbox".into()],
        session_id: Some("sess-1".into()),
        usage: Some(Usage {
            input_tokens: Some(1),
            output_tokens: Some(2),
            cached_tokens: Some(3),
            reasoning_tokens: Some(4),
            cost_usd: Some(0.5),
        }),
        timing: Timing {
            queued_at: "2026-11-18T10:00:00Z".into(),
            started_at: Some("2026-11-18T10:00:01Z".into()),
            finished_at: "2026-11-18T10:00:05Z".into(),
            duration_ms: 4000,
            time_limit_secs: 600,
        },
        provenance: Provenance {
            delega_version: "0.1.0".into(),
            build: Some("abc123".into()),
            provider_executable: Some("/usr/bin/codex".into()),
            provider_version: Some("1.0".into()),
        },
    }
}

fn problem_fields(request: &Request) -> Vec<String> {
    request
        .validate()
        .err()
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.field)
        .collect()
}

fn minimal() -> Request {
    Request::new(Provider::Claude, "do it")
}

#[test]
fn full_request_survives_a_json_round_trip() {
    let request = full_request();
    let text = serde_json::to_string(&request).unwrap();
    let back: Request = serde_json::from_str(&text).unwrap();
    assert_eq!(back, request);
}

#[test]
fn full_result_survives_a_json_round_trip() {
    let result = full_result();
    let text = serde_json::to_string(&result).unwrap();
    let back: RunResult = serde_json::from_str(&text).unwrap();
    assert_eq!(back, result);
}

#[test]
fn request_with_an_unknown_field_is_rejected() {
    let text = r#"{"schema_version":1,"provider":"claude","prompt":"x","promt":"typo"}"#;
    let err = serde_json::from_str::<Request>(text).unwrap_err();
    assert!(err.to_string().contains("unknown field"), "{err}");
}

#[test]
fn unknown_field_inside_permissions_is_rejected() {
    let text =
        r#"{"schema_version":1,"provider":"claude","prompt":"x","permissions":{"mod":"write"}}"#;
    assert!(serde_json::from_str::<Request>(text).is_err());
}

#[test]
fn result_with_an_unknown_field_is_still_read() {
    let mut value = serde_json::to_value(full_result()).unwrap();
    value["added_later"] = json!(true);
    assert!(serde_json::from_value::<RunResult>(value).is_ok());
}

#[test]
fn unknown_provider_fails_to_parse() {
    let text = r#"{"schema_version":1,"provider":"gemini","prompt":"x"}"#;
    assert!(serde_json::from_str::<Request>(text).is_err());
}

#[test]
fn request_without_a_schema_version_fails_to_parse() {
    let text = r#"{"provider":"claude","prompt":"x"}"#;
    assert!(serde_json::from_str::<Request>(text).is_err());
}

#[test]
fn omitted_fields_get_the_documented_defaults() {
    let text = r#"{"schema_version":1,"provider":"claude","prompt":"x"}"#;
    let request: Request = serde_json::from_str(text).unwrap();
    assert_eq!(request.timeout, 600);
    assert_eq!(request.timeout, DEFAULT_TIMEOUT_SECS);
    assert_eq!(request.permissions.mode, Mode::Read);
    assert_eq!(request.permissions.on_request, OnRequest::Deny);
    assert!(request.permissions.commands.is_empty());
    assert!(request.session.is_default());
    assert_eq!(request, minimal_with_prompt("x"));
}

fn minimal_with_prompt(prompt: &str) -> Request {
    Request::new(Provider::Claude, prompt)
}

#[test]
fn absent_optional_fields_are_left_out_of_the_json() {
    let value = serde_json::to_value(minimal()).unwrap();
    let keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let mut keys = keys;
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "permissions",
            "prompt",
            "provider",
            "schema_version",
            "timeout"
        ]
    );
}

#[test]
fn enums_serialize_as_snake_case_strings() {
    assert_eq!(
        serde_json::to_value(Provider::Codex).unwrap(),
        json!("codex")
    );
    assert_eq!(
        serde_json::to_value(Effort::Medium).unwrap(),
        json!("medium")
    );
    assert_eq!(serde_json::to_value(Mode::Full).unwrap(), json!("full"));
    assert_eq!(
        serde_json::to_value(OnRequest::Deny).unwrap(),
        json!("deny")
    );
    assert_eq!(
        serde_json::to_value(Outcome::TimedOut).unwrap(),
        json!("timed_out")
    );
}

#[test]
fn a_minimal_request_is_valid() {
    assert_eq!(minimal().validate(), Ok(()));
}

#[test]
fn a_full_request_is_valid() {
    assert_eq!(full_request().validate(), Ok(()));
}

#[test]
fn unknown_schema_version_is_a_validation_problem() {
    let mut r = minimal();
    r.schema_version = SCHEMA_VERSION + 1;
    assert_eq!(problem_fields(&r), ["schema_version"]);
}

#[test]
fn empty_or_blank_prompt_is_a_validation_problem() {
    for prompt in ["", "  \n\t"] {
        let mut r = minimal();
        r.prompt = prompt.into();
        assert_eq!(problem_fields(&r), ["prompt"]);
    }
}

#[test]
fn oversized_prompt_is_a_validation_problem() {
    let mut r = minimal();
    r.prompt = "x".repeat(MAX_PROMPT_BYTES + 1);
    assert_eq!(problem_fields(&r), ["prompt"]);
}

#[test]
fn zero_timeout_is_a_validation_problem() {
    let mut r = minimal();
    r.timeout = 0;
    assert_eq!(problem_fields(&r), ["timeout"]);
}

#[test]
fn timeout_above_the_maximum_is_a_validation_problem() {
    let mut r = minimal();
    r.timeout = MAX_TIMEOUT_SECS + 1;
    assert_eq!(problem_fields(&r), ["timeout"]);
    r.timeout = MAX_TIMEOUT_SECS;
    assert_eq!(r.validate(), Ok(()));
}

#[test]
fn empty_cwd_is_a_validation_problem() {
    let mut r = minimal();
    r.cwd = Some(PathBuf::new());
    assert_eq!(problem_fields(&r), ["cwd"]);
}

#[test]
fn bad_worker_names_are_validation_problems() {
    for name in ["", "   ", "a\nb", &"n".repeat(MAX_NAME_CHARS + 1)] {
        let mut r = minimal();
        r.name = Some(name.to_string());
        assert_eq!(problem_fields(&r), ["name"], "name {name:?}");
    }
}

#[test]
fn empty_model_is_a_validation_problem() {
    let mut r = minimal();
    r.model = Some(String::new());
    assert_eq!(problem_fields(&r), ["model"]);
}

#[test]
fn oversized_instructions_are_a_validation_problem() {
    let mut r = minimal();
    r.instructions = Some("i".repeat(MAX_INSTRUCTIONS_BYTES + 1));
    assert_eq!(problem_fields(&r), ["instructions"]);
}

#[test]
fn empty_command_names_its_index_in_the_problem() {
    let mut r = minimal();
    r.permissions.commands = vec!["ls".into(), " ".into()];
    assert_eq!(problem_fields(&r), ["permissions.commands[1]"]);
}

#[test]
fn too_many_commands_is_a_validation_problem() {
    let mut r = minimal();
    r.permissions.commands = vec!["ls".into(); MAX_COMMANDS + 1];
    assert_eq!(problem_fields(&r), ["permissions.commands"]);
}

#[test]
fn empty_directory_names_its_index_in_the_problem() {
    let mut r = minimal();
    r.permissions.directories = vec![PathBuf::from("/a"), PathBuf::new()];
    assert_eq!(problem_fields(&r), ["permissions.directories[1]"]);
}

#[test]
fn too_many_directories_is_a_validation_problem() {
    let mut r = minimal();
    r.permissions.directories = vec![PathBuf::from("/a"); MAX_DIRECTORIES + 1];
    assert_eq!(problem_fields(&r), ["permissions.directories"]);
}

#[test]
fn answer_schema_that_is_not_a_schema_is_a_validation_problem() {
    for value in [json!("string"), json!(3), json!([1])] {
        let mut r = minimal();
        r.answer_schema = Some(value);
        assert_eq!(problem_fields(&r), ["answer_schema"]);
    }
}

#[test]
fn ephemeral_with_resume_is_a_validation_problem() {
    let mut r = minimal();
    r.session = Session {
        ephemeral: true,
        resume: Some("s".into()),
    };
    assert_eq!(problem_fields(&r), ["session"]);
}

#[test]
fn empty_resume_id_is_a_validation_problem() {
    let mut r = minimal();
    r.session.resume = Some(String::new());
    assert_eq!(problem_fields(&r), ["session.resume"]);
}

#[test]
fn empty_native_arg_names_its_index_in_the_problem() {
    let mut r = minimal();
    r.native_args = vec!["--ok".into(), String::new()];
    assert_eq!(problem_fields(&r), ["native_args[1]"]);
}

#[test]
fn too_many_native_args_is_a_validation_problem() {
    let mut r = minimal();
    r.native_args = vec!["--a".into(); MAX_NATIVE_ARGS + 1];
    assert_eq!(problem_fields(&r), ["native_args"]);
}

#[test]
fn validate_reports_every_problem_not_only_the_first() {
    let mut r = minimal();
    r.prompt = String::new();
    r.timeout = 0;
    r.name = Some(String::new());
    r.permissions.commands = vec![String::new()];
    let fields = problem_fields(&r);
    assert_eq!(
        fields,
        ["prompt", "name", "timeout", "permissions.commands[0]"]
    );
}

#[test]
fn problem_display_names_the_field_and_the_message() {
    let mut r = minimal();
    r.prompt = String::new();
    let problem = r.validate().unwrap_err().remove(0);
    assert_eq!(problem.to_string(), "prompt: must not be empty");
}

#[test]
fn exit_status_follows_the_documented_mapping() {
    assert_eq!(Outcome::Completed.exit_status(), 0);
    assert_eq!(Outcome::Failed.exit_status(), 1);
    assert_eq!(Outcome::Refused.exit_status(), 2);
    assert_eq!(Outcome::TimedOut.exit_status(), 124);
    assert_eq!(Outcome::Interrupted.exit_status(), 130);
}

#[test]
fn every_outcome_has_a_distinct_exit_status() {
    let mut codes: Vec<u8> = Outcome::ALL.iter().map(|o| o.exit_status()).collect();
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), Outcome::ALL.len());
}

#[test]
fn request_schema_forbids_unknown_properties() {
    let value: Value = serde_json::from_str(super::schema::REQUEST_SCHEMA).unwrap();
    assert_eq!(value["additionalProperties"], json!(false));
}
