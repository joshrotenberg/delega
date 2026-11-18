//! Orchestrates the matrix. Each case runs in its own child process (this
//! binary re-executed with `--case`) with its own temporary directory, so a
//! blocked runtime or a stray process in one case cannot affect another. The
//! parent owns timing, the survivor checks and the final cleanup.

use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use crate::{
    matrix::{self, Spec},
    procs,
};

const FAKES: &[(&str, &str)] = &[
    ("lib.sh", include_str!("../fakes/lib.sh")),
    ("child.sh", include_str!("../fakes/child.sh")),
    ("ticker.sh", include_str!("../fakes/ticker.sh")),
    (
        "oneshot-claude.sh",
        include_str!("../fakes/oneshot-claude.sh"),
    ),
    (
        "oneshot-codex.sh",
        include_str!("../fakes/oneshot-codex.sh"),
    ),
];

pub fn main(args: &[String]) -> i32 {
    let value = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    if let Some(file) = value("--table") {
        return table(Path::new(&file));
    }
    let filter = value("--filter");
    let jobs: usize = value("--jobs").and_then(|j| j.parse().ok()).unwrap_or(10);
    let out = PathBuf::from(value("--out").unwrap_or_else(|| "results.jsonl".into()));
    let mut specs = matrix::all();
    if let Some(f) = &filter {
        specs.retain(|s| s.id().contains(f.as_str()));
    }
    if args.iter().any(|a| a == "--list") {
        for s in &specs {
            println!("{}", s.id());
        }
        return 0;
    }
    let exe = std::env::current_exe().expect("current exe");
    let total = specs.len();
    eprintln!("running {total} cases, {jobs} at a time");
    let queue = Arc::new(Mutex::new(specs));
    let results = Arc::new(Mutex::new(Vec::<Value>::new()));
    let mut workers = Vec::new();
    for _ in 0..jobs {
        let (queue, results, exe) = (queue.clone(), results.clone(), exe.clone());
        workers.push(thread::spawn(move || {
            loop {
                let Some(spec) = queue.lock().unwrap().pop() else {
                    break;
                };
                let record = run_case(spec, &exe);
                let done = {
                    let mut r = results.lock().unwrap();
                    r.push(record.clone());
                    r.len()
                };
                eprintln!("[{done}/{total}] {} -> {}", spec.id(), summary(&record));
            }
        }));
    }
    for w in workers {
        w.join().unwrap();
    }
    let mut records = results.lock().unwrap().clone();
    records.sort_by_key(|r| r["id"].as_str().unwrap_or("").to_owned());
    let mut text = String::new();
    for r in &records {
        text.push_str(&r.to_string());
        text.push('\n');
    }
    fs::write(&out, text).expect("write results");
    // Final global check across every case directory ever created by this run.
    let stray: Vec<_> = procs::ps()
        .into_iter()
        .filter(|p| p.command.contains("delega-spike-"))
        .collect();
    eprintln!(
        "done: {} records in {}; harness force-killed processes in {} cases; stray processes now: {}",
        records.len(),
        out.display(),
        records
            .iter()
            .filter(|r| r["forced_cleanup"]
                .as_array()
                .is_some_and(|a| !a.is_empty()))
            .count(),
        stray.len()
    );
    for p in &stray {
        eprintln!("stray: {} {}", p.pid, p.command);
    }
    if stray.is_empty() { 0 } else { 1 }
}

fn summary(record: &Value) -> String {
    let stage = &record["stages"][0];
    format!(
        "{} {} @{}ms 1s:[{}] 5s:[{}]",
        stage["status"].as_str().unwrap_or("?"),
        stage["error_kind"].as_str().unwrap_or("-"),
        stage["elapsed_ms"],
        stage["survivors_1s"].as_str().unwrap_or("?"),
        stage["survivors_5s"].as_str().unwrap_or("?"),
    )
}

