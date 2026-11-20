//! `delega mcp` as a black box: the built executable, speaking JSON-RPC over its
//! pipes, against the fake providers.
//!
//! Each test copies the fakes into a directory of its own, so that directory's
//! path is in the command line of every process a worker starts (the provider
//! script, and the child scripts the `acc:hang:setsid` behaviour leaves running,
//! one of them in a session of its own). That marker is what the tests look for
//! in the process table, and they do not trust what the server reports. The
//! executable starts with an empty environment plus the few variables set here,
//! a temporary HOME and the copied fakes as the provider executables, so nothing
//! reads real provider state and no real provider CLI is ever started.
//!
//! No test asserts how long anything took. Every wait is for an event, with a
//! generous deadline, because the machine running them is often heavily loaded.

#![cfg(unix)]

mod support;

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;

/// How long a test waits for an event that should happen eventually.
const EVENTUALLY: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(20);

/// The behaviour of the fake that runs until stopped, with a child in the
/// worker's group and a grandchild in a session of its own.
const LONG_RUNNING: &str = "acc:hang:setsid";

/// Only one of these tests runs at a time: each starts several processes and
/// polls the process table, on a machine that is often busy.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// A value no output may contain, given to the server's environment and named
/// with `--env`, to show that a value is never echoed.
const SECRET_VALUE: &str = "never-echo-this-value-7f3a";

struct Rig {
    _guard: MutexGuard<'static, ()>,
    _tmp: TempDir,
    root: PathBuf,
    fakes: PathBuf,
    work: PathBuf,
    home: PathBuf,
    record: PathBuf,
}

impl Rig {
    fn new() -> Self {
        let guard = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
        let tmp = tempfile::tempdir().expect("create temporary directory");
        let root = std::fs::canonicalize(tmp.path()).expect("resolve temporary directory");
        let make = |name: &str| {
            let path = root.join(name);
            std::fs::create_dir(&path).expect("create directory");
            path
        };
        let fakes = make("fakes");
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../delega-core/src/provider/tests/fakes");
        for name in [
            "fake-claude.sh",
            "fake-codex.sh",
            "acc-lib.sh",
            "acc-child.sh",
        ] {
            // A link, never a written copy: a file open for writing cannot be
            // executed on Linux (`ETXTBSY`), and a `fork` on another thread
            // inherits the descriptor of a copy being written.
            std::os::unix::fs::symlink(source.join(name), fakes.join(name)).expect("link a fake");
        }
        Self {
            _guard: guard,
            work: make("work"),
            home: make("home"),
            record: make("record"),
            fakes,
            root,
            _tmp: tmp,
        }
    }

    /// `delega mcp` with a controlled environment, started with its pipes.
    fn start(&self, args: &[&str]) -> Session {
        self.start_with(args, &[])
    }

