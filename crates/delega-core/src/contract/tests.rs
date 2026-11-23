use std::path::PathBuf;

use serde_json::{Value, json};

use super::SCHEMA_VERSION;
use super::applied::*;
use super::limits::*;
use super::outcome::Outcome;
use super::partial::PartialRequest;
use super::permissions::{Mode, OnRequest, Permissions};
use super::request::{Effort, Provider, Request, Session};
use super::result::*;
use super::validate::Problem;

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
        timeout_secs: 1200,
        permissions: Permissions {
            mode: Mode::Write,
            commands: vec!["cargo test".into()],
            directories: vec![PathBuf::from("/work/other")],
            on_request: OnRequest::Deny,
            ask_timeout: None,
            strict: true,
        },
        answer_schema: Some(json!({"type": "object"})),
        session: Session {
            ephemeral: false,
            resume: Some("sess-1".into()),
        },
        native_args: vec!["--flag".into()],
    }
}

fn full_applied() -> Applied {
    Applied {
        config: Some(AppliedConfig {
            path: "/home/u/.config/delega/config.toml".into(),
            sha256: "0".repeat(64),
        }),
        sources: [
            ("provider".to_string(), Layer::Flag),
            ("timeout_secs".to_string(), Layer::Default),
            ("permissions.mode".to_string(), Layer::Config),
        ]
        .into(),
        env: Some(AppliedEnv {
            passed: EnvPassed {
                total: 5,
                base: 2,
                network: 1,
                provider: 1,
                named_by_caller: 0,
                set_by_caller: 1,
                set_by_delega: 0,
            },
            withheld: EnvWithheld {
                total: 7,
                never_passed: 1,
                not_allowlisted: 6,
            },
            named: vec!["TOOLCHAIN_HOME".into()],
        }),
        deny_rules: Some(AppliedDenyRules {
            files: vec![DenyFile {
                path: "/home/u/.claude/settings.json".into(),
                rules: 2,
            }],
            skipped: vec![SkippedFile {
                path: "/work/.claude/settings.local.json".into(),
                reason: "not owned by the user".into(),
            }],
            applied: 2,
        }),
        context: Some(AppliedContext {
            settings: "deny_rules".into(),
            instructions: false,
        }),
    }
}