fn setup(dir: &Path, spec: &Spec) {
    for (name, body) in FAKES {
        let path = dir.join(name);
        fs::write(&path, body).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::create_dir(dir.join("work")).unwrap();
    fs::create_dir(dir.join("home")).unwrap();
    let fake = format!("oneshot-{}.sh", spec.prov.name());
    let launcher = dir.join("provider");
    fs::write(
        &launcher,
        format!(
            "#!/bin/sh\nD=\"$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd -P)\"\nexec /bin/bash \"$D/{fake}\" \"$@\"\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&launcher, fs::Permissions::from_mode(0o755)).unwrap();
}

fn sleep_until(t: Instant) {
    let now = Instant::now();
    if t > now {
        thread::sleep(t - now);
    }
}

/// Survivors one second and five seconds after `t0`.
fn measure(t0: Instant, marker: &str, dir: &Path) -> (String, String, Vec<String>) {
    sleep_until(t0 + Duration::from_secs(1));
    let one = procs::survivors(marker, dir);
    sleep_until(t0 + Duration::from_secs(5));
    let five = procs::survivors(marker, dir);
    let detail = five
        .iter()
        .map(|s| format!("{}:{}", s.label(), s.pid))
        .collect();
    (procs::render(&one), procs::render(&five), detail)
}

pub fn run_case(spec: Spec, exe: &Path) -> Value {
    let id = spec.id();
    let dir = tempfile::Builder::new()
        .prefix(&format!("delega-spike-{}-", id.replace('+', "")))
        .tempdir()
        .expect("tempdir");
    let marker = dir
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    setup(dir.path(), &spec);

    let mut command = Command::new(exe);
    command
        .args([
            "--case",
            spec.case.id,
            "--path",
            spec.path.name(),
            "--prov",
            spec.prov.name(),
            "--variant",
            spec.variant,
        ])
        .env("SPIKE_DIR", dir.path())
        .env("SPIKE_CASE", spec.case.letter.to_string())
        .env("SPIKE_VARIANT", spec.variant)
        .env("HOME", dir.path().join("home"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    if spec.stream {
        command.arg("--stream");
    }
    let mut child = command.spawn().expect("spawn case runner");
    let runner_pid = child.id() as i32;
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    let (tx, rx) = mpsc::channel::<(Instant, String)>();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send((Instant::now(), line)).is_err() {
                break;
            }
        }
    });
    let err_text = Arc::new(Mutex::new(String::new()));
    {
        let err_text = err_text.clone();
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let mut t = err_text.lock().unwrap();
                if t.len() < 4000 {
                    t.push_str(&line);
                    t.push('\n');
                }
            }
        });
    }

    let backstop = spec.hard_limit() + Duration::from_secs(20);
    let mut stages: Vec<Value> = Vec::new();
    let expected = if spec.has_shutdown_stage() { 2 } else { 1 };
    for n in 0..expected {
        if n == 1
            && stdin
                .write_all(b"go\n")
                .and_then(|()| stdin.flush())
                .is_err()
        {
            break;
        }
        match rx.recv_timeout(backstop.max(Duration::from_secs(40))) {
            Ok((at, line)) => {
                let mut stage: Value =
                    serde_json::from_str(&line).unwrap_or_else(|_| json!({"raw": line}));
                let (one, five, detail) = measure(at, &marker, dir.path());
                stage["survivors_1s"] = json!(one);
                stage["survivors_5s"] = json!(five);
                stage["survivors_5s_pids"] = json!(detail);
                stages.push(stage);
            }
            Err(_) => {
                stages.push(json!({
                    "stage": if n == 0 { "result" } else { "shutdown" },
                    "status": "RUNNER-STUCK",
                    "note": "the case process printed nothing before the harness backstop",
                    "survivors_1s": procs::render(&procs::survivors(&marker, dir.path())),
                    "survivors_5s": procs::render(&procs::survivors(&marker, dir.path())),
                }));
                break;
            }
        }
    }

    // Cleanup: end the runner, then kill whatever is left and record it.
    drop(stdin);
    let end = Instant::now() + Duration::from_secs(3);
    while Instant::now() < end && child.try_wait().ok().flatten().is_none() {
        thread::sleep(Duration::from_millis(50));
    }
    unsafe { libc::killpg(runner_pid, libc::SIGKILL) };
    let _ = child.wait();
    let mut forced: Vec<String> = Vec::new();
    for _ in 0..6 {
        let left = procs::survivors(&marker, dir.path());
        if left.is_empty() {
            break;
        }
        forced.extend(left.iter().map(|s| format!("{}:{}", s.label(), s.pid)));
        procs::kill_all(&left);
        thread::sleep(Duration::from_millis(300));
    }
    let still = procs::survivors(&marker, dir.path());
    thread::sleep(Duration::from_millis(50));
    let stderr_text = err_text.lock().unwrap().clone();
    json!({
        "id": id,
        "path": spec.path.name(),
        "stream": spec.stream,
        "provider": spec.prov.name(),
        "case": spec.case.id,
        "case_desc": spec.case.desc,
        "variant": spec.variant,
        "stages": stages,
        "forced_cleanup": forced,
        "not_killable": procs::render(&still),
        "runner_stderr": stderr_text,
    })
}