    /// Like [`Rig::start`], with more variables in the server's environment.
    fn start_with(&self, args: &[&str], more_env: &[(&str, &str)]) -> Session {
        let config = self.root.join("home");
        let mut child = Command::new(env!("CARGO_BIN_EXE_delega"))
            .arg("mcp")
            .args(args)
            .env_clear()
            .envs(more_env.iter().copied())
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &config)
            .env("CLAUDE_CONFIG_DIR", &config)
            .env("CODEX_HOME", &config)
            .env("DELEGA_CLAUDE_BIN", self.fakes.join("fake-claude.sh"))
            .env("DELEGA_CODEX_BIN", self.fakes.join("fake-codex.sh"))
            .env("FAKE_RECORD_DIR", &self.record)
            .env("MCP_BLACKBOX_VALUE", SECRET_VALUE)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn delega mcp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let stderr = child.stderr.take().expect("stderr");
        let lines = Arc::new(Mutex::new(Vec::<String>::new()));
        let (sender, receiver) = channel::<Value>();
        let reader_lines = lines.clone();
        let paused = Arc::new(AtomicBool::new(false));
        let reader_paused = paused.clone();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                // A paused reader stops reading standard output, as a client
                // that is frozen or no longer interested does.
                while reader_paused.load(Ordering::SeqCst) {
                    std::thread::sleep(POLL);
                }
                let Ok(line) = line else { break };
                reader_lines
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(line.clone());
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    let _ = sender.send(value);
                }
            }
        });
        let error_text = Arc::new(Mutex::new(String::new()));
        let error_sink = error_text.clone();
        let error_reader = std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                let mut text = error_sink.lock().unwrap_or_else(PoisonError::into_inner);
                text.push_str(&line);
                text.push('\n');
            }
        });
        Session {
            child,
            stdin: Some(stdin),
            messages: receiver,
            lines,
            error_text,
            readers: vec![reader, error_reader],
            paused,
            next_id: 1,
        }
    }

    /// Write the user-level configuration file where this rig's
    /// `XDG_CONFIG_HOME` points, private to the user.
    fn write_config(&self, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = self.root.join("home").join("delega").join("config.toml");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create directory");
        std::fs::write(&path, body).expect("write config");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        path
    }

    /// Processes whose command line carries this rig's directory.
    fn survivors(&self) -> Vec<String> {
        let out = Command::new("/bin/ps")
            .args(["-axww", "-o", "pid=,command="])
            .output()
            .expect("run ps");
        let marker = self.root.to_string_lossy().into_owned();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| l.contains(&marker))
            .map(|l| l.trim().to_string())
            .collect()
    }

    /// Wait until every process of the workers is gone. The server was waited
    /// for already; this is the proof, taken from the process table.
    fn assert_nothing_left(&self) {
        let began = Instant::now();
        let mut left = self.survivors();
        while !left.is_empty() {
            assert!(
                began.elapsed() < EVENTUALLY,
                "processes are still running after the server exited: {left:#?}"
            );
            std::thread::sleep(POLL);
            left = self.survivors();
        }
    }

    /// How many lines of `pids` (written by the fakes' `acc:` behaviours) begin
    /// with `role`.
    fn recorded(&self, role: &str) -> usize {
        std::fs::read_to_string(self.fakes.join("pids"))
            .unwrap_or_default()
            .lines()
            .filter(|line| line.split_whitespace().next() == Some(role))
            .count()
    }

    /// Wait until `count` workers are running with their descendants.
    fn wait_for_workers(&self, count: usize) {
        let began = Instant::now();
        while self.recorded("provider") < count || self.recorded("leaf-setsid") < count {
            assert!(
                began.elapsed() < EVENTUALLY,
                "the workers did not all start: {} providers, {} grandchildren",
                self.recorded("provider"),
                self.recorded("leaf-setsid")
            );
            std::thread::sleep(POLL);
        }
        // Every worker's processes are in the table now.
        assert!(
            self.survivors().len() >= count * 3,
            "expected the providers, their children and grandchildren: {:#?}",
            self.survivors()
        );
    }
}

/// A running `delega mcp` and its pipes.
struct Session {
    child: Child,
    stdin: Option<ChildStdin>,
    messages: Receiver<Value>,
    lines: Arc<Mutex<Vec<String>>>,
    error_text: Arc<Mutex<String>>,
    readers: Vec<std::thread::JoinHandle<()>>,
    paused: Arc<AtomicBool>,
    next_id: u64,
}

impl Session {
    fn send(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().expect("standard input is open");
        writeln!(stdin, "{message}").expect("write to the server");
        stdin.flush().expect("flush");
    }

