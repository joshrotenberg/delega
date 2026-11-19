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
            std::fs::copy(source.join(name), fakes.join(name)).expect("copy a fake");
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
        let config = self.root.join("home");
        let mut child = Command::new(env!("CARGO_BIN_EXE_delega"))
            .arg("mcp")
            .args(args)
            .env_clear()
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
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
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
            next_id: 1,
        }
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
    next_id: u64,
}

impl Session {
    fn send(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().expect("standard input is open");
        writeln!(stdin, "{message}").expect("write to the server");
        stdin.flush().expect("flush");
    }

    /// Send a request and wait for its response.
    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let began = Instant::now();
        loop {
            let left = EVENTUALLY.saturating_sub(began.elapsed());
            match self.messages.recv_timeout(left) {
                Ok(message) if message["id"] == json!(id) => {
                    assert!(message.get("error").is_none(), "{method}: {message}");
                    return message["result"].clone();
                }
                // A notification, or the response to something else.
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => panic!("no response to {method}"),
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("the server closed standard output before answering {method}")
                }
            }
        }
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
    assert_eq!(status.code(), Some(0), "stderr: {}", session.stderr());
    rig.assert_nothing_left();

    let messages = session.assert_only_json_rpc();
    // The runs were stopped, not abandoned: nothing was left to finish later.
    assert!(messages.len() >= 5, "{messages:#?}");
    assert_no_secret_value(&session);
    assert_eq!(started.len(), 2);
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
    assert_eq!(
        status.code(),
        Some(130),
        "the server must exit by itself with status 130, not be killed: {status:?}; stderr: {}",
        session.stderr()
    );
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
        assert_eq!(status.code(), Some(130), "SIG{name}: {status:?}");
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
fn a_call_in_flight_when_input_ends_is_still_answered_or_released() {
    let rig = Rig::new();
    let mut session = rig.start(&[]);
    session.initialize();
    let started = session.call(
        "start",
        json!({"provider": "codex", "prompt": LONG_RUNNING, "cwd": rig.work}),
    );
    rig.wait_for_workers(1);
    // A wait that would last 30 s, then the end of input: shutdown stops the
    // run, which releases the wait, and the server exits by itself.
    let id = session.next_id;
    session.next_id += 1;
    session.send(&json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": {"name": "wait", "arguments": {"runs": [started["run"]], "timeout_secs": 30}},
    }));
    session.close_stdin();
    let status = session.exit();
    assert_eq!(status.code(), Some(0), "stderr: {}", session.stderr());
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
    assert_eq!(session.exit().code(), Some(0));
    rig.assert_nothing_left();
    session.assert_only_json_rpc();
}
