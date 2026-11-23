//! Tests for the supervisor through its public API, against the fake provider
//! executables.
//!
//! They are a test target of their own, so `cargo test` runs them apart from
//! the library's unit tests: they start many processes and poll them, and beside
//! the timing-sensitive tests of the process layer they made those fail on a busy
//! machine.
//!
//! Each test copies the fakes into a directory of its own, so the path of that
//! directory is in the command line of every process the test starts. The
//! test ends by listing the process table and failing if any process still
//! carries it. Every test runs with a temporary home and a parent environment
//! it builds itself, so none reads or writes real provider state.
//!
//! A run is held open by a gate: the prompt `gate:<file>` makes the fake write
//! `<file>.started`, then wait until `<file>` exists. That makes the order of
//! events a test depends on a fact about files, not about timing.

#![cfg(unix)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Duration;

use tempfile::TempDir;

use delega_core::contract::request::{Provider, Request};
use delega_core::contract::result::RunResult;
use delega_core::provider::ProviderOptions;

use delega_core::supervisor::{
    Detail, Limits, ReadResponse, ResultView, RunState, Supervisor, SupervisorConfig, Target,
    WaitResponse, WaitTargets,
};

mod asks;
mod drops;
mod lifecycle;
mod limits;
mod reads;
mod state;
mod stops;
mod stress;
mod usage;
mod waits;

/// How many of these tests may run at once. Each starts shell processes and
/// polls them, so on a busy machine running them all in parallel with the
/// timing-sensitive tests of the process layer makes those tests fail.
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

/// A private area per test.
pub(crate) struct Rig {
    _slot: Slot,
    _tmp: TempDir,
    root: PathBuf,
    work: PathBuf,
    record: PathBuf,
    home: PathBuf,
    config: PathBuf,
    fakes: PathBuf,
    session: String,
}

/// A run held open until the test lets it go.
pub(crate) struct Gate(PathBuf);

/// `output` while holding the process layer's spawn lock for the spawn (#104).
/// This binary runs supervisors in the same process; an unguarded `ps` could
/// hand a gated provider its output pipe, or take a provider's standard input.
fn guarded_output(command: &mut std::process::Command) -> std::io::Result<std::process::Output> {
    let child = {
        let _held = delega_core::process::spawn_guard();
        #[allow(clippy::disallowed_methods)]
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
    }?;
    child.wait_with_output()
}

impl Gate {
    /// The prompt that holds a run on this gate.
    pub fn prompt(&self) -> String {
        format!("gate:{}", self.0.display())
    }

    /// Let the run finish.
    pub fn open(&self) {
        std::fs::write(&self.0, b"").expect("open the gate");
    }

    fn started_marker(&self) -> PathBuf {
        let mut name = self.0.clone().into_os_string();
        name.push(".started");
        PathBuf::from(name)
    }

    /// Whether the fake has begun the run held on this gate.
    pub fn has_started(&self) -> bool {
        self.started_marker().exists()
    }

    /// Wait until the fake has begun the run held on this gate. The deadline
    /// is generous: on a loaded machine or a shared CI runner, ten workers'
    /// version probes and starts can take well over the 15 s this used to allow.
    pub async fn started(&self) {
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        while std::time::Instant::now() < deadline {
            if self.has_started() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the run held on {} never started", self.0.display());
    }

    /// Assert that the fake has not begun the run held on this gate. This is
    /// only a snapshot; what proves the run waited is its state, see
    /// [`assert_state`].
    pub fn assert_not_started(&self) {
        assert!(
            !self.has_started(),
            "the run held on {} started too early",
            self.0.display()
        );
    }
}

impl Rig {
    pub fn new() -> Self {
        let slot = SLOTS.take();
        let tmp = tempfile::tempdir().expect("create temporary directory");
        let root = std::fs::canonicalize(tmp.path()).expect("resolve temporary directory");
        let make = |name: &str| {
            let path = root.join(name);
            std::fs::create_dir(&path).expect("create directory");
            path
        };
        let fakes = make("fakes");
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/provider/tests/fakes");
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
        Self {
            _slot: slot,
            work: make("work"),
            record: make("record"),
            home: make("home"),
            config: make("config"),
            fakes,
            root,
            session,
            _tmp: tmp,
        }
    }