    /// Send a request without waiting, and return its id.
    fn send_request(&mut self, method: &str, params: Value) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        id
    }

    /// Wait for the response to request `id`, which must not be an error.
    fn response(&mut self, id: u64) -> Value {
        let began = Instant::now();
        loop {
            let left = EVENTUALLY.saturating_sub(began.elapsed());
            match self.messages.recv_timeout(left) {
                Ok(message) if message["id"] == json!(id) => {
                    assert!(message.get("error").is_none(), "request {id}: {message}");
                    return message["result"].clone();
                }
                // A notification, or the response to something else.
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => panic!("no response to request {id}"),
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("the server closed standard output before answering request {id}")
                }
            }
        }
    }

    /// Send a request and wait for its response.
    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.send_request(method, params);
        self.response(id)
    }

    fn initialize(&mut self) -> Value {
        let result = self.request(
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "black-box-test", "version": "1"},
            }),
        );
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        result
    }

    /// Call a tool and return its structured result, which must not be an error.
    fn call(&mut self, tool: &str, arguments: Value) -> Value {
        let result = self.request("tools/call", json!({"name": tool, "arguments": arguments}));
        assert_ne!(result["isError"], json!(true), "{tool}: {result}");
        result["structuredContent"].clone()
    }

    fn close_stdin(&mut self) {
        self.stdin = None;
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Wait for the server to exit, killing it and failing the test past the
    /// deadline, and return its status.
    fn exit(&mut self) -> ExitStatus {
        let status = support::wait_exit_only(&mut self.child, EVENTUALLY);
        self.paused.store(false, Ordering::SeqCst);
        for reader in self.readers.drain(..) {
            reader.join().expect("reader thread");
        }
        status
    }

    /// Everything the server wrote to standard output, which must be nothing
    /// but JSON-RPC messages, one per line.
    fn assert_only_json_rpc(&self) -> Vec<Value> {
        let lines = self.lines.lock().unwrap_or_else(PoisonError::into_inner);
        assert!(!lines.is_empty(), "the server wrote nothing");
        lines
            .iter()
            .map(|line| {
                let value: Value = serde_json::from_str(line)
                    .unwrap_or_else(|err| panic!("not JSON on standard output: {err}: {line:?}"));
                assert_eq!(value["jsonrpc"], "2.0", "{line}");
                assert!(
                    value.get("method").is_some()
                        || value.get("result").is_some()
                        || value.get("error").is_some(),
                    "not a JSON-RPC message: {line}"
                );
                value
            })
            .collect()
    }

    fn stderr(&self) -> String {
        self.error_text
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for Session {
    /// A test that fails leaves nothing running: the server is killed, which
    /// stops its workers through the supervisor's drop path only when it can,
    /// so the rig's own check in the test body is what proves cleanup.
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            self.stdin = None;
            let _ = support::wait_exit_only(&mut self.child, EVENTUALLY);
        }
    }
}

/// Start two workers that run until stopped, and wait until both are running.
fn start_two_workers(rig: &Rig, session: &mut Session) -> Vec<Value> {
    let started: Vec<Value> = (0..2)
        .map(|n| {
            session.call(
                "start",
                json!({
                    "provider": "codex",
                    "prompt": LONG_RUNNING,
                    "cwd": rig.work,
                    "name": format!("worker-{n}"),
                }),
            )
        })
        .collect();
    for response in &started {
        assert!(response["worker"].is_string(), "{response}");
        assert!(response["run"].is_string(), "{response}");
        assert!(
            ["queued", "running"].contains(&response["state"].as_str().expect("state")),
            "{response}"
        );
    }
    rig.wait_for_workers(2);
    started
}

/// Assert the exit status of a stop. Where the process listing taken before the
/// first signal came late (a loaded machine), the process layer cannot confirm
/// cleanup and says so, and the server then exits 1 and names that reason. That
/// is the documented behaviour, so it is accepted here, and only for that
/// reason; whether anything is left running is checked separately.
fn assert_stop_status(session: &Session, status: ExitStatus, expected: i32) {
    use delega_core::process::LATE_LISTING_REASON;
    if status.code() == Some(expected) {
        return;
    }
    let error = session.stderr();
    let lines: Vec<&str> = error
        .lines()
        .filter(|l| l.contains("cleanup not confirmed"))
        .collect();
    assert!(
        status.code() == Some(1)
            && !lines.is_empty()
            && lines.iter().all(|l| l.contains(LATE_LISTING_REASON)),
        "expected status {expected}, or 1 for a late listing only: {status:?}; stderr: {error}"
    );
}

fn assert_no_secret_value(session: &Session) {
    let lines = session.lines.lock().unwrap_or_else(PoisonError::into_inner);
    assert!(
        !lines.iter().any(|l| l.contains(SECRET_VALUE)),
        "standard output holds an environment value"
    );
    assert!(
        !session.stderr().contains(SECRET_VALUE),
        "standard error holds an environment value"
    );
}

