//! The per-case process. It drives counterpart's worker lifecycle (spawn, read
//! until the turn settles, stop, shutdown) against the fake provider and prints
//! one JSON line per stage. The parent measures survivors between stages, so
//! this process stays alive until the parent closes its stdin.

use std::{
    io::BufRead,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use counterpart_core::{
    providers::BackendConfig,
    supervisor::{ServiceConfig, SupervisorHandle, spawn_request},
    worker_input,
};
use serde_json::{Value, json};

use crate::matrix::{Prov, Spec};

const PROMPT: &str = "do the assigned work";

fn emit(value: Value) {
    println!("{value}");
}

fn ms(d: Duration) -> u64 {
    d.as_millis() as u64
}

async fn wait_line() -> Option<String> {
    tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        match std::io::stdin().lock().read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line),
        }
    })
    .await
    .ok()
    .flatten()
}

fn stage(name: &str, elapsed: Duration, mut body: Value, note: &str) {
    body["stage"] = json!(name);
    body["elapsed_ms"] = json!(ms(elapsed));
    body["note"] = json!(note);
    emit(body);
}

pub fn run(spec: Spec, dir: PathBuf) -> ! {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        case(&spec, &dir).await;
        while wait_line().await.is_some() {}
    });
    std::process::exit(0);
}

/// The settled agent as the supervisor reports it, reduced to the fields the
/// table needs. The full `last_result` goes into the record verbatim.
fn describe(agent: &Value) -> Value {
    let last = &agent["last_result"];
    let obs = &last["process_observations"];
    let states: Vec<Value> = obs
        .as_array()
        .map(|a| a.iter().map(|o| o["cleanup"]["state"].clone()).collect())
        .unwrap_or_default();
    json!({
        "status": agent["status"],
        "pid": agent["pid"],
        "error_kind": last["error_code"].clone(),
        "cleanup_reason": last["cleanup_reason"].clone(),
        "message": last["error"].clone(),
        "exit_code": last["exit_code"].clone(),
        "signal": last["signal"].clone(),
        "text": last["text"].clone(),
        "cleanup_state": states,
        "cleanup_warnings": last["cleanup_warnings"].clone(),
        "agent": agent,
    })
}

fn is_settled(agent: &Value) -> bool {
    matches!(
        agent["status"].as_str(),
        Some("completed" | "failed" | "interrupted")
    ) && agent["pid"].is_null()
}

