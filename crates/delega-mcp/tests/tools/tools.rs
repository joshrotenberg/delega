//! Each tool through the router, with the fake providers.

use std::time::Duration;

use delega_core::contract::request::Provider;
use delega_core::supervisor::{Limits, Supervisor, SupervisorConfig};
use serde_json::{Value, json};

use super::{Rig, assert_honest_cleanup};

const CODEX: &str = "codex";

#[tokio::test]
async fn the_server_lists_eight_tools_each_with_input_and_output_schemas() {
    let mut rig = Rig::new(Provider::Codex).await;
    let listed = rig.client.list_tools().await;
    let mut names: Vec<&str> = listed
        .iter()
        .map(|t| t["name"].as_str().expect("name"))
        .collect();
    // tower-mcp lists tools by name.
    names.sort_unstable();
    let mut expected = delega_mcp::TOOL_NAMES.to_vec();
    expected.sort_unstable();
    assert_eq!(names, expected);
    let schemas = delega_mcp::tool_schemas();
    assert_eq!(schemas.len(), listed.len());
    for schema in &schemas {
        let tool = listed
            .iter()
            .find(|t| t["name"] == schema.name.as_str())
            .unwrap_or_else(|| panic!("{} is not listed", schema.name));
        assert_eq!(tool["inputSchema"], schema.input, "{}", schema.name);
        assert_eq!(tool["outputSchema"], schema.output, "{}", schema.name);
        assert_eq!(tool["inputSchema"]["type"], "object", "{}", schema.name);
        assert_eq!(tool["outputSchema"]["type"], "object", "{}", schema.name);
        assert!(
            tool["description"].as_str().is_some_and(|d| !d.is_empty()),
            "{}",
            schema.name
        );
        // No tool takes arguments it does not name.
        assert_eq!(
            tool["inputSchema"]["additionalProperties"],
            json!(false),
            "{}",
            schema.name
        );
    }
    rig.finish().await;
}

#[tokio::test]
async fn a_run_goes_from_start_through_wait_to_a_result_read_with_read() {
    let mut rig = Rig::new(Provider::Codex).await;
    let args = rig.start_args(CODEX, "success");
    let started = rig.call("start", args).await;
    let run = started["run"].as_str().expect("run").to_string();
    let worker = started["worker"].as_str().expect("worker").to_string();
    assert!(
        ["queued", "running"].contains(&started["state"].as_str().expect("state")),
        "{started}"
    );

    let waited = rig.finished(&run).await;
    assert_eq!(waited["runs"][0]["outcome"], "completed", "{waited}");

    let read = rig.call("read", json!({"run": run})).await;
    assert_eq!(read["state"], "finished");
    assert_eq!(read["result"]["outcome"], "completed");
    assert_eq!(read["result"]["text"], "fake final answer");
    assert_eq!(read["result"]["worker"]["id"], worker.as_str());
    // A worker names its latest run.
    let by_worker = rig.call("read", json!({"worker": worker})).await;
    assert_eq!(by_worker["run"], run.as_str());
    assert_eq!(by_worker["worker_state"], "idle");

    let full = rig.read_full(&run).await;
    assert_eq!(full["result"]["outcome"], "completed");
    assert!(full["result"]["usage"].is_object(), "{full}");
    assert!(full["result"]["session_usage"].is_object(), "{full}");
    rig.finish().await;
}

#[tokio::test]
async fn an_invalid_request_is_a_refused_run_that_read_returns() {
    let mut rig = Rig::new(Provider::Codex).await;
    let mut args = rig.start_args(CODEX, "success");
    args["cwd"] = json!(rig.work.join("missing"));
    let refused = rig.call("start", args).await;
    assert_eq!(refused["state"], "finished", "{refused}");
    assert_eq!(refused["outcome"], "refused");
    assert!(refused.get("worker").is_none(), "a refusal makes no worker");
    assert_eq!(refused["error"]["kind"], "cwd_unusable");

    let read = rig.call("read", json!({"run": refused["run"]})).await;
    assert_eq!(read["result"]["outcome"], "refused");
    assert_eq!(read["result"]["error"]["kind"], "cwd_unusable");

    // A value the contract rejects and the schema does not describe is a
    // refused run too, not a tool error.
    let refused = rig.call("start", rig.start_args(CODEX, "   ")).await;
    assert_eq!(refused["outcome"], "refused", "{refused}");
    assert_eq!(refused["error"]["kind"], "invalid_request");
    // A time limit out of range is stated by the input schema, which rejects it.
    let mut args = rig.start_args(CODEX, "success");
    args["timeout_secs"] = json!(0);
    let error = rig.client.call_tool_expect_error("start", args).await;
    assert!(error.to_string().contains("input schema"), "{error}");
    rig.finish().await;
}