#[test]
fn closing_standard_input_stops_every_worker_and_exits_zero() {
    let rig = Rig::new();
    let mut session = rig.start(&[
        "--env",
        "MCP_BLACKBOX_VALUE",
        "-e",
        "MCP_SET=assigned",
        "--env",
        "FAKE_RECORD_DIR",
    ]);
    let initialized = session.initialize();
    assert_eq!(initialized["serverInfo"]["name"], "delega");
    let tools = session.request("tools/list", json!({}));
    let names: Vec<&str> = tools["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|t| t["name"].as_str().expect("name"))
        .collect();
    // The server lists tools by name.
    assert_eq!(
        names,
        [
            "close",
            "follow_up",
            "interrupt",
            "list",
            "read",
            "start",
            "wait"
        ]
    );

    let started = start_two_workers(&rig, &mut session);
    let listed = session.call("list", json!({}));
    assert_eq!(listed["workers"].as_array().expect("workers").len(), 2);
    assert_eq!(listed["limits"]["live_workers"], 2);
    // The --env names reached the workers, by name; no value is in any output.
    let env_names = std::fs::read_to_string(rig.record.join("env")).unwrap_or_default();
    assert!(
        env_names.lines().any(|n| n == "MCP_BLACKBOX_VALUE"),
        "{env_names}"
    );
    assert!(env_names.lines().any(|n| n == "MCP_SET"), "{env_names}");
    assert!(!rig.survivors().is_empty());

    session.close_stdin();
    let status = session.exit();
    assert_stop_status(&session, status, 0);
    rig.assert_nothing_left();

    let messages = session.assert_only_json_rpc();
    // The runs were stopped, not abandoned: nothing was left to finish later.
    assert!(messages.len() >= 5, "{messages:#?}");
    assert_no_secret_value(&session);
    assert_eq!(started.len(), 2);
}

#[test]
fn a_start_without_an_absolute_cwd_is_refused_and_starts_nothing() {
    let rig = Rig::new();
    let mut session = rig.start(&["--env", "FAKE_RECORD_DIR"]);
    session.initialize();
    for args in [
        json!({"provider": "codex", "prompt": "success"}),
        json!({"provider": "codex", "prompt": "success", "cwd": "relative/dir"}),
    ] {
        let refused = session.call("start", args);
        assert_eq!(refused["state"], "finished", "{refused}");
        assert_eq!(refused["outcome"], "refused", "{refused}");
        assert!(refused.get("worker").is_none(), "{refused}");
        assert_eq!(refused["error"]["kind"], "cwd_required", "{refused}");
        let read = session.call("read", json!({"run": refused["run"]}));
        assert_eq!(read["result"]["outcome"], "refused", "{read}");
        assert_eq!(read["result"]["error"]["kind"], "cwd_required", "{read}");
    }
    let listed = session.call("list", json!({}));
    assert!(
        listed["workers"].as_array().expect("workers").is_empty(),
        "{listed}"
    );
    assert!(
        !rig.record.join("argv").exists(),
        "no provider process was started"
    );
    session.close_stdin();
    let status = session.exit();
    assert_stop_status(&session, status, 0);
    rig.assert_nothing_left();
}

#[test]
fn sigterm_stops_every_worker_and_exits_with_the_interrupted_status() {
    let rig = Rig::new();
    let mut session = rig.start(&[]);
    session.initialize();
    start_two_workers(&rig, &mut session);

    // A second signal straight after the first must not end the server before
    // the workers are stopped. If the server has already finished, there is
    // nothing to signal and the failure to deliver is expected.
    support::signal(session.pid(), "TERM");
    let _ = Command::new("/bin/kill")
        .args(["-TERM", &session.pid().to_string()])
        .stderr(Stdio::null())
        .status();
    let status = session.exit();
    // By itself, with status 130, not killed by the second signal.
    assert!(status.code().is_some(), "killed by a signal: {status:?}");
    assert_stop_status(&session, status, 130);
    rig.assert_nothing_left();

    session.assert_only_json_rpc();
    assert!(
        session.stderr().contains("signal received"),
        "{}",
        session.stderr()
    );
    assert_no_secret_value(&session);
}

#[test]
fn sighup_sigint_and_sigquit_stop_the_workers_like_sigterm() {
    for name in ["HUP", "INT", "QUIT"] {
        let rig = Rig::new();
        let mut session = rig.start(&[]);
        session.initialize();
        let started = session.call(
            "start",
            json!({"provider": "claude", "prompt": LONG_RUNNING, "cwd": rig.work}),
        );
        assert!(started["worker"].is_string(), "{started}");
        rig.wait_for_workers(1);
        support::signal(session.pid(), name);
        let status = session.exit();
        assert_stop_status(&session, status, 130);
        rig.assert_nothing_left();
        session.assert_only_json_rpc();
    }
}

#[test]
fn an_env_name_the_policy_refuses_stops_the_server_before_it_serves_without_echoing_values() {
    // Parsed and refused as `delega run` parses and refuses them: the kind and
    // message are the provider layer's, the status is the one `run` uses when
    // nothing was started (2), nothing is served, and no value is written.
    for (given, kind) in [
        ("9BAD=secret-value-9d41", "invalid_env_name"),
        ("DELEGA_X=secret-value-9d41", "forbidden_env_name"),
        ("BAD-NAME", "invalid_env_name"),
        (
            "CLAUDE_CODE_ENTRYPOINT=secret-value-9d41",
            "forbidden_env_name",
        ),
    ] {
        let rig = Rig::new();
        let mut session = rig.start(&["--env", given]);
        let status = session.exit();
        assert_eq!(status.code(), Some(2), "{given}: {}", session.stderr());
        let error = session.stderr();
        assert!(error.contains("delega mcp: refused"), "{given}: {error}");
        assert!(error.contains(kind), "{given}: {error}");
        assert!(!error.contains("secret-value-9d41"), "{given}: {error}");
        // The same refusal `delega run` gives for the same flag.
        let run = Command::new(env!("CARGO_BIN_EXE_delega"))
            .args([
                "run",
                "--provider",
                "codex",
                "-p",
                "x",
                "--output",
                "json",
                "--env",
                given,
            ])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("DELEGA_CODEX_BIN", rig.fakes.join("fake-codex.sh"))
            .output()
            .expect("run delega");
        let result: Value = serde_json::from_slice(&run.stdout).expect("run result");
        assert_eq!(result["error"]["kind"], kind, "{given}");
        assert!(
            error.contains(result["error"]["message"].as_str().expect("message")),
            "the message differs from run's: {error} / {result}"
        );
        // Nothing was served.
        assert!(
            session
                .lines
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty()
        );
        rig.assert_nothing_left();
    }
}

#[test]
fn a_call_in_flight_is_answered_by_the_shutdown_before_the_server_exits() {
    // A `wait` whose cursor no run will pass, with the longest time limit, can
    // only be released by the shutdown. It must be answered before the
    // server exits: if the workers were stopped only
    // after the transport finished, the transport's drain limit would abandon the
    // call and no answer would come. No time is measured; the answer either
    // arrives or the test waits out its generous deadline and fails.
    for how in ["end of input", "SIGTERM"] {
        let rig = Rig::new();
        let mut session = rig.start(&[]);
        session.initialize();
        let started = session.call(
            "start",
            json!({"provider": "codex", "prompt": LONG_RUNNING, "cwd": rig.work}),
        );
        rig.wait_for_workers(1);
        let id = session.send_request(
            "tools/call",
            json!({"name": "wait", "arguments": {
                "runs": [started["run"]], "timeout_secs": 300, "after": 1_000_000,
            }}),
        );
        // Make sure the server has read the call before it is told to stop: a
        // later call is answered after it, on the same connection.
        let listed = session.call("list", json!({}));
        assert_eq!(listed["workers"].as_array().map(Vec::len), Some(1));
        if how == "end of input" {
            session.close_stdin();
        } else {
            support::signal(session.pid(), "TERM");
        }
        let answer = session.response(id);
        assert_ne!(answer["isError"], json!(true), "{how}: {answer}");
        let waited = &answer["structuredContent"];
        assert_eq!(waited["timed_out"], false, "{how}: {answer}");
        // Released when the shutdown began, so the run may still be stopping.
        assert_eq!(waited["runs"][0]["run"], started["run"], "{how}: {answer}");
        let status = session.exit();
        assert_stop_status(&session, status, if how == "SIGTERM" { 130 } else { 0 });
        rig.assert_nothing_left();
        session.assert_only_json_rpc();
    }
}

#[test]
fn a_signal_stops_the_workers_and_the_server_exits_though_the_client_stopped_reading() {
    let rig = Rig::new();
    let mut session = rig.start(&[]);
    session.initialize();
    session.call(
        "start",
        json!({"provider": "codex", "prompt": LONG_RUNNING, "cwd": rig.work}),
    );
    rig.wait_for_workers(1);
    // The client stops reading standard output but keeps the pipe open, then
    // keeps asking for the tool list until the pipe is full and the server's
    // write blocks. The writes go on a thread: the server stops reading input
    // once it is blocked, and a write would then block the test.
    session.paused.store(true, Ordering::SeqCst);
    let mut stdin = session.stdin.take().expect("stdin");
    let flood = std::thread::spawn(move || {
        for id in 1000..1400 {
            let request = json!({"jsonrpc": "2.0", "id": id, "method": "tools/list"});
            if writeln!(stdin, "{request}")
                .and_then(|()| stdin.flush())
                .is_err()
            {
                break;
            }
        }
        // Held open until the test ends: end of input must not be what stops it.
        stdin
    });
    // Let the server fill the pipe. Whether it is blocked yet is not observable,
    // so the flood is large and given this long; the signal then arrives in
    // whichever state the server is in, and must be handled in either.
    std::thread::sleep(Duration::from_secs(3));
    support::signal(session.pid(), "TERM");
    // Generous: the transport is given up on after its 15 s bound at the latest.
    let status = session.exit();
    assert_stop_status(&session, status, 130);
    rig.assert_nothing_left();
    drop(flood);
}

#[test]
fn unconfirmed_cleanup_after_a_signal_exits_1_not_130() {
    // `DELEGA_TEST_NO_PROCESS_LISTING` makes process inspection unavailable, so
    // the process layer cannot confirm that anything stopped. The server must
    // say so and exit 1, after a signal as after end of input.
    for how in ["SIGTERM", "end of input"] {
        let rig = Rig::new();
        let mut session = rig.start_with(&[], &[("DELEGA_TEST_NO_PROCESS_LISTING", "1")]);
        session.initialize();
        session.call(
            "start",
            json!({"provider": "codex", "prompt": "acc:hang:child", "cwd": rig.work}),
        );
        // A child in the worker's group only: with process inspection off the
        // layer can reach nothing else, so a grandchild in a session of its own
        // would be left, which the unconfirmed verdict is the warning of.
        let began = Instant::now();
        while rig.recorded("provider") < 1 || rig.recorded("child") < 1 {
            assert!(began.elapsed() < EVENTUALLY, "the worker did not start");
            std::thread::sleep(POLL);
        }
        if how == "SIGTERM" {
            support::signal(session.pid(), "TERM");
        } else {
            session.close_stdin();
        }
        let status = session.exit();
        assert_eq!(
            status.code(),
            Some(1),
            "{how}: stderr: {}",
            session.stderr()
        );
        assert!(
            session.stderr().contains("cleanup not confirmed for run"),
            "{how}: {}",
            session.stderr()
        );
        // The workers were still stopped, as well as the layer could.
        rig.assert_nothing_left();
        session.assert_only_json_rpc();
    }
}

#[test]
fn an_unconfirmed_run_replaced_as_the_latest_by_a_refused_follow_up_still_exits_1() {
    // The first run ends with cleanup unconfirmed: process inspection is off,
    // and the provider starts a child and exits 3 before it writes anything, so
    // it never reports a session. A follow-up is therefore refused when it is
    // dispatched (`no_session`), with a confirmed cleanup, and becomes the
    // worker's latest run. The exit status must still count the earlier run.
    // (Interrupting a hanging provider instead raced its session id: when delega
    // had read it, the follow-up resumed the session and completed.)
    let rig = Rig::new();
    let mut session = rig.start_with(&[], &[("DELEGA_TEST_NO_PROCESS_LISTING", "1")]);
    session.initialize();
    let started = session.call(
        "start",
        json!({"provider": "codex", "prompt": "acc:exit3:child", "cwd": rig.work}),
    );
    let first = finished_result(&mut session, &started["run"]);
    assert_eq!(first["cleanup"]["confirmed"], json!(false), "{first}");
    assert!(first.get("session_id").is_none(), "{first}");

    let followed = session.call(
        "follow_up",
        json!({"worker": started["worker"], "prompt": "more"}),
    );
    let second = finished_result(&mut session, &followed["run"]);
    assert_eq!(second["outcome"], "refused", "{second}");
    assert_eq!(second["error"]["kind"], "no_session", "{second}");
    assert_eq!(second["cleanup"]["confirmed"], json!(true), "{second}");
    let listed = session.call("list", json!({}));
    assert_eq!(
        listed["workers"][0]["last_run"], followed["run"],
        "the refused follow-up is the worker's latest run: {listed}"
    );

    session.close_stdin();
    let status = session.exit();
    assert_eq!(status.code(), Some(1), "stderr: {}", session.stderr());
    assert!(
        session.stderr().contains(&format!(
            "cleanup not confirmed for run {}",
            started["run"].as_str().expect("run id")
        )),
        "{}",
        session.stderr()
    );
    rig.assert_nothing_left();
    session.assert_only_json_rpc();
}

#[test]
fn the_skills_and_the_identity_over_stdio_are_what_the_cli_prints() {
    let rig = Rig::new();
    let mut session = rig.start(&[]);
    session.initialize();
    let listed = session.request("resources/list", json!({}));
    let uris: Vec<&str> = listed["resources"]
        .as_array()
        .expect("resources")
        .iter()
        .map(|r| r["uri"].as_str().expect("uri"))
        .collect();
    assert!(uris.contains(&"delega://identity"), "{uris:?}");

    let cli = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_delega"))
            .args(args)
            .output()
            .expect("run delega")
    };
    let catalog: Value =
        serde_json::from_slice(&cli(&["skill", "list", "--output", "json"]).stdout)
            .expect("skill list JSON");
    let skills = catalog["skills"].as_array().expect("skills");
    assert!(!skills.is_empty());
    for skill in skills {
        let name = skill["name"].as_str().expect("name");
        let uri = format!("skill://delega/{name}/SKILL.md");
        assert!(
            uris.contains(&uri.as_str()),
            "{uri} is not listed: {uris:?}"
        );
        let read = session.request("resources/read", json!({"uri": uri}));
        let served = read["contents"][0]["text"].as_str().expect("text");
        let shown = cli(&["skill", "show", name]).stdout;
        assert_eq!(
            served.as_bytes(),
            shown.as_slice(),
            "{name}: the resource differs from `delega skill show`"
        );
    }

    let read = session.request("resources/read", json!({"uri": "delega://identity"}));
    let identity: Value = serde_json::from_str(read["contents"][0]["text"].as_str().expect("text"))
        .expect("identity JSON");
    let version: Value = serde_json::from_slice(&cli(&["version", "--output", "json"]).stdout)
        .expect("version JSON");
    assert_eq!(
        identity, version,
        "the identity differs from `delega version`"
    );
    assert_eq!(
        identity["skills"].as_array().map(Vec::len),
        Some(skills.len())
    );

    session.close_stdin();
    let status = session.exit();
    assert_stop_status(&session, status, 0);
    rig.assert_nothing_left();
    session.assert_only_json_rpc();
}