    /// The test's private directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The session id the fakes report when they are not resuming one.
    pub fn session(&self) -> &str {
        &self.session
    }

    pub fn gate(&self, name: &str) -> Gate {
        Gate(self.root.join(format!("gate-{name}")))
    }

    /// Options that run the fake for `provider` with a controlled environment.
    pub fn options(&self, provider: Provider) -> ProviderOptions {
        let name = match provider {
            Provider::Claude => "fake-claude.sh",
            Provider::Codex => "fake-codex.sh",
        };
        let text = |p: &PathBuf| p.to_string_lossy().into_owned();
        ProviderOptions {
            system_paths: delega_core::provider::managed::SystemPaths::under(
                self.work.join("system"),
            ),
            executable: Some(self.fakes.join(name)),
            parent_env: Some(vec![
                (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
                (OsString::from("HOME"), self.home.clone().into_os_string()),
            ]),
            env_set: vec![
                ("FAKE_RECORD_DIR".into(), text(&self.record)),
                ("FAKE_SESSION_ID".into(), self.session.clone()),
                ("XDG_CONFIG_HOME".into(), text(&self.config)),
                ("CLAUDE_CONFIG_DIR".into(), text(&self.config)),
                ("CODEX_HOME".into(), text(&self.config)),
            ],
            stop_grace: Some(Duration::from_millis(200)),
            ..ProviderOptions::default()
        }
    }

    pub fn config(&self, provider: Provider) -> SupervisorConfig {
        SupervisorConfig {
            provider: self.options(provider),
            ..SupervisorConfig::default()
        }
    }

    pub fn config_with(&self, provider: Provider, limits: Limits) -> SupervisorConfig {
        SupervisorConfig {
            limits,
            ..self.config(provider)
        }
    }

    pub fn supervisor(&self, provider: Provider) -> Supervisor {
        Supervisor::new(self.config(provider))
    }

    pub fn request(&self, provider: Provider, prompt: &str) -> Request {
        let mut request = Request::new(provider, prompt);
        request.cwd = Some(self.work.clone());
        request
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

    /// Processes whose command line carries this rig's directory.
    fn survivors(&self) -> Vec<String> {
        let out = guarded_output(std::process::Command::new("/bin/ps").args([
            "-axww",
            "-o",
            "pid=,command=",
        ]))
        .expect("run ps");
        let marker = self.root.to_string_lossy().into_owned();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| l.contains(&marker))
            .map(|l| l.trim().to_string())
            .collect()
    }

    /// The same check for a test that is not async: poll for up to thirty
    /// seconds until no process carries this rig's directory.
    pub fn settle_blocking(&self) {
        let mut left = Vec::new();
        for _ in 0..1500 {
            left = self.survivors();
            if left.is_empty() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("processes are still running: {left:#?}");
    }

    /// Fail if a process started for this test is still alive after sixty
    /// seconds. Each poll runs `ps`, which is slow on a loaded machine, so the
    /// limit is a deadline, not a count of polls.
    pub async fn assert_nothing_left(&self) {
        let mut left = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while std::time::Instant::now() < deadline {
            left = self.survivors();
            if left.is_empty() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("processes are still running: {left:#?}");
    }
}

/// Assert a cleanup verdict after a stop is honest. On a loaded machine the
/// process layer's listing before the first signal can miss its budget, and it
/// then says so instead of confirming; that is correct behaviour. What must hold
/// is that the verdict is either confirmed or unconfirmed with a stated reason of
/// that kind. Whether anything is really left is measured separately with
/// [`Rig::assert_nothing_left`].
pub(crate) fn assert_honest_cleanup(cleanup: &delega_core::contract::result::Cleanup) {
    use delega_core::contract::result::Cleanup;
    match cleanup {
        Cleanup::Confirmed => {}
        Cleanup::Unconfirmed { reason, .. } => assert!(
            reason.contains("process listing")
                || reason.contains("process inspection was unavailable")
                || reason.contains("stopping was not confirmed"),
            "an unconfirmed cleanup must give the missed-listing reason, got: {reason}"
        ),
    }
}

/// Assert the cleanup verdict of a run that ended on its own. It is
/// `Confirmed`, or exactly the late-listing verdict: after a natural exit the
/// process layer waits a bounded time for the listing that precedes its first
/// signal, and on a slow machine or a shared CI runner that listing can miss
/// the budget, which the layer reports instead of confirming. No pids, no
/// process groups and no other reason are accepted. Whether anything is really
/// left is measured separately with [`Rig::assert_nothing_left`].
pub(crate) fn assert_exit_cleanup(cleanup: &delega_core::contract::result::Cleanup) {
    use delega_core::contract::result::Cleanup;
    assert!(
        matches!(cleanup, Cleanup::Confirmed)
            || delega_core::process::is_late_listing_only(cleanup),
        "a run that ended on its own must be confirmed or late-listing only, got: {cleanup:?}"
    );
}

/// Assert the state the supervisor reports for a run. A run that has to wait is
/// proved to wait by this, not by waiting out a clock.
pub(crate) async fn assert_state(supervisor: &Supervisor, run: &str, state: RunState) {
    let read = read_compact(supervisor, run).await;
    assert_eq!(read.state, Some(state), "{read:#?}");
}

/// Wait for the runs to finish; the wait must not time out.
pub(crate) async fn finished(supervisor: &Supervisor, runs: &[&str]) -> WaitResponse {
    let targets = WaitTargets {
        runs: runs.iter().map(ToString::to_string).collect(),
        workers: Vec::new(),
    };
    let response = supervisor
        .wait(targets, Some(60), None)
        .await
        .expect("wait answers");
    assert!(!response.timed_out, "{response:#?}");
    response
}

/// Wait for one run to finish and return its whole result, checked.
pub(crate) async fn result_of(supervisor: &Supervisor, run: &str) -> RunResult {
    // A wait may time out, since a test can set a short wait ceiling and a
    // loaded machine can be slow; it is repeated until the run is finished.
    let targets = WaitTargets {
        runs: vec![run.to_string()],
        workers: Vec::new(),
    };
    let mut done = false;
    for _ in 0..240 {
        let waited = supervisor
            .wait(targets.clone(), Some(30), None)
            .await
            .expect("wait answers");
        if waited.runs.iter().all(|r| r.outcome.is_some()) {
            done = true;
            break;
        }
    }
    assert!(done, "run {run} did not finish in time");
    let read = read_full(supervisor, run).await;
    let Some(ResultView::Full(result)) = read.result else {
        panic!("run {run} has no full result: {read:#?}");
    };
    assert_eq!(result.validate(), Ok(()), "{result:#?}");
    *result
}

pub(crate) async fn read_full(supervisor: &Supervisor, run: &str) -> ReadResponse {
    supervisor
        .read(Target::Run(run.to_string()), Detail::Full, None, None)
        .await
        .expect("read answers")
}

pub(crate) async fn read_compact(supervisor: &Supervisor, run: &str) -> ReadResponse {
    supervisor
        .read(Target::Run(run.to_string()), Detail::Compact, None, None)
        .await
        .expect("read answers")
}

/// Poll `read` until the run has begun writing, so partial text exists.
pub(crate) async fn read_until_text(supervisor: &Supervisor, run: &str) -> ReadResponse {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while std::time::Instant::now() < deadline {
        let read = read_compact(supervisor, run).await;
        if read
            .progress
            .as_ref()
            .is_some_and(|p| !p.partial_text.is_empty())
        {
            return read;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("run {run} never showed partial text");
}

/// A request for each provider.
pub(crate) const BOTH: [Provider; 2] = [Provider::Claude, Provider::Codex];
