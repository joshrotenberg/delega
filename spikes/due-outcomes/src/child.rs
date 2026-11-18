//! The per-case process. It drives one due path against the fake provider and
//! prints one JSON line per stage. The parent measures survivors between
//! stages, so this process stays alive (and so does due's runtime) until the
//! parent closes its stdin.

use std::{
    future::Future,
    io::BufRead,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use due_core::{
    Assignment, Engine, InteractionMode, Limits, Provider, RunState, WaitOptions, WaitUntil,
    WorkerOptions,
    oneshot::{self, RunObserver, RunOptions},
    result::{RunResult, Status},
};
use serde_json::{Value, json};

use crate::matrix::{Path as RunPath, Prov, Spec};

const PROMPT: &str = "do the assigned work";

struct Streaming;
impl RunObserver for Streaming {}

fn provider(spec: &Spec) -> Provider {
    match spec.prov {
        Prov::Claude => Provider::Claude,
        Prov::Codex => Provider::Codex,
    }
}

fn emit(value: Value) {
    println!("{value}");
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

fn ms(d: Duration) -> u64 {
    d.as_millis() as u64
}

/// What the caller received, as plain data.
fn describe(result: &RunResult) -> Value {
    let status = match result.status {
        Status::Succeeded => "Succeeded",
        Status::Failed => "Failed",
        Status::TimedOut => "TimedOut",
        Status::Cancelled => "Cancelled",
        Status::Planned => "Planned",
    };
    json!({
        "status": status,
        "error_kind": result.error.as_ref().map(|e| format!("{:?}", e.kind)),
        "exit_code": result.error.as_ref().and_then(|e| e.provider_exit_code),
        "message": result.error.as_ref().map(|e| e.message.clone()),
        "text": result.text,
        "diagnostics": result.diagnostics,
        "permission_denials": result.permission_denials.items.len(),
    })
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
        match spec.path {
            RunPath::OneShot => oneshot_case(&spec, &dir).await,
            RunPath::Resident => resident_case(&spec, &dir).await,
        }
        // Stay up until the parent has measured everything.
        while wait_line().await.is_some() {}
    });
    // Skip runtime teardown so no destructor cleans up for due.
    std::process::exit(0);
}

type Cancel = Pin<Box<dyn Future<Output = ()> + Send>>;

async fn oneshot_case(spec: &Spec, dir: &Path) {
    let mut options = RunOptions::new(provider(spec));
    options.binary = Some(dir.join("provider"));
    options.cwd = Some(dir.join("work"));
    options.timeout = spec.case.timeout;
    options.ephemeral = true;
    let observer: Arc<dyn RunObserver> = if spec.stream {
        Arc::new(Streaming)
    } else {
        Arc::new(())
    };
    let hard = spec.hard_limit();
    let start = Instant::now();

    if spec.case.id == "j" {
        // The calling task is cancelled mid-run.
        let task = tokio::spawn(async move {
            oneshot::execute(&options, PROMPT, observer, std::future::pending()).await
        });
        tokio::time::sleep(Duration::from_secs(1)).await;
        task.abort();
        let joined = task.await;
        let cancelled = matches!(&joined, Err(e) if e.is_cancelled());
        stage(
            "result",
            start.elapsed(),
            json!({"status": "none", "error_kind": null, "message": null,
                   "caller_received": format!("JoinError cancelled={cancelled}; no RunResult")}),
            "calling task aborted at 1 s",
        );
        return;
    }

    let cancel: Cancel = if spec.case.id == "d" {
        Box::pin(tokio::time::sleep(Duration::from_secs(1)))
    } else {
        Box::pin(std::future::pending())
    };
    let call = oneshot::execute(&options, PROMPT, observer, cancel);
    match tokio::time::timeout(hard, call).await {
        Ok(result) => {
            let note = if spec.case.id == "d" {
                "cancel future fired at 1 s"
            } else {
                ""
            };
            stage("result", start.elapsed(), describe(&result), note);
        }
        Err(_) => stage(
            "result",
            start.elapsed(),
            json!({"status": "HANG", "error_kind": null, "message": null}),
            &format!(
                "no result in {} s; future dropped at the limit",
                hard.as_secs()
            ),
        ),
    }
}