/// Read `run` until it has a result, and return the result.
fn finished_result(session: &mut Session, run: &Value) -> Value {
    let began = Instant::now();
    loop {
        let read = session.call("read", json!({"run": run}));
        if read["result"].is_object() {
            return read["result"].clone();
        }
        assert!(
            began.elapsed() < EVENTUALLY,
            "the run did not finish: {read}"
        );
        std::thread::sleep(POLL);
    }
}

#[test]
fn the_files_limits_defaults_and_environment_apply_to_the_server_and_its_starts() {
    let rig = Rig::new();
    // No --env flag: the file alone gives the workers the record directory, a
    // value of its own and the request defaults; the limits are lowered.
    rig.write_config(&format!(
        "schema_version = 1\n\
         [defaults]\nprovider = \"codex\"\nmodel = \"file-model\"\ntimeout_secs = 777\n\
         [env]\npass = [\"FAKE_RECORD_DIR\"]\n[env.set]\nFROM_FILE = \"{SECRET_VALUE}\"\n\
         [mcp]\nmax_live_workers = 2\nmax_queued_per_worker = 3\n\
         wait_default_secs = 1\nwait_ceiling_secs = 2\n"
    ));
    let mut session = rig.start(&[]);
    session.initialize();

    // The limits in force are the file's.
    let listed = session.call("list", json!({}));
    let limits = &listed["limits"];
    assert_eq!(limits["max_live_workers"], 2, "{limits}");
    assert_eq!(limits["max_queued_per_worker"], 3, "{limits}");
    assert_eq!(limits["max_running_runs"], 4, "{limits}");
    assert_eq!(limits["default_wait_secs"], 1, "{limits}");
    assert_eq!(limits["wait_ceiling_secs"], 2, "{limits}");

    // A start with no provider and no timeout takes the file's.
    let first = session.call(
        "start",
        json!({"prompt": "success", "cwd": rig.work, "name": "a"}),
    );
    assert!(first["worker"].is_string(), "{first}");
    let result = finished_result(&mut session, &first["run"]);
    assert_eq!(result["outcome"], "completed", "{result}");
    assert_eq!(result["provider"], "codex", "{result}");
    assert_eq!(result["timing"]["timeout_secs"], 777, "{result}");
    // The model reached the provider's command line, and the file's variables
    // its environment, by name.
    let argv = std::fs::read_to_string(rig.record.join("argv")).expect("argv");
    assert!(argv.split('\0').any(|a| a == "file-model"), "{argv:?}");
    let env_names = std::fs::read_to_string(rig.record.join("env")).expect("env");
    assert!(env_names.lines().any(|n| n == "FROM_FILE"), "{env_names}");

    // An argument of the call beats the file.
    let second = session.call(
        "start",
        json!({"prompt": "success", "cwd": rig.work, "name": "b", "timeout_secs": 55, "provider": "codex"}),
    );
    let result = finished_result(&mut session, &second["run"]);
    assert_eq!(result["timing"]["timeout_secs"], 55, "{result}");

    // A profile beats the file, and an argument beats the profile.
    let profile = rig.root.join("profile.json");
    std::fs::write(
        &profile,
        r#"{"schema_version": 1, "timeout_secs": 66, "model": "profile-model"}"#,
    )
    .expect("write profile");
    session.call("close", json!({"worker": first["worker"]}));
    let third = session.call(
        "start",
        json!({"profile": profile, "prompt": "success", "cwd": rig.work, "name": "c"}),
    );
    let result = finished_result(&mut session, &third["run"]);
    assert_eq!(result["timing"]["timeout_secs"], 66, "{result}");
    let argv = std::fs::read_to_string(rig.record.join("argv")).expect("argv");
    assert!(argv.split('\0').any(|a| a == "profile-model"), "{argv:?}");

    // Two workers are live (b and c); a third is refused at the file's limit.
    let refused = session.call("start", json!({"prompt": "success", "cwd": rig.work}));
    assert_eq!(refused["outcome"], "refused", "{refused}");
    assert_eq!(refused["error"]["kind"], "limit_reached", "{refused}");

    // A wait with no timeout uses the file's default, and is capped at its ceiling.
    session.call("close", json!({"worker": second["worker"]}));
    let long = session.call(
        "start",
        json!({"prompt": LONG_RUNNING, "cwd": rig.work, "name": "d"}),
    );
    rig.wait_for_workers(1);
    let waited = session.call("wait", json!({"runs": [long["run"]]}));
    assert_eq!(waited["timed_out"], true, "{waited}");
    assert_eq!(waited["timeout_secs"], 1, "{waited}");
    let waited = session.call("wait", json!({"runs": [long["run"]], "timeout_secs": 50}));
    assert_eq!(waited["timeout_secs"], 2, "{waited}");

    session.close_stdin();
    let status = session.exit();
    assert_stop_status(&session, status, 0);
    rig.assert_nothing_left();
    session.assert_only_json_rpc();
    assert_no_secret_value(&session);
}