async fn case(spec: &Spec, dir: &Path) {
    let exe = dir.join("provider").display().to_string();
    let home = dir.join("home");
    let vars = vec![
        ("HOME".to_string(), home.display().to_string()),
        ("TMPDIR".to_string(), dir.display().to_string()),
        ("PATH".to_string(), "/usr/bin:/bin".to_string()),
        ("SPIKE_DIR".to_string(), dir.display().to_string()),
        ("SPIKE_CASE".to_string(), spec.case.letter.to_string()),
        ("SPIKE_VARIANT".to_string(), spec.variant.to_string()),
    ];
    let mut config = match ServiceConfig::from_vars_with_hook_program(vars, exe.clone()) {
        Ok(c) => c,
        Err(e) => {
            stage(
                "result",
                Duration::ZERO,
                json!({"status": "config-error", "message": e}),
                "",
            );
            return;
        }
    };
    let backend = BackendConfig {
        command: exe,
        prefix_args: vec![],
    };
    config.claude = backend.clone();
    config.codex = backend;
    let defaults = json!({
        "kill_grace_ms": ms(config.kill_grace), "result_grace_ms": ms(config.result_grace),
        "stop_deadline_ms": ms(config.stop_deadline), "drain_bound_ms": ms(config.drain_bound),
        "track_descendants": config.track_descendants,
    });
    let workers = SupervisorHandle::start(config);

    let cwd = std::fs::canonicalize(dir.join("work")).unwrap();
    let mut args = json!({
        "task_name": "spike", "cwd": cwd, "message": PROMPT,
        "backend": match spec.prov { Prov::Claude => "claude", Prov::Codex => "codex" },
    });
    if spec.prov == Prov::Claude {
        args["claude"] = json!({"allowed_tools": []});
    }
    if let Some(t) = spec.case.timeout {
        args["timeout_ms"] = json!(ms(t));
    }
    let input = match worker_input::parse_spawn(&args) {
        Ok(i) => i,
        Err(e) => {
            stage(
                "result",
                Duration::ZERO,
                json!({"status": "parse-error", "message": e}),
                "",
            );
            return;
        }
    };
    let limit = format!("{:?}", input.timeout_ms);
    let hard = spec.hard_limit();
    let start = Instant::now();
    let admitted = match workers.spawn(spawn_request(input)).await {
        Ok(v) => v,
        Err(e) => {
            stage(
                "result",
                start.elapsed(),
                json!({"status": "spawn-error", "message": e.error}),
                "",
            );
            return;
        }
    };
    let id = admitted["agent_id"].as_str().unwrap().to_owned();
    let mut note = format!("turn timeout parsed as {limit} ms; supervisor defaults {defaults}");

    let mut interrupted_at: Option<Instant> = None;
    let body: Option<Value>;
    let mut workers = Some(workers);
    let deadline = start + hard;
    loop {
        let now = Instant::now();
        let w = workers.as_ref().unwrap();
        if spec.case.id == "d" && interrupted_at.is_none() && now >= start + Duration::from_secs(1)
        {
            interrupted_at = Some(now);
            let r = w.stop(id.clone(), false).await;
            note.push_str(&format!(
                "; stop(close=false) at 1 s returned {}",
                match &r {
                    Ok(v) => v.to_string(),
                    Err(e) => format!("Err({})", e.error),
                }
            ));
        }
        if spec.case.id == "j" && now >= start + Duration::from_secs(1) {
            // Drop a wait future mid-run; the run stays owned by the supervisor.
            let _ = tokio::time::timeout(
                Duration::from_millis(50),
                w.wait(
                    vec![id.clone()],
                    0,
                    Duration::from_secs(30),
                    || false,
                    std::future::pending(),
                ),
            )
            .await;
            let state = w
                .read(id.clone(), 0)
                .await
                .map(|v| v["agent"]["status"].clone());
            body = Some(json!({"status": "none", "caller_received":
                "no result; wait future dropped, run still owned by the supervisor"}));
            note.push_str(&format!("; wait dropped at 1 s; agent status {state:?}"));
            break;
        }
        if spec.case.id == "j2" && now >= start + Duration::from_secs(1) {
            workers = None; // drop the SupervisorHandle without shutdown
            body = Some(json!({"status": "none", "caller_received":
                "no result; SupervisorHandle dropped"}));
            note.push_str("; handle dropped at 1 s");
            break;
        }
        match w.read(id.clone(), 0).await {
            Ok(v) if is_settled(&v["agent"]) => {
                body = Some(describe(&v["agent"]));
                break;
            }
            Ok(_) => {}
            Err(e) => {
                body = Some(json!({"status": "read-error", "message": e.error}));
                break;
            }
        }
        if Instant::now() >= deadline {
            let v = w.read(id.clone(), 0).await.ok();
            body = Some(json!({"status": "HANG",
                "agent": v.as_ref().map(|v| v["agent"].clone())}));
            note.push_str(&format!("; no settled result in {} s", hard.as_secs()));
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    stage("result", start.elapsed(), body.unwrap(), &note);

    if spec.case.id == "j2" {
        return;
    }
    // Stage two: close the worker and shut the supervisor down.
    if wait_line().await.is_none() {
        return;
    }
    let w = workers.unwrap();
    let t = Instant::now();
    let closed = tokio::time::timeout(Duration::from_secs(40), w.stop(id.clone(), true)).await;
    let close_ms = ms(t.elapsed());
    let closed = match closed {
        Ok(Ok(v)) => json!({"ok": v}),
        Ok(Err(e)) => json!({"err": e.error}),
        Err(_) => json!("close did not return in 40 s"),
    };
    let after = w.read(id.clone(), 0).await.ok().map(|v| v["agent"].clone());
    let t2 = Instant::now();
    let shut = tokio::time::timeout(Duration::from_secs(40), w.shutdown()).await;
    let shutdown = match shut {
        Ok(Ok(())) => json!("ok"),
        Ok(Err(errors)) => json!({"err": errors}),
        Err(_) => json!("shutdown did not return in 40 s"),
    };
    stage(
        "shutdown",
        t.elapsed(),
        json!({
            "status": if shutdown == json!("ok") { "shutdown-ok" } else { "shutdown-error" },
            "close": closed, "close_ms": close_ms, "shutdown": shutdown,
            "shutdown_ms": ms(t2.elapsed()),
            "agent_after_close": after.as_ref().map(describe),
        }),
        "",
    );
}