async fn resident_case(spec: &Spec, dir: &Path) {
    let engine = Engine::new(Limits {
        settlement_timeout: Duration::from_secs(3),
        ..Limits::default()
    })
    .expect("engine");
    let mut worker = WorkerOptions::new(provider(spec), dir.join("work"));
    worker.binary = Some(dir.join("provider"));
    worker.ephemeral = true;
    if spec.case.id == "gr" {
        worker.interaction_mode = InteractionMode::Reject;
    }
    let mut assignment = Assignment::new(worker, PROMPT);
    assignment.timeout = spec.case.timeout;
    let hard = spec.hard_limit();
    let start = Instant::now();
    let run = match engine.spawn(assignment).await {
        Ok(run) => run,
        Err(e) => {
            stage(
                "result",
                start.elapsed(),
                json!({"status": "spawn-error", "error_kind": format!("{:?}", e.kind), "message": e.message}),
                "",
            );
            return;
        }
    };
    let wait = |limit: Duration| {
        engine.wait(
            std::slice::from_ref(&run),
            WaitOptions {
                until: WaitUntil::All,
                timeout: Some(limit),
            },
        )
    };

    let mut note = String::new();
    let mut body: Option<Value> = None;
    match spec.case.id {
        "d" => {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let t = Instant::now();
            match engine.interrupt(&run).await {
                Ok(snapshot) => {
                    body = snapshot.result.as_deref().map(describe);
                    note = format!(
                        "interrupt() issued at 1 s returned after {} ms",
                        ms(t.elapsed())
                    );
                }
                Err(e) => {
                    body = Some(
                        json!({"status": "interrupt-error", "error_kind": format!("{:?}", e.kind), "message": e.message}),
                    );
                    note = format!("interrupt() returned Err after {} ms", ms(t.elapsed()));
                }
            }
        }
        "j" => {
            tokio::select! {
                _ = wait(hard) => note = "wait finished before the drop".into(),
                _ = tokio::time::sleep(Duration::from_secs(1)) => {
                    let state = engine.inspect(&run).map(|s| format!("{:?}", s.state)).unwrap_or_default();
                    note = format!("wait future dropped at 1 s; run state {state}");
                    body = Some(json!({"status": "none", "error_kind": null, "message": null,
                        "caller_received": "no RunResult; run still owned by the engine"}));
                }
            }
        }
        "j2" => {
            tokio::time::sleep(Duration::from_secs(1)).await;
            drop(engine);
            stage(
                "result",
                start.elapsed(),
                json!({"status": "none", "error_kind": null, "message": null,
                       "caller_received": "no RunResult; Engine dropped"}),
                "Engine dropped at 1 s",
            );
            return;
        }
        _ => {}
    }
    if body.is_none() {
        match wait(hard).await {
            Ok(waited) if waited.condition_met => {
                let snap = &waited.runs[0];
                body = snap.result.as_deref().map(describe);
            }
            Ok(waited) => {
                let state = waited.runs.first().map(|s| s.state);
                body = Some(json!({"status": "HANG", "error_kind": null, "message": null}));
                note = format!(
                    "wait returned condition_met=false after {} s; run state {:?}",
                    hard.as_secs(),
                    state.unwrap_or(RunState::Starting)
                );
            }
            Err(e) => {
                body = Some(
                    json!({"status": "wait-error", "error_kind": format!("{:?}", e.kind), "message": e.message}),
                );
            }
        }
    }
    stage("result", start.elapsed(), body.unwrap(), &note);

    // Stage two: the caller shuts the engine down.
    if wait_line().await.is_none() {
        return;
    }
    let t = Instant::now();
    let outcome = tokio::time::timeout(Duration::from_secs(25), engine.shutdown()).await;
    let (body, note) = match outcome {
        Ok(Ok(())) => (json!({"status": "shutdown-ok"}), String::new()),
        Ok(Err(e)) => (
            json!({"status": "shutdown-error", "error_kind": format!("{:?}", e.kind), "message": e.message}),
            String::new(),
        ),
        Err(_) => (
            json!({"status": "HANG"}),
            "shutdown did not return in 25 s".to_owned(),
        ),
    };
    let mut body = body;
    if let Ok(snap) = engine.inspect(&run) {
        body["run_state"] = json!(format!("{:?}", snap.state));
        body["run_result"] = snap.result.as_deref().map(describe).unwrap_or(Value::Null);
    }
    stage("shutdown", t.elapsed(), body, &note);
}