// ---------------------------------------------------------------- table

fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

fn short(s: &str, n: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= n {
        s
    } else {
        format!("{}...", s.chars().take(n).collect::<String>())
    }
}

fn outcome_text(stage: &Value) -> String {
    let status = stage["status"].as_str().unwrap_or("?");
    let mut text = status.to_owned();
    if let Some(kind) = stage["error_kind"].as_str() {
        text.push_str(&format!(" / {kind}"));
    }
    if let Some(code) = stage["exit_code"].as_i64() {
        text.push_str(&format!(" (exit {code})"));
    }
    if let Some(received) = stage["caller_received"].as_str() {
        text.push_str(&format!(": {received}"));
    }
    if let Some(sig) = stage["signal"].as_str() {
        text.push_str(&format!(" (signal {sig})"));
    }
    if let Some(msg) = stage["message"].as_str() {
        text.push_str(&format!(": {}", short(msg, 70)));
    }
    if let Some(reason) = stage["cleanup_reason"].as_str() {
        text.push_str(&format!(" [reason: {reason}]"));
    }
    if let Some(states) = stage["cleanup_state"].as_array() {
        let states: Vec<&str> = states.iter().filter_map(Value::as_str).collect();
        text.push_str(&format!(" [cleanup: {}]", states.join(",")));
    }
    if let Some(w) = stage["cleanup_warnings"]
        .as_array()
        .filter(|w| !w.is_empty())
    {
        let w: Vec<String> = w
            .iter()
            .filter_map(|x| x.as_str())
            .map(|x| short(x, 50))
            .collect();
        text.push_str(&format!(" [warnings: {}]", w.join(" / ")));
    }
    text
}

/// Markdown table from a results file.
pub fn table(file: &Path) -> i32 {
    let text = fs::read_to_string(file).expect("read results");
    let records: Vec<Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    println!(
        "| Path | Provider | Case | Variant | Outcome received | Time (ms) | Survivors 1 s | Survivors 5 s | After shutdown (time ms; 1 s; 5 s) | Note |"
    );
    println!("| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |");
    for r in &records {
        let stage = &r["stages"][0];
        let shutdown = r["stages"]
            .as_array()
            .and_then(|a| a.get(1))
            .map(|s| {
                format!(
                    "{}; {}; {}; {}",
                    s["status"].as_str().unwrap_or("?"),
                    s["elapsed_ms"],
                    s["survivors_1s"].as_str().unwrap_or("?"),
                    s["survivors_5s"].as_str().unwrap_or("?")
                )
            })
            .unwrap_or_else(|| "n/a".into());
        let forced = r["forced_cleanup"].as_array().map_or(0, Vec::len);
        let mut note = stage["note"].as_str().unwrap_or("").to_owned();
        if forced > 0 {
            note.push_str(&format!(" [harness killed {forced} leftover]"));
        }
        println!(
            "| {}{} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            r["path"].as_str().unwrap_or(""),
            if r["stream"].as_bool() == Some(true) {
                " (stream)"
            } else {
                ""
            },
            r["provider"].as_str().unwrap_or(""),
            r["case"].as_str().unwrap_or(""),
            r["variant"].as_str().unwrap_or(""),
            cell(&outcome_text(stage)),
            stage["elapsed_ms"],
            cell(stage["survivors_1s"].as_str().unwrap_or("?")),
            cell(stage["survivors_5s"].as_str().unwrap_or("?")),
            cell(&shutdown),
            cell(note.trim()),
        );
    }
    0
}