#[tokio::test]
async fn a_start_without_an_absolute_cwd_is_refused_as_a_run_and_starts_nothing() {
    let mut rig = Rig::new(Provider::Codex).await;
    let mut missing = rig.start_args(CODEX, "success");
    missing.as_object_mut().expect("object").remove("cwd");
    let mut relative = rig.start_args(CODEX, "success");
    relative["cwd"] = json!("work");
    for args in [missing, relative] {
        let refused = rig.call("start", args).await;
        assert_eq!(refused["state"], "finished", "{refused}");
        assert_eq!(refused["outcome"], "refused");
        assert!(refused.get("worker").is_none(), "a refusal makes no worker");
        assert_eq!(refused["error"]["kind"], "cwd_required", "{refused}");

        let read = rig.call("read", json!({"run": refused["run"]})).await;
        assert_eq!(read["result"]["outcome"], "refused", "{read}");
        assert_eq!(read["result"]["error"]["kind"], "cwd_required");
        let message = read["result"]["error"]["message"]
            .as_str()
            .expect("message");
        assert!(message.contains("absolute"), "{message}");
        assert!(message.contains("host"), "{message}");
    }
    assert!(rig.argv().is_empty(), "no process was started");
    let listed = rig.call("list", json!({})).await;
    assert!(
        listed["workers"].as_array().expect("workers").is_empty(),
        "{listed}"
    );

    // A profile without a cwd is refused the same way; an explicit absolute cwd
    // given with it runs.
    let profile = rig.work.join("profile.json");
    std::fs::write(
        &profile,
        json!({"schema_version": 1, "provider": "codex", "prompt": "success"}).to_string(),
    )
    .expect("write profile");
    let refused = rig.call("start", json!({"profile": profile})).await;
    assert_eq!(refused["error"]["kind"], "cwd_required", "{refused}");
    let started = rig
        .call("start", json!({"profile": profile, "cwd": rig.work}))
        .await;
    assert!(started["worker"].is_string(), "{started}");
    rig.finished(started["run"].as_str().expect("run")).await;
    assert!(!rig.argv().is_empty());
    rig.finish().await;
}

#[tokio::test]
async fn a_relative_cwd_in_a_profile_or_a_dot_cwd_is_refused() {
    let mut rig = Rig::new(Provider::Codex).await;
    let profile = rig.work.join("relative.json");
    std::fs::write(
        &profile,
        json!({"schema_version": 1, "provider": "codex", "prompt": "success", "cwd": "work"})
            .to_string(),
    )
    .expect("write profile");
    let refused = rig.call("start", json!({"profile": profile})).await;
    assert_eq!(refused["error"]["kind"], "cwd_required", "{refused}");
    let mut dot = rig.start_args(CODEX, "success");
    dot["cwd"] = json!(".");
    let refused = rig.call("start", dot).await;
    assert_eq!(refused["error"]["kind"], "cwd_required", "{refused}");
    assert!(rig.argv().is_empty(), "no process was started");
    rig.finish().await;
}

#[tokio::test]
async fn a_relative_extra_directory_resolves_under_cwd_not_the_servers_directory() {
    let mut rig = Rig::new(Provider::Codex).await;
    std::fs::create_dir(rig.work.join("extra")).expect("create extra");
    let mut args = rig.start_args(CODEX, "success");
    args["permissions"] = json!({"directories": ["extra"]});
    let started = rig.call("start", args).await;
    assert!(started["worker"].is_string(), "{started}");
    rig.finished(started["run"].as_str().expect("run")).await;

    let mut args = rig.start_args(CODEX, "success");
    args["permissions"] = json!({"directories": ["absent"]});
    let refused = rig.call("start", args).await;
    assert_eq!(refused["error"]["kind"], "directory_unusable", "{refused}");
    let message = refused["error"]["message"].as_str().expect("message");
    let under_cwd = rig.work.join("absent");
    assert!(
        message.contains(under_cwd.to_str().expect("utf-8")),
        "{message}"
    );
    let server = std::env::current_dir().expect("server directory");
    assert!(
        !message.contains(server.to_str().expect("utf-8")),
        "{message}"
    );
    rig.finish().await;
}

#[tokio::test]
async fn a_profile_supplying_an_absolute_cwd_satisfies_the_requirement() {
    let mut rig = Rig::new(Provider::Codex).await;
    let profile = rig.work.join("with-cwd.json");
    std::fs::write(
        &profile,
        json!({
            "schema_version": 1,
            "provider": "codex",
            "prompt": "success",
            "cwd": rig.work,
        })
        .to_string(),
    )
    .expect("write profile");
    let started = rig.call("start", json!({"profile": profile})).await;
    assert!(started["worker"].is_string(), "{started}");
    let done = rig.finished(started["run"].as_str().expect("run")).await;
    assert_eq!(done["runs"][0]["outcome"], "completed", "{done}");
    rig.finish().await;
}

