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

use delega_core::contract::applied::AppliedConfig;
use delega_core::contract::partial::PartialRequest;
use delega_core::contract::request::Provider;
use delega_core::provider::ProviderOptions;
use delega_core::supervisor::{Limits, Supervisor, SupervisorConfig};
use delega_mcp::BuildInfo;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower_mcp::{CallToolResult, TestClient};

mod applied;
mod resources;
mod stale;
mod tools;

/// Start `command` while holding delega's spawn lock, for the spawn call only
/// (#107). The supervisors of these tests start providers in this process, and
/// on macOS a child forked while another spawn's pipes still lack close-on-exec
/// inherits them. The lock covers the pipes std creates during this call; it is
/// dropped before the caller waits or reads output. The streams are left as
/// the caller set them.
#[allow(clippy::disallowed_methods)]
fn guarded_spawn(command: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    let _held = delega_core::process::spawn_guard();
    command.spawn()
}

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
    /// The directory the workers' `CLAUDE_CONFIG_DIR` points at.
    pub config_dir: PathBuf,
    pub session: String,
    pub supervisor: Supervisor,
    pub client: TestClient,
}

impl Rig {
    pub async fn new(provider: Provider) -> Self {
        Self::with_limits(provider, Limits::default()).await
    }

    pub async fn with_limits(provider: Provider, limits: Limits) -> Self {
        Self::build(
            provider,
            limits,
            PartialRequest::default(),
            None,
            Vec::new(),
        )
        .await
    }

    /// A rig whose server was started as `delega mcp` starts with a
    /// configuration file: `defaults` under every `start`, `config` as the
    /// file they came from, and `env_set` as extra variables for the workers.
    pub async fn with_config(
        provider: Provider,
        defaults: PartialRequest,
        config: AppliedConfig,
        env_set: Vec<(String, String)>,
    ) -> Self {
        Self::build(provider, Limits::default(), defaults, Some(config), env_set).await
    }

    async fn build(
        provider: Provider,
        limits: Limits,
        defaults: PartialRequest,
        applied_config: Option<AppliedConfig>,
        extra_env: Vec<(String, String)>,
    ) -> Self {
        let slot = SLOTS.take();
        let tmp = tempfile::tempdir().expect("create temporary directory");
        let root = std::fs::canonicalize(tmp.path()).expect("resolve temporary directory");
        let make = |name: &str| {
            let path = root.join(name);
            std::fs::create_dir(&path).expect("create directory");
            path
        };
        let fakes = make("fakes");
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fakes");
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
            system_paths: delega_core::provider::managed::SystemPaths::under(root.join("system")),
            executable: Some(fakes.join(name)),
            parent_env: Some(vec![
                (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
                (OsString::from("HOME"), home.clone().into_os_string()),
            ]),
            env_set: [
                ("FAKE_RECORD_DIR".into(), text(&record)),
                ("FAKE_SESSION_ID".into(), session.clone()),
                ("XDG_CONFIG_HOME".into(), text(&config)),
                ("CLAUDE_CONFIG_DIR".into(), text(&config)),
                ("CODEX_HOME".into(), text(&config)),
            ]
            .into_iter()
            .chain(extra_env)
            .collect(),
            stop_grace: Some(Duration::from_millis(200)),
            ..ProviderOptions::default()
        };
        let supervisor = Supervisor::new(SupervisorConfig {
            limits,
            provider: options,
            ..SupervisorConfig::default()
        });
        let router = delega_mcp::router_with_config(
            supervisor.clone(),
            &BuildInfo::default(),
            defaults,
            applied_config,
        )
        .expect("the router for the supervisor");
        let mut client = TestClient::from_router(router);
        client.initialize().await;
        Self {
            _slot: slot,
            _tmp: tmp,
            root,
            work,
            record,
            config_dir: config,
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
        // The streams `output` defaulted to: no input, both outputs captured.
        let out = guarded_spawn(
            std::process::Command::new("/bin/ps")
                .args(["-axww", "-o", "pid=,command="])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped()),
        )
        .and_then(std::process::Child::wait_with_output)
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

/// Whether every object, at any depth, has its keys in alphabetical order. The
/// order of a parsed value is the order of the text because `tower-mcp` turns on
/// `serde_json`'s `preserve_order`; the server sorts so that the document is the
/// one an unconfigured `serde_json` writes.
pub(crate) fn keys_sorted(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            let keys: Vec<&String> = map.keys().collect();
            keys.windows(2).all(|w| w[0] < w[1]) && map.values().all(keys_sorted)
        }
        Value::Array(items) => items.iter().all(keys_sorted),
        _ => true,
    }
}