fn full_result() -> RunResult {
    RunResult {
        schema_version: SCHEMA_VERSION,
        run: "run-1".into(),
        worker: Some(WorkerRef {
            id: "w-1".into(),
            name: Some("summarizer".into()),
        }),
        provider: Some(Provider::Claude),
        outcome: Outcome::Failed,
        text: "partial".into(),
        structured: Some(json!({"a": 1})),
        structured_valid: Some(false),
        error: Some(ErrorDetail {
            kind: "provider_exit".into(),
            message: "exited".into(),
            provider_exit_code: Some(3),
        }),
        cleanup: Cleanup::Unconfirmed {
            pids: vec![10, 11],
            process_groups: vec![10],
            reason: "permission denied".into(),
        },
        permissions: AppliedPermissions {
            asked: Vec::new(),
            mode: Mode::Read,
            denied: vec![DeniedRequest {
                request: "run ls".into(),
            }],
        },
        provider_notes: vec!["no sandbox".into()],
        applied: Some(full_applied()),
        session_id: Some("sess-1".into()),
        usage: Some(Usage {
            input_tokens: Some(1),
            output_tokens: Some(2),
            cached_tokens: Some(3),
            reasoning_tokens: Some(4),
            cost_usd: Some(0.5),
        }),
        session_usage: Some(Usage {
            input_tokens: Some(10),
            output_tokens: Some(20),
            cached_tokens: Some(30),
            reasoning_tokens: Some(40),
            cost_usd: Some(1.5),
        }),
        timing: Timing {
            queued_at: "2026-11-18T10:00:00Z".into(),
            started_at: Some("2026-11-18T10:00:01Z".into()),
            finished_at: "2026-11-18T10:00:05Z".into(),
            duration_ms: 4000,
            timeout_secs: 600,
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
fn result_written_before_session_usage_existed_is_still_read() {
    let mut value = serde_json::to_value(full_result()).unwrap();
    value.as_object_mut().unwrap().remove("session_usage");
    let result: RunResult = serde_json::from_value(value).unwrap();
    assert_eq!(result.session_usage, None);
    assert!(result.usage.is_some());
}

#[test]
fn unknown_provider_fails_to_parse() {
    let text = r#"{"schema_version":1,"provider":"gemini","prompt":"x"}"#;
    assert!(serde_json::from_str::<Request>(text).is_err());
}

#[test]
fn request_without_a_schema_version_gets_the_current_one() {
    let text = r#"{"provider":"claude","prompt":"x"}"#;
    let request: Request = serde_json::from_str(text).unwrap();
    assert_eq!(request.schema_version, SCHEMA_VERSION);
}

#[test]
fn unknown_field_inside_session_is_rejected() {
    let text = r#"{"provider":"claude","prompt":"x","session":{"keep":true}}"#;
    assert!(serde_json::from_str::<Request>(text).is_err());
}

#[test]
fn result_reader_ignores_unknown_fields_in_nested_objects() {
    let mut value = serde_json::to_value(full_result()).unwrap();
    value["cleanup"]["added_later"] = json!(1);
    value["timing"]["added_later"] = json!(1);
    value["error"]["added_later"] = json!(1);
    value["permissions"]["added_later"] = json!(1);
    value["provenance"]["added_later"] = json!(1);
    assert!(serde_json::from_value::<RunResult>(value).is_ok());
}

#[test]
fn omitted_fields_get_the_documented_defaults() {
    let text = r#"{"schema_version":1,"provider":"claude","prompt":"x"}"#;
    let request: Request = serde_json::from_str(text).unwrap();
    assert_eq!(request.timeout_secs, 600);
    assert_eq!(request.timeout_secs, DEFAULT_TIMEOUT_SECS);
    assert_eq!(request.permissions.mode, Mode::Read);
    assert_eq!(request.permissions.on_request, OnRequest::Deny);
    assert!(request.permissions.commands.is_empty());
    assert!(request.session.is_default());
    assert_eq!(request, Request::new(Provider::Claude, "x"));
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
            "timeout_secs"
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
    assert_eq!(serde_json::to_value(Mode::Build).unwrap(), json!("build"));
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
    r.timeout_secs = 0;
    assert_eq!(problem_fields(&r), ["timeout_secs"]);
}

#[test]
fn timeout_above_the_maximum_is_a_validation_problem() {
    let mut r = minimal();
    r.timeout_secs = MAX_TIMEOUT_SECS + 1;
    assert_eq!(problem_fields(&r), ["timeout_secs"]);
    r.timeout_secs = MAX_TIMEOUT_SECS;
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
    r.timeout_secs = 0;
    r.name = Some(String::new());
    r.permissions.commands = vec![String::new()];
    let fields = problem_fields(&r);
    assert_eq!(
        fields,
        ["prompt", "name", "timeout_secs", "permissions.commands[0]"]
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
fn build_mode_with_commands_is_a_validation_problem() {
    let mut r = minimal();
    r.permissions.mode = Mode::Build;
    assert_eq!(r.validate(), Ok(()));
    r.permissions.commands = vec!["cargo test".into()];
    assert_eq!(problem_fields(&r), ["permissions.commands"]);
}

#[test]
fn modes_are_ordered_with_build_between_write_and_full() {
    let schema: Value = serde_json::to_value(schemars::schema_for!(Mode)).unwrap();
    let names: Vec<&str> = schema["oneOf"]
        .as_array()
        .expect("variants in schema")
        .iter()
        .map(|v| {
            v["const"]
                .as_str()
                .or_else(|| v["enum"][0].as_str())
                .unwrap()
        })
        .collect();
    assert_eq!(names, ["read", "write", "build", "full"]);
}

#[test]
fn request_schema_refuses_build_mode_with_commands() {
    let schema: Value = serde_json::from_str(super::schema::REQUEST_SCHEMA).unwrap();
    let validator = jsonschema::validator_for(&schema).expect("compile schema");
    let mut request = serde_json::to_value(minimal()).unwrap();
    request["permissions"] = json!({"mode": "build"});
    assert!(validator.is_valid(&request), "build alone is valid");
    request["permissions"] = json!({"mode": "build", "commands": ["cargo test"]});
    assert!(!validator.is_valid(&request), "build with commands is not");
    request["permissions"] = json!({"mode": "write", "commands": ["cargo test"]});
    assert!(
        validator.is_valid(&request),
        "write with commands stays valid"
    );
}

#[test]
fn request_schema_forbids_unknown_properties() {
    let value: Value = serde_json::from_str(super::schema::REQUEST_SCHEMA).unwrap();
    assert_eq!(value["additionalProperties"], json!(false));
}

#[test]
fn outcome_all_lists_exactly_the_variants_of_the_schema() {
    // Adding a variant changes the generated schema, which fails this test
    // until `Outcome::ALL` lists it. `exit_status` is an exhaustive match, so
    // a new variant also fails to compile until it has a status.
    let schema = serde_json::to_value(schemars::schema_for!(Outcome)).unwrap();
    let mut from_schema: Vec<String> = schema["oneOf"]
        .as_array()
        .or_else(|| schema["enum"].as_array())
        .expect("variants in schema")
        .iter()
        .map(|v| {
            v["const"]
                .as_str()
                .or_else(|| v.as_str())
                .unwrap()
                .to_string()
        })
        .collect();
    let mut from_all: Vec<String> = Outcome::ALL
        .iter()
        .map(|o| {
            serde_json::to_value(o)
                .unwrap()
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    from_schema.sort();
    from_all.sort();
    assert_eq!(from_all, from_schema);
}

// Validation gaps.

type Edit = Box<dyn Fn(&mut Request)>;

fn nul() -> String {
    "a\0b".to_string()
}

#[test]
fn nul_byte_in_any_string_field_is_a_validation_problem() {
    let cases: Vec<(&str, Edit)> = vec![
        ("prompt", Box::new(|r| r.prompt = nul())),
        ("instructions", Box::new(|r| r.instructions = Some(nul()))),
        ("name", Box::new(|r| r.name = Some("n\0".into()))),
        ("model", Box::new(|r| r.model = Some(nul()))),
        (
            "session.resume",
            Box::new(|r| r.session.resume = Some(nul())),
        ),
        ("cwd", Box::new(|r| r.cwd = Some(PathBuf::from(nul())))),
        (
            "permissions.commands[0]",
            Box::new(|r| r.permissions.commands = vec![nul()]),
        ),
        (
            "permissions.directories[0]",
            Box::new(|r| r.permissions.directories = vec![PathBuf::from(nul())]),
        ),
        ("native_args[0]", Box::new(|r| r.native_args = vec![nul()])),
    ];
    for (field, edit) in cases {
        let mut r = minimal();
        edit(&mut r);
        let problems = r.validate().unwrap_err();
        assert!(
            problems
                .iter()
                .any(|p| p.field == field && p.message.contains("NUL")),
            "no NUL problem for {field}: {problems:?}"
        );
    }
}

#[test]
fn values_that_become_provider_arguments_must_not_start_with_a_dash() {
    for field in ["model", "session.resume", "name"] {
        let mut r = minimal();
        match field {
            "model" => r.model = Some("--yolo".into()),
            "name" => r.name = Some("-x".into()),
            _ => r.session.resume = Some("-s".into()),
        }
        assert_eq!(problem_fields(&r), [field]);
    }
}

#[test]
fn oversized_answer_schema_is_a_validation_problem() {
    let mut r = minimal();
    r.answer_schema = Some(json!({"description": "x".repeat(MAX_ANSWER_SCHEMA_BYTES)}));
    assert_eq!(problem_fields(&r), ["answer_schema"]);
}

#[test]
fn oversized_paths_are_validation_problems() {
    let long = || PathBuf::from("/".to_string() + &"d".repeat(MAX_PATH_BYTES));
    let mut r = minimal();
    r.cwd = Some(long());
    r.permissions.directories = vec![long()];
    assert_eq!(problem_fields(&r), ["cwd", "permissions.directories[0]"]);
}

#[test]
fn problem_serializes_with_field_and_message() {
    let problem = Problem::new("prompt", "must not be empty");
    assert_eq!(
        serde_json::to_value(problem).unwrap(),
        json!({"field": "prompt", "message": "must not be empty"})
    );
}

// Results.

fn context() -> RunContext {
    RunContext {
        run: "run-1".into(),
        worker: None,
        provider: Provider::Codex,
        permissions: AppliedPermissions::default(),
        cleanup: Cleanup::Confirmed,
        timing: full_result().timing,
        provenance: full_result().provenance,
    }
}

/// Contexts that stress the constructors: leftovers, a worker, denials, no
/// start time, a long duration.
fn awkward_contexts() -> Vec<RunContext> {
    let mut unconfirmed = context();
    unconfirmed.cleanup = Cleanup::Unconfirmed {
        pids: vec![],
        process_groups: vec![],
        reason: String::new(),
    };
    let mut busy = context();
    busy.worker = full_result().worker;
    busy.permissions = full_result().permissions;
    busy.cleanup = full_result().cleanup;
    busy.timing.duration_ms = u64::MAX;
    let mut never_started = context();
    never_started.timing.started_at = None;
    vec![context(), unconfirmed, busy, never_started]
}

fn refusal_context() -> RefusalContext {
    let ctx = context();
    RefusalContext {
        run: ctx.run,
        worker: None,
        provider: None,
        permissions: ctx.permissions,
        timing: ctx.timing,
        provenance: ctx.provenance,
    }
}

fn awkward_refusal_contexts() -> Vec<RefusalContext> {
    let mut with_start = refusal_context();
    with_start.timing.started_at = Some("2026-11-18T10:00:01Z".into());
    with_start.timing.duration_ms = 99;
    let mut known = refusal_context();
    known.provider = Some(Provider::Claude);
    known.worker = full_result().worker;
    known.permissions = full_result().permissions;
    vec![refusal_context(), with_start, known]
}

fn result_fields(result: &RunResult) -> Vec<String> {
    result
        .validate()
        .err()
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.field)
        .collect()
}

#[test]
fn every_constructor_builds_a_valid_result_for_every_awkward_context() {
    let error = || ErrorDetail {
        kind: String::new(),
        message: String::new(),
        provider_exit_code: Some(-1),
    };
    for ctx in awkward_contexts() {
        let built = [
            (RunResult::completed(ctx.clone(), ""), Outcome::Completed),
            (
                RunResult::failed(ctx.clone(), error(), "x"),
                Outcome::Failed,
            ),
            (RunResult::timed_out(ctx.clone(), "", ""), Outcome::TimedOut),
            (
                RunResult::interrupted(ctx.clone(), "", "x"),
                Outcome::Interrupted,
            ),
            (
                RunResult::refused_after_start(ctx.clone(), "denied", "m", "partial"),
                Outcome::Refused,
            ),
        ];
        for (result, outcome) in built {
            assert_eq!(result.outcome, outcome);
            assert_eq!(result.validate(), Ok(()), "{outcome:?} with {ctx:?}");
            let text = serde_json::to_string(&result).unwrap();
            let back: RunResult = serde_json::from_str(&text).unwrap();
            assert_eq!(back, result, "{outcome:?} does not round-trip");
        }
    }
    for ctx in awkward_refusal_contexts() {
        let a = RunResult::refused_before_start(ctx.clone(), "limit", "full");
        let b = RunResult::refused_invalid(ctx.clone(), &[Problem::new("a", "b")]).unwrap();
        for result in [a, b] {
            assert_eq!(result.outcome, Outcome::Refused);
            assert_eq!(result.validate(), Ok(()), "{ctx:?}");
        }
    }
}

#[test]
fn refused_before_start_has_no_start_time_no_duration_and_no_text() {
    for ctx in awkward_refusal_contexts() {
        let r = RunResult::refused_before_start(ctx, "limit", "full");
        assert_eq!(r.timing.started_at, None);
        assert_eq!(r.timing.duration_ms, 0);
        assert_eq!(r.text, "");
        assert_eq!(r.cleanup, Cleanup::Confirmed);
    }
}

#[test]
fn refused_after_start_keeps_timing_and_partial_text() {
    let mut ctx = context();
    ctx.timing.duration_ms = 1234;
    let r = RunResult::refused_after_start(ctx, "permission_denied", "needed a write", "so far");
    assert_eq!(r.timing.started_at, full_result().timing.started_at);
    assert_eq!(r.timing.duration_ms, 1234);
    assert_eq!(r.text, "so far");
    assert_eq!(r.validate(), Ok(()));
}

#[test]
fn refused_invalid_with_no_problems_is_an_error() {
    assert_eq!(
        RunResult::refused_invalid(refusal_context(), &[]),
        Err(NoProblems)
    );
}

#[test]
fn refused_result_for_an_unparseable_request_has_no_provider_or_worker() {
    let problems = vec![Problem::new("provider", "is required")];
    let result = RunResult::refused_invalid(refusal_context(), &problems).unwrap();
    let value = serde_json::to_value(&result).unwrap();
    assert!(value.get("provider").is_none());
    assert!(value.get("worker").is_none());
    assert_eq!(value["outcome"], "refused");
    assert_eq!(value["error"]["kind"], "invalid_request");
    assert_eq!(value["error"]["message"], "provider: is required");
}

#[test]
fn result_without_permissions_reads_with_the_default_mode() {
    let mut value = serde_json::to_value(RunResult::completed(context(), "a")).unwrap();
    value.as_object_mut().unwrap().remove("permissions");
    let result: RunResult = serde_json::from_value(value).unwrap();
    assert_eq!(result.permissions.mode, Mode::Read);
}

#[test]
fn completed_result_with_an_error_is_a_problem() {
    let mut r = RunResult::completed(context(), "a");
    r.error = full_result().error;
    assert_eq!(result_fields(&r), ["error"]);
}

#[test]
fn non_completed_result_without_an_error_is_a_problem() {
    for outcome in [
        Outcome::Failed,
        Outcome::TimedOut,
        Outcome::Interrupted,
        Outcome::Refused,
    ] {
        let mut r = RunResult::completed(context(), "a");
        r.outcome = outcome;
        assert_eq!(result_fields(&r), ["error"], "{outcome:?}");
    }
}

#[test]
fn reading_a_confirmed_cleanup_with_leftovers_fails() {
    for text in [
        r#"{"confirmed":true,"pids":[1]}"#,
        r#"{"confirmed":true,"process_groups":[1]}"#,
        r#"{"confirmed":true,"reason":"why"}"#,
    ] {
        assert!(serde_json::from_str::<Cleanup>(text).is_err(), "{text}");
    }
}

#[test]
fn reading_an_unconfirmed_cleanup_without_a_reason_fails() {
    assert!(serde_json::from_str::<Cleanup>(r#"{"confirmed":false,"pids":[1]}"#).is_err());
}

#[test]
fn cleanup_serializes_to_the_documented_shape() {
    assert_eq!(
        serde_json::to_value(Cleanup::Confirmed).unwrap(),
        json!({"confirmed": true})
    );
    let c = Cleanup::Unconfirmed {
        pids: vec![1],
        process_groups: vec![],
        reason: "r".into(),
    };
    assert_eq!(
        serde_json::to_value(c).unwrap(),
        json!({"confirmed": false, "pids": [1], "reason": "r"})
    );
}

#[test]
fn structured_valid_without_structured_is_a_problem() {
    let mut r = RunResult::completed(context(), "a");
    r.structured_valid = Some(true);
    assert_eq!(result_fields(&r), ["structured_valid"]);
}

#[test]
fn refused_result_may_carry_a_start_time() {
    let mut r = RunResult::refused_after_start(context(), "k", "m", "");
    r.timing.started_at = Some("2026-11-18T10:00:01Z".into());
    assert_eq!(r.validate(), Ok(()));
}

#[test]
fn started_run_without_a_provider_is_a_problem() {
    for mut r in [
        RunResult::completed(context(), "a"),
        RunResult::timed_out(context(), "m", ""),
    ] {
        r.provider = None;
        assert_eq!(result_fields(&r), ["provider"]);
    }
}

#[test]
fn result_with_an_unknown_schema_version_is_a_problem() {
    let mut r = RunResult::completed(context(), "a");
    r.schema_version = SCHEMA_VERSION + 1;
    assert_eq!(result_fields(&r), ["schema_version"]);
}

#[test]
fn leading_dash_in_cwd_or_a_directory_is_a_validation_problem() {
    let mut r = minimal();
    r.cwd = Some(PathBuf::from("-rf"));
    r.permissions.directories = vec![PathBuf::from("-x")];
    assert_eq!(problem_fields(&r), ["cwd", "permissions.directories[0]"]);
}

// Partial requests.

fn partial(text: &str) -> PartialRequest {
    serde_json::from_str(text).unwrap()
}

#[test]
fn profile_file_without_a_schema_version_is_rejected() {
    let err = PartialRequest::from_profile_json(r#"{"provider":"codex"}"#).unwrap_err();
    assert!(err.to_string().contains("schema_version"), "{err}");
}

#[test]
fn profile_file_with_a_schema_version_is_read() {
    let p =
        PartialRequest::from_profile_json(r#"{"schema_version":1,"provider":"codex"}"#).unwrap();
    assert_eq!(p.provider, Some(Provider::Codex));
}

#[test]
fn partial_request_rejects_unknown_fields_at_every_level() {
    for text in [
        r#"{"nope":1}"#,
        r#"{"permissions":{"nope":1}}"#,
        r#"{"session":{"nope":1}}"#,
    ] {
        assert!(
            serde_json::from_str::<PartialRequest>(text).is_err(),
            "{text}"
        );
    }
}

#[test]
fn merge_lets_explicit_values_override_the_profile() {
    let profile = partial(
        r#"{"schema_version":1,"provider":"codex","prompt":"from file","timeout_secs":60,
            "permissions":{"mode":"write","commands":["a"]},"session":{"resume":"s1"}}"#,
    );
    let flags = partial(
        r#"{"prompt":"from flags","permissions":{"commands":["b"]},"session":{"ephemeral":false}}"#,
    );
    let request = profile.merge(flags).into_request().unwrap();
    assert_eq!(request.provider, Provider::Codex);
    assert_eq!(request.prompt, "from flags");
    assert_eq!(request.timeout_secs, 60);
    assert_eq!(request.permissions.mode, Mode::Write);
    assert_eq!(request.permissions.commands, ["b"]);
    assert_eq!(request.session.resume.as_deref(), Some("s1"));
}

#[test]
fn into_request_reports_missing_provider_and_prompt_together_with_other_problems() {
    let p = partial(r#"{"timeout_secs":0}"#);
    let fields: Vec<String> = p
        .into_request()
        .unwrap_err()
        .into_iter()
        .map(|p| p.field)
        .collect();
    assert_eq!(fields, ["provider", "prompt", "timeout_secs"]);
}

#[test]
fn into_request_applies_request_defaults() {
    let request = partial(r#"{"provider":"claude","prompt":"x"}"#)
        .into_request()
        .unwrap();
    assert_eq!(request, Request::new(Provider::Claude, "x"));
}

#[test]
fn explicit_resume_clears_ephemeral_from_the_file() {
    let profile = partial(r#"{"session":{"ephemeral":true}}"#);
    let merged = profile.merge(partial(r#"{"session":{"resume":"s1"}}"#));
    let session = merged.session.unwrap();
    assert_eq!(session.ephemeral, None);
    assert_eq!(session.resume.as_deref(), Some("s1"));
}

#[test]
fn explicit_ephemeral_clears_resume_from_the_file() {
    let profile = partial(r#"{"session":{"resume":"s1"}}"#);
    let merged = profile.merge(partial(r#"{"session":{"ephemeral":true}}"#));
    let session = merged.session.unwrap();
    assert_eq!(session.ephemeral, Some(true));
    assert_eq!(session.resume, None);
}

#[test]
fn merge_keeps_both_session_fields_when_the_override_does_not_touch_them() {
    let profile = partial(r#"{"session":{"resume":"s1"}}"#);
    let merged = profile.merge(partial(r#"{"session":{"ephemeral":false}}"#));
    let session = merged.session.unwrap();
    assert_eq!(session.resume.as_deref(), Some("s1"));
    assert_eq!(session.ephemeral, Some(false));
}

#[test]
fn explicit_conflicting_overrides_still_fail_validation() {
    let both =
        partial(r#"{"provider":"claude","prompt":"x","session":{"ephemeral":true,"resume":"s"}}"#);
    let merged = PartialRequest::default().merge(both);
    let fields: Vec<String> = merged
        .into_request()
        .unwrap_err()
        .into_iter()
        .map(|p| p.field)
        .collect();
    assert_eq!(fields, ["session"]);
}
