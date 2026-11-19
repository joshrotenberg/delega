//! The MCP tools, in process, against the fake providers.
//!
//! Each test builds the router over a real supervisor and talks to it through
//! tower-mcp's test client, which sends the same JSON-RPC requests a host would.
//! No real provider CLI is started. Each test copies the fakes into a directory
//! of its own, so that directory's path is in the command line of every process
//! the test starts, and ends by listing the process table and failing if any
//! process still carries it.
//!
//! A run is held open by a gate: the prompt `gate:<file>` makes the fake write
//! `<file>.started`, then wait until `<file>` exists. The order of events a test
//! depends on is therefore a fact about files, not about timing, and no test
//! asserts how long anything took.

#![cfg(unix)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Duration;

use delega_core::contract::request::Provider;
use delega_core::provider::ProviderOptions;
use delega_core::supervisor::{Limits, Supervisor, SupervisorConfig};
use delega_mcp::BuildInfo;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower_mcp::{CallToolResult, TestClient};

mod resources;
mod tools;

/// How many of these tests may run at once. Each starts shell processes and
/// polls them, and the machine running them is often busy.
const CONCURRENT_TESTS: usize = 3;

static SLOTS: Slots = Slots {
    free: Mutex::new(CONCURRENT_TESTS),
    freed: Condvar::new(),
};

struct Slots {
    free: Mutex<usize>,
    freed: Condvar,
}

/// A held slot. Dropping it lets the next test start.
struct Slot;

impl Slots {
    fn take(&'static self) -> Slot {
        let mut free = self.free.lock().unwrap_or_else(PoisonError::into_inner);
        while *free == 0 {
            free = self
                .freed
                .wait(free)
                .unwrap_or_else(PoisonError::into_inner);
        }
        *free -= 1;
        Slot
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        *SLOTS.free.lock().unwrap_or_else(PoisonError::into_inner) += 1;
        SLOTS.freed.notify_one();
    }
}

/// A run held open until the test lets it go.
pub(crate) struct Gate(PathBuf);

impl Gate {
    /// The prompt that holds a run on this gate.
    pub fn prompt(&self) -> String {
        format!("gate:{}", self.0.display())
    }

    /// Let the run finish.
    pub fn open(&self) {
        std::fs::write(&self.0, b"").expect("open the gate");
    }

    /// Wait until the fake has begun the run held on this gate.
    pub async fn started(&self) {
        let mut marker = self.0.clone().into_os_string();
        marker.push(".started");
        let marker = PathBuf::from(marker);
        for _ in 0..6000 {
            if marker.exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the run held on {} never started", self.0.display());
    }
}

/// A private area per test, with the supervisor and a client over its server.
pub(crate) struct Rig {
    _slot: Slot,
    _tmp: TempDir,
    root: PathBuf,
    pub work: PathBuf,
    record: PathBuf,
    pub session: String,
    pub supervisor: Supervisor,
    pub client: TestClient,
}

impl Rig {
    pub async fn new(provider: Provider) -> Self {
        Self::with_limits(provider, Limits::default()).await
    }