#[tokio::test]
async fn start_with_strict_permissions_refuses_codex_read_and_read_returns_it() {
    let mut rig = Rig::new(Provider::Codex).await;
    let mut args = rig.start_args(CODEX, "success");
    args["permissions"] = json!({"strict": true});
    let refused = rig.call("start", args).await;
    assert_eq!(refused["state"], "finished", "{refused}");
    assert_eq!(refused["outcome"], "refused");
    assert!(refused.get("worker").is_none(), "a refusal makes no worker");
    assert_eq!(refused["error"]["kind"], "permissions_not_enforceable");
    assert!(
        refused["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("reads are not confined")),
        "{refused}"
    );
    assert!(rig.argv().is_empty(), "no provider process started");

    let read = rig.call("read", json!({"run": refused["run"]})).await;
    assert_eq!(read["result"]["outcome"], "refused");
    assert_eq!(
        read["result"]["error"]["kind"],
        "permissions_not_enforceable"
    );
    rig.finish().await;
}

#[tokio::test]
async fn reaching_the_worker_limit_is_a_refused_run_and_a_closed_worker_refuses_a_follow_up() {
    let limits = Limits {
        max_live_workers: 1,
        ..Limits::default()
    };
    let mut rig = Rig::with_limits(Provider::Codex, limits).await;
    let gate = rig.gate("limit");
    let first = rig
        .call("start", rig.start_args(CODEX, &gate.prompt()))
        .await;
    gate.started().await;
    let worker = first["worker"].as_str().expect("worker").to_string();

    let second = rig.call("start", rig.start_args(CODEX, "success")).await;
    assert_eq!(second["outcome"], "refused", "{second}");
    assert_eq!(second["error"]["kind"], "limit_reached");
    let read = rig.call("read", json!({"run": second["run"]})).await;
    assert_eq!(read["result"]["error"]["kind"], "limit_reached");

    let listed = rig.call("list", json!({})).await;
    assert_eq!(listed["limits"]["max_live_workers"], 1);
    assert_eq!(listed["limits"]["live_workers"], 1);

    gate.open();
    rig.finished(first["run"].as_str().expect("run")).await;
    rig.call("close", json!({"worker": worker})).await;
    rig.finish().await;
}

#[tokio::test]
async fn an_id_that_names_nothing_is_a_tool_error_with_a_message() {
    let mut rig = Rig::new(Provider::Codex).await;
    for (tool, args, needle) in [
        ("read", json!({"run": "run-nope"}), "run-nope"),
        ("read", json!({"worker": "worker-nope"}), "worker-nope"),
        ("interrupt", json!({"run": "run-nope"}), "run-nope"),
        ("interrupt", json!({"worker": "worker-nope"}), "worker-nope"),
        ("close", json!({"worker": "worker-nope"}), "worker-nope"),
        (
            "follow_up",
            json!({"worker": "worker-nope", "prompt": "hello"}),
            "worker-nope",
        ),
    ] {
        let message = rig.call_error(tool, args).await;
        assert!(message.contains(needle), "{tool}: {message}");
        assert!(message.starts_with("no "), "{tool}: {message}");
    }
    // Give neither, or both, of run and worker.
    let message = rig.call_error("read", json!({})).await;
    assert!(message.contains("run or a worker"), "{message}");
    let message = rig
        .call_error("interrupt", json!({"run": "a", "worker": "b"}))
        .await;
    assert!(message.contains("not both"), "{message}");
    // Nothing to wait for.
    let message = rig.call_error("wait", json!({"timeout_secs": 1})).await;
    assert!(!message.is_empty());
    // A request that cannot be built has no provider or no prompt.
    let message = rig.call_error("start", json!({"prompt": "hello"})).await;
    assert!(message.contains("provider"), "{message}");
    let message = rig.call_error("start", json!({"provider": "codex"})).await;
    assert!(message.contains("prompt"), "{message}");
    rig.finish().await;
}

#[tokio::test]
async fn an_unknown_argument_is_rejected_by_every_tool() {
    let mut rig = Rig::new(Provider::Codex).await;
    let valid_start = rig.start_args(CODEX, "success");
    for (tool, mut args) in [
        ("start", valid_start),
        ("follow_up", json!({"worker": "w", "prompt": "p"})),
        ("wait", json!({"runs": ["r"]})),
        ("read", json!({"run": "r"})),
        ("list", json!({})),
        ("interrupt", json!({"run": "r"})),
        ("close", json!({"worker": "w"})),
    ] {
        args["bogus"] = json!(1);
        let error = rig.client.call_tool_expect_error(tool, args).await;
        assert!(
            error.to_string().contains("input schema"),
            "{tool} did not reject the argument against its schema: {error}"
        );
    }
    // Nothing was started by the rejected calls.
    let listed = rig.call("list", json!({})).await;
    assert_eq!(listed["workers"], json!([]));
    rig.finish().await;
}

#[tokio::test]
async fn a_wait_that_times_out_is_not_an_error_and_its_cursor_is_accepted_again() {
    let mut rig = Rig::new(Provider::Codex).await;
    let gate = rig.gate("wait");
    let started = rig
        .call("start", rig.start_args(CODEX, &gate.prompt()))
        .await;
    let run = started["run"].as_str().expect("run").to_string();
    gate.started().await;

    let first = rig
        .call("wait", json!({"runs": [run], "timeout_secs": 1}))
        .await;
    assert_eq!(first["timed_out"], true, "{first}");
    assert_eq!(first["runs"][0]["state"], "running");
    let cursor = first["cursor"].clone();
    assert!(cursor.is_u64());

    let second = rig
        .call(
            "wait",
            json!({"runs": [run], "timeout_secs": 1, "after": cursor}),
        )
        .await;
    assert_eq!(second["timed_out"], true, "{second}");
    assert_eq!(second["runs"][0]["changed"], false);

    gate.open();
    // The cursor-bearing wait returns when the run finishes after it.
    let mut done = Value::Null;
    for _ in 0..60 {
        let waited = rig
            .call(
                "wait",
                json!({"runs": [run], "timeout_secs": 30, "after": cursor}),
            )
            .await;
        if waited["timed_out"] == false {
            done = waited;
            break;
        }
    }
    assert_eq!(done["runs"][0]["outcome"], "completed", "{done}");
    assert_eq!(done["runs"][0]["changed"], true);
    assert!(done["cursor"].as_u64() > cursor.as_u64());
    rig.finish().await;
}

#[tokio::test]
async fn a_long_answer_is_paged_by_run_with_text_offset_and_reassembles() {
    let mut rig = Rig::new(Provider::Codex).await;
    let started = rig.call("start", rig.start_args(CODEX, "large")).await;
    let run = started["run"].as_str().expect("run").to_string();
    rig.finished(&run).await;

    let first = rig.call("read", json!({"run": run})).await;
    assert_eq!(first["result"]["outcome"], "completed", "{first}");
    assert_eq!(first["result"]["text_truncated"], true);
    let total = first["result"]["text_chars"].as_u64().expect("total") as usize;
    let mut text = first["result"]["text"].as_str().expect("text").to_string();
    assert!(text.chars().count() < total);

    let mut pages = 1;
    loop {
        let offset = text.chars().count();
        let page = rig
            .call("read", json!({"run": run, "text_offset": offset}))
            .await;
        let window = &page["text_window"];
        assert_eq!(window["offset"], offset);
        assert_eq!(window["total"], total);
        text.push_str(page["result"]["text"].as_str().expect("text"));
        pages += 1;
        if window["more"] == false {
            break;
        }
        assert!(pages < 1000, "paging does not end");
    }
    assert_eq!(text.chars().count(), total);
    assert!(text.chars().all(|c| c == 'x'));
    // The same answer in one piece.
    let whole = rig.read_full(&run).await;
    assert_eq!(whole["result"]["text"].as_str().expect("text").len(), total);
    rig.finish().await;
}

#[tokio::test]
async fn list_shows_workers_their_states_and_the_limits() {
    let limits = Limits {
        max_live_workers: 4,
        max_running_runs: 2,
        ..Limits::default()
    };
    let mut rig = Rig::with_limits(Provider::Codex, limits).await;
    let empty = rig.call("list", json!({})).await;
    assert_eq!(empty["workers"], json!([]));
    assert_eq!(empty["limits"]["max_live_workers"], 4);
    assert_eq!(empty["limits"]["max_running_runs"], 2);
    assert_eq!(empty["limits"]["live_workers"], 0);

    let done = rig.call("start", rig.start_args(CODEX, "success")).await;
    let gate = rig.gate("list");
    let held = rig
        .call("start", rig.start_args(CODEX, &gate.prompt()))
        .await;
    gate.started().await;
    rig.finished(done["run"].as_str().expect("run")).await;

    let listed = rig.call("list", json!({})).await;
    let workers = listed["workers"].as_array().expect("workers");
    assert_eq!(workers.len(), 2, "{listed}");
    let state_of = |id: &Value| {
        workers
            .iter()
            .find(|w| w["worker"] == *id)
            .unwrap_or_else(|| panic!("worker {id} not listed"))
    };
    let idle = state_of(&done["worker"]);
    assert_eq!(idle["state"], "idle");
    assert_eq!(idle["queue_length"], 0);
    assert_eq!(idle["last_outcome"], "completed");
    assert_eq!(idle["provider"], "codex");
    assert!(idle["session_id"].is_string(), "{idle}");
    assert_eq!(idle["can_follow_up"], true);
    let running = state_of(&held["worker"]);
    assert_eq!(running["state"], "running");
    assert_eq!(running["current_run"], held["run"]);
    assert_eq!(listed["limits"]["live_workers"], 2);
    assert_eq!(listed["limits"]["running_runs"], 1);

    gate.open();
    rig.finished(held["run"].as_str().expect("run")).await;
    rig.finish().await;
}

#[tokio::test]
async fn interrupt_returns_after_the_run_has_stopped() {
    let mut rig = Rig::new(Provider::Codex).await;
    let gate = rig.gate("interrupt");
    let started = rig
        .call("start", rig.start_args(CODEX, &gate.prompt()))
        .await;
    gate.started().await;

    let stopped = rig.call("interrupt", json!({"run": started["run"]})).await;
    assert_eq!(stopped["settled"], true, "{stopped}");
    assert_eq!(stopped["state"], "finished");
    assert_eq!(stopped["result"]["outcome"], "interrupted");
    assert_honest_cleanup(&stopped["result"]["cleanup"]);

    // By worker, a finished run is returned unchanged.
    let again = rig
        .call("interrupt", json!({"worker": started["worker"]}))
        .await;
    assert_eq!(again["result"]["outcome"], "interrupted");
    let read = rig.call("read", json!({"run": started["run"]})).await;
    assert_eq!(read["result"]["outcome"], "interrupted");
    rig.finish().await;
}

#[tokio::test]
async fn close_returns_after_the_run_has_stopped_and_discards_the_queue() {
    let mut rig = Rig::new(Provider::Codex).await;
    let gate = rig.gate("close");
    let started = rig
        .call("start", rig.start_args(CODEX, &gate.prompt()))
        .await;
    gate.started().await;
    let worker = started["worker"].as_str().expect("worker").to_string();
    let queued = rig
        .call("follow_up", json!({"worker": worker, "prompt": "later"}))
        .await;
    assert_eq!(queued["state"], "queued", "{queued}");

    let closed = rig.call("close", json!({"worker": worker})).await;
    assert_eq!(closed["state"], "closed");
    assert_eq!(closed["settled"], true, "{closed}");
    assert_eq!(closed["stopped_run"], started["run"]);
    assert_eq!(closed["discarded"], json!([queued["run"]]));
    assert_eq!(closed["result"]["outcome"], "interrupted");
    assert_honest_cleanup(&closed["result"]["cleanup"]);

    let discarded = rig.call("read", json!({"run": queued["run"]})).await;
    assert_eq!(discarded["result"]["outcome"], "interrupted");
    let again = rig.call("close", json!({"worker": worker})).await;
    assert_eq!(again["already_closed"], true);
    rig.finish().await;
}

#[tokio::test]
async fn a_follow_up_continues_the_workers_session() {
    let mut rig = Rig::new(Provider::Codex).await;
    let started = rig.call("start", rig.start_args(CODEX, "echo:one")).await;
    let worker = started["worker"].as_str().expect("worker").to_string();
    let first = started["run"].as_str().expect("run").to_string();
    rig.finished(&first).await;
    let read = rig.call("read", json!({"run": first})).await;
    assert_eq!(read["result"]["session_id"], rig.session.as_str());

    let second = rig
        .call(
            "follow_up",
            json!({"worker": worker, "prompt": "echo:two", "timeout_secs": 120}),
        )
        .await;
    assert_eq!(second["worker"], worker.as_str());
    let run = second["run"].as_str().expect("run").to_string();
    assert_ne!(run, first);
    rig.finished(&run).await;
    let read = rig.call("read", json!({"run": run})).await;
    assert_eq!(read["result"]["outcome"], "completed", "{read}");
    assert_eq!(read["result"]["timing"]["timeout_secs"], 120);
    let argv = rig.argv();
    let at = argv
        .iter()
        .position(|a| a == "resume")
        .unwrap_or_else(|| panic!("the follow-up did not resume the session: {argv:?}"));
    assert_eq!(argv[at + 1], rig.session);
    assert!(
        read["result"]["text"]
            .as_str()
            .is_some_and(|t| t.contains("echo:two")),
        "{read}"
    );
    rig.finish().await;
}

#[tokio::test]
async fn a_follow_up_to_an_ephemeral_worker_is_refused_as_a_run() {
    let mut rig = Rig::new(Provider::Codex).await;
    let mut args = rig.start_args(CODEX, "success");
    args["session"] = json!({"ephemeral": true});
    let started = rig.call("start", args).await;
    rig.finished(started["run"].as_str().expect("run")).await;
    let refused = rig
        .call(
            "follow_up",
            json!({"worker": started["worker"], "prompt": "more"}),
        )
        .await;
    assert_eq!(refused["outcome"], "refused", "{refused}");
    assert_eq!(refused["error"]["kind"], "no_session");
    rig.finish().await;
}

#[tokio::test]
async fn start_merges_a_profile_and_its_overrides_as_delega_run_does() {
    let mut rig = Rig::new(Provider::Codex).await;
    let profile = rig.work.join("profile.json");
    std::fs::write(
        &profile,
        json!({
            "schema_version": 1,
            "provider": "codex",
            "prompt": "from the profile",
            "cwd": rig.work,
            "name": "profiled",
            "permissions": {"mode": "write", "commands": ["make test"]},
        })
        .to_string(),
    )
    .expect("write profile");

    // The profile alone.
    let started = rig.call("start", json!({"profile": profile})).await;
    rig.finished(started["run"].as_str().expect("run")).await;
    assert!(rig.stdin().contains("from the profile"), "{}", rig.stdin());
    let listed = rig.call("list", json!({})).await;
    assert_eq!(listed["workers"][0]["name"], "profiled");

    // An explicit prompt and permissions replace the profile's, and a list
    // replaces the profile's list. The name is taken and the start is refused,
    // which shows the profile's name still applies.
    let overridden = rig
        .call(
            "start",
            json!({
                "profile": profile,
                "prompt": "overridden",
                "permissions": {"mode": "read"},
            }),
        )
        .await;
    assert_eq!(overridden["outcome"], "refused", "{overridden}");
    let read = rig.call("read", json!({"run": overridden["run"]})).await;
    let message = read["result"]["error"]["message"]
        .as_str()
        .expect("message");
    assert!(message.contains("profiled"), "{message}");

    // An unreadable profile and an unknown field in a profile are tool errors.
    let message = rig
        .call_error("start", json!({"profile": rig.work.join("nope.json")}))
        .await;
    assert!(message.contains("cannot read"), "{message}");
    let bad = rig.work.join("bad.json");
    std::fs::write(&bad, json!({"schema_version": 1, "bogus": 1}).to_string()).expect("write");
    let message = rig.call_error("start", json!({"profile": bad})).await;
    assert!(message.contains("not a valid profile"), "{message}");
    // The message gives the path, the kind and the position, and none of the
    // file's own text: not the field name, and not a value.
    assert!(message.contains("line 1, column"), "{message}");
    assert!(!message.contains("bogus"), "{message}");
    std::fs::write(
        &bad,
        json!({"schema_version": 1, "provider": "value-from-the-file"}).to_string(),
    )
    .expect("write");
    let message = rig.call_error("start", json!({"profile": bad})).await;
    assert!(!message.contains("value-from-the-file"), "{message}");
    assert!(message.contains("not a valid profile"), "{message}");
    rig.finish().await;
}

#[tokio::test]
async fn a_waiting_call_with_an_unreachable_cursor_is_released_by_shutdown() {
    let mut rig = Rig::new(Provider::Codex).await;
    let gate = rig.gate("shutdown");
    let started = rig
        .call("start", rig.start_args(CODEX, &gate.prompt()))
        .await;
    gate.started().await;
    let run = started["run"].clone();
    // A second connection to the same server, so a call can wait while this one
    // goes on. The cursor is past anything that will finish, so only shutdown
    // can release the call.
    let router = delega_mcp::router(rig.supervisor.clone(), &delega_mcp::BuildInfo::default())
        .expect("router");
    let mut other = tower_mcp::TestClient::from_router(router);
    other.initialize().await;
    let mut waits = rig.supervisor.observe_waits();
    let waiting = tokio::spawn(async move {
        let result = other
            .call_tool(
                "wait",
                json!({"runs": [run], "timeout_secs": 300, "after": 1_000_000}),
            )
            .await;
        (other, result)
    });
    let registration = waits.registered().await;
    assert!(!waiting.is_finished(), "the wait returned before shutdown");
    let report = rig.supervisor.shutdown().await;
    assert_eq!(report.runs_stopped, 1);
    registration.removed().await;
    let (_, result) = tokio::time::timeout(Duration::from_secs(120), waiting)
        .await
        .expect("shutdown released the waiting call")
        .expect("the task");
    assert!(!result.is_error, "{}", result.all_text());
    let released = super::agree(&result);
    assert_eq!(released["timed_out"], false, "{released}");
    assert_eq!(released["released_by_shutdown"], true, "{released}");
    // Released when the shutdown began, so the run may still be stopping.
    assert_eq!(released["runs"][0]["run"], started["run"], "{released}");

    // After shutdown a read says the run was interrupted, and new work is not
    // accepted.
    let read = rig.call("read", json!({"run": started["run"]})).await;
    assert_eq!(read["result"]["outcome"], "interrupted");
    let message = rig
        .call_error("start", rig.start_args(CODEX, "success"))
        .await;
    assert!(message.contains("shut down"), "{message}");
    let late = rig
        .call(
            "wait",
            json!({"runs": [started["run"]], "timeout_secs": 300, "after": 1_000_000}),
        )
        .await;
    assert_eq!(late["timed_out"], false, "{late}");
    assert_eq!(late["released_by_shutdown"], true, "{late}");
    rig.finish().await;
}

#[tokio::test]
async fn a_cursorless_wait_released_by_shutdown_says_so_and_a_timed_out_wait_does_not() {
    let mut rig = Rig::new(Provider::Codex).await;
    let gate = rig.gate("shutdown-cursorless");
    let started = rig
        .call("start", rig.start_args(CODEX, &gate.prompt()))
        .await;
    gate.started().await;
    let run = started["run"].clone();

    // The timeout is the behavior under test. Registration alone cannot
    // exercise the timeout response.
    let timed_out = rig
        .call("wait", json!({"runs": [run.clone()], "timeout_secs": 1}))
        .await;
    assert_eq!(timed_out["timed_out"], true, "{timed_out}");
    assert!(
        timed_out.get("released_by_shutdown").is_none(),
        "{timed_out}"
    );

    let router = delega_mcp::router(rig.supervisor.clone(), &delega_mcp::BuildInfo::default())
        .expect("router");
    let mut other = tower_mcp::TestClient::from_router(router);
    other.initialize().await;
    let mut waits = rig.supervisor.observe_waits();
    let waiting = tokio::spawn({
        let run = run.clone();
        async move {
            let result = other
                .call_tool("wait", json!({"runs": [run], "timeout_secs": 300}))
                .await;
            (other, result)
        }
    });
    let registration = waits.registered().await;
    assert!(!waiting.is_finished(), "the wait returned before shutdown");
    let report = rig.supervisor.shutdown().await;
    assert_eq!(report.runs_stopped, 1);
    registration.removed().await;
    let (_, result) = tokio::time::timeout(Duration::from_secs(120), waiting)
        .await
        .expect("shutdown released the waiting call")
        .expect("the task");
    assert!(!result.is_error, "{}", result.all_text());
    let released = super::agree(&result);
    assert_eq!(released["timed_out"], false, "{released}");
    assert_eq!(released["released_by_shutdown"], true, "{released}");
    rig.finish().await;
}

#[tokio::test]
async fn a_wait_released_by_a_run_finishing_does_not_claim_shutdown() {
    let mut rig = Rig::new(Provider::Codex).await;
    let started = rig.call("start", rig.start_args(CODEX, "success")).await;
    let waited = rig
        .call(
            "wait",
            json!({"runs": [started["run"].clone()], "timeout_secs": 60}),
        )
        .await;
    assert_eq!(waited["timed_out"], false, "{waited}");
    assert_eq!(waited["runs"][0]["state"], "finished", "{waited}");
    assert!(waited.get("released_by_shutdown").is_none(), "{waited}");
    rig.finish().await;
}

#[tokio::test]
async fn a_wait_past_the_limit_is_answered_at_once_and_interrupt_and_list_are_not_held_behind_waits()
 {
    let limits = Limits {
        max_waits_in_flight: 2,
        ..Limits::default()
    };
    let mut rig = Rig::with_limits(Provider::Codex, limits).await;
    let gate = rig.gate("waits");
    let started = rig
        .call("start", rig.start_args(CODEX, &gate.prompt()))
        .await;
    gate.started().await;
    let run = started["run"].clone();

    // Each parked wait is a call on a connection of its own to the same
    // server, so the calls below go on while they wait.
    let mut waits = rig.supervisor.observe_waits();
    let mut parked = Vec::new();
    for _ in 0..2 {
        let router = delega_mcp::router(rig.supervisor.clone(), &delega_mcp::BuildInfo::default())
            .expect("router");
        let mut other = tower_mcp::TestClient::from_router(router);
        other.initialize().await;
        let run = run.clone();
        parked.push(tokio::spawn(async move {
            let result = other
                .call_tool("wait", json!({"runs": [run], "timeout_secs": 120}))
                .await;
            (other, result)
        }));
    }
    let registrations = [waits.registered().await, waits.registered().await];
    let listed = rig.call("list", json!({})).await;
    assert_eq!(listed["limits"]["max_waits_in_flight"], 2, "{listed}");
    assert_eq!(listed["limits"]["waits_in_flight"], 2, "{listed}");

    // Keep the elapsed bound as a responsiveness assertion while the other
    // calls remain parked. Their registration only establishes ordering.
    let at_once = Duration::from_secs(10);
    let begun = std::time::Instant::now();
    let refused = rig
        .call("wait", json!({"runs": [run], "timeout_secs": 120}))
        .await;
    assert!(begun.elapsed() < at_once, "{:?}", begun.elapsed());
    assert_eq!(refused["limit_reached"], "max_waits_in_flight", "{refused}");
    assert_eq!(refused["timeout_secs"], 0, "{refused}");
    assert_eq!(refused["timed_out"], true, "{refused}");
    assert_eq!(refused["runs"][0]["state"], "running", "{refused}");
    assert!(refused.get("released_by_shutdown").is_none(), "{refused}");

    // A wait given no time never blocks, so it is not turned away.
    let poll = rig
        .call("wait", json!({"runs": [run], "timeout_secs": 0}))
        .await;
    assert!(poll.get("limit_reached").is_none(), "{poll}");
    assert_eq!(poll["timed_out"], true, "{poll}");

    // A wait with nothing to block on is answered without a slot.
    let unknown = rig
        .call(
            "wait",
            json!({"runs": ["no-such-run"], "timeout_secs": 120}),
        )
        .await;
    assert!(unknown.get("limit_reached").is_none(), "{unknown}");

    let begun = std::time::Instant::now();
    let listed = rig.call("list", json!({})).await;
    assert!(begun.elapsed() < at_once, "{:?}", begun.elapsed());
    assert_eq!(listed["limits"]["waits_in_flight"], 2, "{listed}");
    let stopped = rig.call("interrupt", json!({"run": run})).await;
    assert_eq!(stopped["settled"], true, "{stopped}");

    // The interrupt finished the run, which wakes the parked waits.
    for registration in registrations {
        registration.removed().await;
    }
    for wait in parked {
        let (_, result) = tokio::time::timeout(Duration::from_secs(60), wait)
            .await
            .expect("the finished run woke the parked wait")
            .expect("the task");
        assert!(!result.is_error, "{}", result.all_text());
        let woken = super::agree(&result);
        assert_eq!(woken["timed_out"], false, "{woken}");
        assert!(woken.get("limit_reached").is_none(), "{woken}");
        assert_eq!(woken["timeout_secs"], 120, "{woken}");
    }
    let listed = rig.call("list", json!({})).await;
    assert_eq!(listed["limits"]["waits_in_flight"], 0, "{listed}");
    rig.finish().await;
}

#[tokio::test]
async fn no_router_is_built_for_a_supervisor_whose_waits_could_fill_the_request_slots() {
    let router_for = |max_waits_in_flight: usize| {
        let supervisor = Supervisor::new(SupervisorConfig {
            limits: Limits {
                max_waits_in_flight,
                ..Limits::default()
            },
            ..SupervisorConfig::default()
        });
        delega_mcp::router(supervisor, &delega_mcp::BuildInfo::default())
    };
    assert!(router_for(64).is_ok());
    let Err(error) = router_for(65) else {
        panic!("a router was built for 65 waits in flight");
    };
    assert!(error.contains("65 waits in flight"), "{error}");
    assert!(error.contains("at most 64"), "{error}");
}

#[tokio::test]
async fn respond_denies_the_permission_reported_by_a_worker_wait() {
    let mut rig = Rig::new(Provider::Claude).await;
    let mut args = rig.start_args("claude", "ask");
    args["permissions"] = json!({"on_request":"ask"});
    let started = rig.call("start", args).await;
    let run = started["run"].as_str().unwrap();
    let worker = started["worker"].as_str().unwrap();
    let pending = rig
        .call("wait", json!({"workers":[worker],"timeout_secs":5}))
        .await;
    assert_eq!(pending["workers"][0]["state"], "waiting_for_answer");
    let id = &pending["workers"][0]["pending_interactions"][0]["id"];
    assert!(id.is_string());
    let answer = rig
        .call(
            "respond",
            json!({"interaction":id,"decision":"deny","note":"fixture note"}),
        )
        .await;
    assert_eq!(answer["status"], "accepted");
    rig.finished(run).await;
    let finished = rig.read_full(run).await;
    assert_eq!(finished["result"]["text"], "denied");
    assert_eq!(
        finished["result"]["permissions"]["asked"][0]["decided_by"],
        "caller"
    );
    assert!(!finished.to_string().contains("fixture note"));
    rig.finish().await;
}