#[test]
fn a_file_the_server_cannot_use_refuses_to_start_it_and_a_missing_one_does_not() {
    // No file: the server starts and serves, as before.
    let rig = Rig::new();
    let mut session = rig.start(&[]);
    session.initialize();
    let listed = session.call("list", json!({}));
    assert_eq!(listed["limits"]["max_live_workers"], 16);
    assert_eq!(listed["limits"]["default_wait_secs"], 30);
    session.close_stdin();
    assert_eq!(session.exit().code(), Some(0), "{}", session.stderr());
    // A rig holds the lock that lets one of these tests run at a time.
    drop(session);
    drop(rig);

    // A group-writable file: refused at start, nothing served, status 2, and
    // the message names the file and holds no value.
    let rig = Rig::new();
    let path = rig.write_config(&format!(
        "schema_version = 1\n[env.set]\nFROM_FILE = \"{SECRET_VALUE}\"\n"
    ));
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o664)).expect("chmod");
    }
    let mut session = rig.start(&[]);
    let status = session.exit();
    assert_eq!(status.code(), Some(2), "{}", session.stderr());
    let error = session.stderr();
    assert!(
        error.contains("delega mcp: refused: config_unreadable"),
        "{error}"
    );
    assert!(error.contains(&path.display().to_string()), "{error}");
    assert!(
        session
            .lines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty(),
        "nothing is served"
    );
    assert_no_secret_value(&session);
}