    pub async fn with_limits(provider: Provider, limits: Limits) -> Self {
        let slot = SLOTS.take();
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
        let session = format!(
            "session-{}",
            root.file_name().and_then(|n| n.to_str()).unwrap_or("x")
        );
        let work = make("work");
        let record = make("record");
        let home = make("home");
        let config = make("config");
        let name = match provider {
            Provider::Claude => "fake-claude.sh",
            Provider::Codex => "fake-codex.sh",
        };
        let text = |p: &Path| p.to_string_lossy().into_owned();
        let options = ProviderOptions {
            executable: Some(fakes.join(name)),
            parent_env: Some(vec![
                (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
                (OsString::from("HOME"), home.clone().into_os_string()),
            ]),
            env_set: vec![
                ("FAKE_RECORD_DIR".into(), text(&record)),
                ("FAKE_SESSION_ID".into(), session.clone()),
                ("XDG_CONFIG_HOME".into(), text(&config)),
                ("CLAUDE_CONFIG_DIR".into(), text(&config)),
                ("CODEX_HOME".into(), text(&config)),
            ],
            stop_grace: Some(Duration::from_millis(200)),
            ..ProviderOptions::default()
        };
        let supervisor = Supervisor::new(SupervisorConfig {
            limits,
            provider: options,
            ..SupervisorConfig::default()
        });
        let router = delega_mcp::router(supervisor.clone(), &BuildInfo::default())
            .expect("the router for the supervisor");
        let mut client = TestClient::from_router(router);
        client.initialize().await;
        Self {
            _slot: slot,
            _tmp: tmp,
            root,
            work,
            record,
            session,
            supervisor,
            client,
        }
    }

    pub fn gate(&self, name: &str) -> Gate {
        Gate(self.root.join(format!("gate-{name}")))
    }

    /// The arguments of `start` for a prompt, in this rig's working directory.
    pub fn start_args(&self, provider: &str, prompt: &str) -> Value {
        json!({"provider": provider, "prompt": prompt, "cwd": self.work})
    }

    /// The arguments the fake received on its latest start.
    pub fn argv(&self) -> Vec<String> {
        let bytes = std::fs::read(self.record.join("argv")).unwrap_or_default();
        let mut parts: Vec<String> = bytes
            .split(|b| *b == 0)
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .collect();
        parts.pop();
        parts
    }

    /// What the fake received on standard input on its latest start.
    pub fn stdin(&self) -> String {
        std::fs::read_to_string(self.record.join("stdin")).unwrap_or_default()
    }

    /// Call a tool and return its result, which must not be an error, after
    /// checking that its text is the same JSON as its structured content.
    pub async fn call(&mut self, tool: &str, args: Value) -> Value {
        let result = self.client.call_tool(tool, args).await;
        assert!(
            !result.is_error,
            "{tool} returned an error: {}",
            result.all_text()
        );
        agree(&result)
    }

    /// Call a tool that must return a tool error, and return its message.
    pub async fn call_error(&mut self, tool: &str, args: Value) -> String {
        let raw = self.client.call_tool_expect_error(tool, args).await;
        match raw.get("content") {
            Some(content) => content[0]["text"].as_str().unwrap_or_default().to_string(),
            None => raw.to_string(),
        }
    }

    /// Wait for a run to finish, repeating a short wait that may time out.
    pub async fn finished(&mut self, run: &str) -> Value {
        let mut cursor = Value::Null;
        for _ in 0..240 {
            let mut args = json!({"runs": [run], "timeout_secs": 30});
            if !cursor.is_null() {
                args["after"] = cursor.clone();
            }
            let waited = self.call("wait", args).await;
            if waited["runs"][0]["outcome"].is_string() {
                return waited;
            }
            cursor = waited["cursor"].clone();
        }
        panic!("run {run} did not finish in time");
    }

    /// Read a run in full detail.
    pub async fn read_full(&mut self, run: &str) -> Value {
        self.call("read", json!({"run": run, "detail": "full"}))
            .await
    }

    /// Processes whose command line carries this rig's directory.
    fn survivors(&self) -> Vec<String> {
        let out = std::process::Command::new("/bin/ps")
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

    /// Stop every worker, then fail if a process started for this test is
    /// still alive after a generous wait.
    pub async fn finish(self) {
        let report = self.supervisor.shutdown().await;
        assert!(report.unsettled.is_empty(), "{report:?}");
        let mut left = Vec::new();
        for _ in 0..6000 {
            left = self.survivors();
            if left.is_empty() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("processes are still running: {left:#?}");
    }
}

/// The structured content of a result, after checking that the text block holds
/// the same JSON.
pub(crate) fn agree(result: &CallToolResult) -> Value {
    let structured = result
        .structured_content
        .clone()
        .unwrap_or_else(|| panic!("no structured content: {}", result.all_text()));
    let text: Value = serde_json::from_str(result.first_text().expect("a text block"))
        .expect("the text block is JSON");
    assert_eq!(
        text, structured,
        "the text and the structured content differ"
    );
    structured
}

/// Assert a cleanup verdict after a stop is honest. On a loaded machine the
/// listing before the first signal can miss its budget, and the result then
/// says so instead of confirming. Whether anything is really left is measured
/// separately by [`Rig::finish`].
pub(crate) fn assert_honest_cleanup(cleanup: &Value) {
    if cleanup["confirmed"] == json!(true) {
        return;
    }
    let reason = cleanup["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("process listing")
            || reason.contains("process inspection was unavailable")
            || reason.contains("stopping was not confirmed"),
        "an unconfirmed cleanup must give the missed-listing reason: {cleanup}"
    );
}
