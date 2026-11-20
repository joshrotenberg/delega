//! Running one case against the built `delega` binary and measuring it.
//!
//! Each case gets a directory that holds copies of the fake providers and
//! their helper scripts. The directory name is the case marker: every process
//! the fakes start has a script path in it, so it shows in the process table.
//! The harness gives `delega` a cleared environment (a temporary `HOME`, the
//! fakes as the provider executables, nothing else but `PATH`) so no real
//! provider state, credential or configuration is read.

use std::io::Read;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use delega_core::contract::result::{Cleanup, RunResult};
use rustix::process::Signal;
use serde::{Deserialize, Serialize};

use crate::matrix::{Action, Docs, Exit, LeakClass, Row as Case, Sig, needs_perl, roles_of};
use crate::procs;

/// Prefix of every case marker. `--sweep` looks for it.
pub const MARKER_PREFIX: &str = "accmk-";

/// The marker of case `n` of run `run_id`. It ends in `/`, the separator that
/// follows it in every path it appears in, so no marker is contained in
/// another (`accmk-7-1/` is not in `accmk-7-10/`).
pub fn case_marker(run_id: &str, n: u64) -> String {
    format!("{MARKER_PREFIX}{run_id}-{n}/")
}

/// The verdict of a known-gap row on a run where no survivor remained.
pub const GAP_NOT_SHOWN: &str = "gap_not_shown";
/// The verdict of a known-gap row that showed no gap in every repeat of a run.
pub const GAP_CLOSED: &str = "gap_closed";

/// A case that has not ended this long after it started is a hang.
const HARD_LIMIT: Duration = Duration::from_secs(30);
/// How long after `delega` exits the harness waits before it measures what is
/// left, so a process that is merely slow to die is not counted.
const SETTLE: Duration = Duration::from_millis(1000);

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Settings shared by every case of a run.
pub struct Context {
    /// Pids of `bare` processes any case recorded, so the end of the run can
    /// look for them after the case directories (and their ledgers) are gone.
    pub bare: std::sync::Mutex<Vec<i32>>,
    pub delega: PathBuf,
    pub fakes: PathBuf,
    pub run_id: String,
    pub base: PathBuf,
}

impl Context {
    /// What every case marker of this run starts with.
    pub fn run_marker(&self) -> String {
        format!("{MARKER_PREFIX}{}-", self.run_id)
    }

    pub fn new(delega: PathBuf, base: PathBuf) -> Self {
        let run_id = format!(
            "{}{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs() % 100_000)
        );
        Self {
            bare: std::sync::Mutex::new(Vec::new()),
            delega,
            fakes: crate::delega_bin::workspace_root()
                .join("crates/delega-core/src/provider/tests/fakes"),
            run_id,
            base,
        }
    }
}

fn one() -> u32 {
    1
}

/// What was measured for one case, plus what was expected and the verdict.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Measured {
    pub id: String,
    pub repeat: u32,
    pub provider: String,
    pub case: String,
    pub variant: String,
    pub behaviour: String,
    pub action: String,
    pub timeout_secs: Option<u64>,
    pub exit_code: Option<i32>,
    pub exit_signal: Option<i32>,
    pub hang: bool,
    pub stdout_docs: Option<usize>,
    pub result_valid: Option<bool>,
    pub outcome: Option<String>,
    pub error_kind: Option<String>,
    /// The message of the result's error, clipped, so a failed row says why.
    #[serde(default)]
    pub error_message: Option<String>,
    pub cleanup_confirmed: Option<bool>,
    pub cleanup_reason: Option<String>,
    pub cleanup_pids: Vec<u32>,
    pub cleanup_groups: Vec<u32>,
    /// The cleanup verdict was `Unconfirmed` for the late process listing and
    /// for nothing else (`delega_core::process::is_late_listing_only`).
    #[serde(default)]
    pub cleanup_late_listing_only: bool,
    /// Runs of the row, the last one measured. More than one only when earlier
    /// runs failed on a late listing alone.
    #[serde(default = "one")]
    pub attempts: u32,
    /// The cleanup reasons of the runs that were discarded, in order.
    #[serde(default)]
    pub discarded: Vec<String>,
    pub provider_pid: Option<i32>,
    pub survivors_at_exit: Vec<String>,
    pub survivors: Vec<String>,
    pub honest: Option<bool>,
    /// Milliseconds from the action to the exit; for a time limit the
    /// overshoot past `--timeout`; for a run left to finish, the whole run.
    pub latency_ms: Option<u64>,
    pub latency_kind: String,
    /// Milliseconds from start to exit.
    pub elapsed_ms: Option<u64>,
    pub expected_outcome: String,
    pub expected_exit: Exit,
    pub expected_confirmed: Option<bool>,
    pub expected_survivors: Vec<String>,
    pub expected_leak: LeakClass,
    /// The row's gap depends on timing, so a run without a survivor is
    /// `gap_not_shown` and not a failure (`Expect::timing_dependent`).
    #[serde(default)]
    pub timing_dependent: bool,
    pub expected_reason: Option<String>,
    pub verdict: String,
    pub problems: Vec<String>,
    /// First line of standard error, for diagnosis.
    pub stderr_first: Option<String>,
}

pub fn perl_available() -> bool {
    Command::new("perl")
        .arg("-e")
        .arg("1")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

struct Fixture {
    _root: tempfile::TempDir,
    marker: String,
    dir: PathBuf,
    work: PathBuf,
    home: PathBuf,
    record: PathBuf,
}

impl Fixture {
    fn new(ctx: &Context) -> std::io::Result<Self> {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = tempfile::Builder::new()
            .prefix("dacc-")
            .tempdir_in(&ctx.base)?;
        let top = std::fs::canonicalize(root.path())?;
        let marker = case_marker(&ctx.run_id, n);
        let dir = top.join(marker.trim_end_matches('/'));
        let work = top.join("work");
        let home = top.join("home");
        let record = top.join("record");
        for d in [&dir, &work, &home, &record] {
            std::fs::create_dir(d)?;
        }
        for name in [
            "fake-claude.sh",
            "fake-codex.sh",
            "acc-lib.sh",
            "acc-child.sh",
        ] {
            let target = dir.join(name);
            // A link, not a copy. A file this process writes cannot be executed
            // on Linux while any process holds it open for writing, and a
            // `fork` on another harness thread inherits the descriptor of a
            // copy being written (`ETXTBSY`, "text file busy"). A link is
            // never written: the target is the checked-in script. `$0` is the
            // link, so the case directory is still what the scripts and the
            // process table see.
            std::os::unix::fs::symlink(ctx.fakes.join(name), &target)?;
        }
        Ok(Self {
            _root: root,
            marker,
            dir,
            work,
            home,
            record,
        })
    }

    fn command(&self, ctx: &Context, case: &Case) -> Command {
        let mut command = Command::new(&ctx.delega);
        command.args([
            "run",
            "--output",
            "json",
            "--provider",
            case.provider,
            "-p",
            &format!("acc:{}:{}", case.case.behaviour, case.variant),
            "-C",
        ]);
        command.arg(&self.work);
        // Workers do not inherit the caller's environment; the fakes write
        // their records to the directory this variable names.
        command.args(["--env", "FAKE_RECORD_DIR"]);
        if let Some(secs) = case.case.timeout_secs {
            command.args(["--timeout", &format!("{secs}s")]);
        }
        let config = self.home.join("config");
        let _ = std::fs::create_dir(&config);
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &config)
            .env("CLAUDE_CONFIG_DIR", &config)
            .env("CODEX_HOME", &config)
            .env("DELEGA_CLAUDE_BIN", self.dir.join("fake-claude.sh"))
            .env("DELEGA_CODEX_BIN", self.dir.join("fake-codex.sh"))
            .env("FAKE_RECORD_DIR", &self.record)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Its own process group: a signal the harness's terminal sends
            // never reaches it, and the harness never signals its own group.
            .process_group(0);
        if case.case.ps_unavailable {
            // Test-only seam: no place to look for `ps`.
            command.env("DELEGA_TEST_NO_PROCESS_LISTING", "");
            // Tell the fake too, so it does not wait for a listing that never
            // comes (acc-lib.sh, acc_after_listing).
            command.env("ACC_NO_LISTING", "1");
            command.args(["--env", "ACC_NO_LISTING"]);
        }
        command
    }
}

fn drain(mut pipe: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = std::io::Read::by_ref(&mut pipe)
            .take(8 * 1024 * 1024)
            .read_to_end(&mut bytes);
        // Keep draining so the writer is never blocked by a full pipe.
        let mut sink = [0u8; 8192];
        while pipe.read(&mut sink).is_ok_and(|n| n > 0) {}
        bytes
    })
}

fn ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

fn signal_of(sig: Sig) -> Signal {
    match sig {
        Sig::Int => Signal::INT,
        Sig::Term => Signal::TERM,
        Sig::Hup => Signal::HUP,
        Sig::Kill => Signal::KILL,
    }
}

/// Whether the fake has recorded every role the variant starts.
/// Every behaviour but `exit3` also creates the file `ready` once its opening output is
/// written, so a SIGKILL of `delega` cannot arrive before the provider's first
/// write and end it with SIGPIPE.
fn ready(dir: &Path, case: &Case) -> bool {
    let roles = procs::ledger(dir);
    let have = |r: &str| roles.iter().any(|(role, _)| role == r);
    have("provider")
        && (case.case.behaviour == "exit3" || dir.join("ready").exists())
        && roles_of(case.variant).iter().all(|r| have(r))
}

fn heartbeat_fresh(dir: &Path) -> bool {
    std::fs::metadata(dir.join("beat"))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < Duration::from_millis(300))
}

/// Run one case and return what was measured. Never leaves a process behind.
pub fn run_case(ctx: &Context, case: &Case, repeat: u32) -> Measured {
    let mut m = Measured {
        id: case.id.clone(),
        repeat,
        provider: case.provider.to_string(),
        case: case.case.id.to_string(),
        variant: case.variant.to_string(),
        behaviour: case.case.behaviour.to_string(),
        action: case.case.action.label(),
        timeout_secs: case.case.timeout_secs,
        exit_code: None,
        exit_signal: None,
        hang: false,
        stdout_docs: None,
        result_valid: None,
        outcome: None,
        error_kind: None,
        error_message: None,
        cleanup_confirmed: None,
        cleanup_reason: None,
        cleanup_pids: Vec::new(),
        cleanup_groups: Vec::new(),
        cleanup_late_listing_only: false,
        attempts: 1,
        discarded: Vec::new(),
        provider_pid: None,
        survivors_at_exit: Vec::new(),
        survivors: Vec::new(),
        honest: None,
        latency_ms: None,
        latency_kind: String::new(),
        elapsed_ms: None,
        expected_outcome: case.expect.outcome.to_string(),
        expected_exit: case.expect.exit,
        expected_confirmed: case.expect.confirmed,
        expected_survivors: case
            .expect
            .survivors
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        expected_leak: case.expect.leak,
        timing_dependent: case.expect.timing_dependent,
        expected_reason: case.expect.reason.map(str::to_string),
        verdict: String::new(),
        problems: Vec::new(),
        stderr_first: None,
    };

    if needs_perl(case.variant) && !perl_available() {
        m.verdict = "skipped".to_string();
        m.problems
            .push("perl is not available, so descendants cannot be started".to_string());
        return m;
    }
    let fixture = match Fixture::new(ctx) {
        Ok(f) => f,
        Err(err) => {
            m.verdict = "fail".to_string();
            m.problems
                .push(format!("could not set up the case directory: {err}"));
            return m;
        }
    };

    let started = Instant::now();
    let mut child: Child = match fixture.command(ctx, case).spawn() {
        Ok(c) => c,
        Err(err) => {
            m.verdict = "fail".to_string();
            m.problems.push(format!("could not start delega: {err}"));
            return m;
        }
    };
    let delega_pid = i32::try_from(child.id()).unwrap_or(0);
    let mut stdout = child.stdout.take();
    let stderr_reader = child.stderr.take().map(drain);
    let stdout_reader = if matches!(case.case.action, Action::CloseStdout(_)) {
        None
    } else {
        stdout.take().map(drain)
    };

    let action = case.case.action;
    let mut acted_at: Option<Instant> = None;
    let mut status: Option<ExitStatus> = None;
    loop {
        match child.try_wait() {
            Ok(Some(s)) => {
                status = Some(s);
                break;
            }
            Ok(None) => {}
            Err(_) => break,
        }
        let elapsed = started.elapsed();
        if acted_at.is_none()
            && let Some(at) = action.at_ms()
            && elapsed >= Duration::from_millis(at)
            && (ready(&fixture.dir, case) || elapsed >= Duration::from_secs(5))
        {
            match action {
                Action::Signal(sig, _) => procs::signal(delega_pid, signal_of(sig)),
                Action::CloseStdout(_) => drop(stdout.take()),
                Action::Wait => {}
            }
            acted_at = Some(Instant::now());
        }
        if elapsed > HARD_LIMIT {
            m.hang = true;
            procs::signal(delega_pid, Signal::KILL);
            let _ = child.wait();
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let exited = Instant::now();
    drop(stdout);
    let stdout_bytes = stdout_reader
        .map(|h| h.join().unwrap_or_default())
        .unwrap_or_default();
    let stderr_bytes = stderr_reader
        .map(|h| h.join().unwrap_or_default())
        .unwrap_or_default();

    if let Some(s) = status {
        m.exit_code = s.code();
        m.exit_signal = s.signal();
    }
    m.elapsed_ms = Some(ms(exited - started));
    let timed_out = m.exit_code == Some(124);
    match (action, case.case.timeout_secs, acted_at) {
        (Action::Wait | Action::CloseStdout(_), Some(t), _) if timed_out => {
            m.latency_kind = "timeout overshoot".to_string();
            m.latency_ms = Some(ms(exited - started).saturating_sub(t * 1000));
        }
        (Action::Signal(..), _, Some(at)) => {
            m.latency_kind = "signal to exit".to_string();
            m.latency_ms = Some(ms(exited - at));
        }
        _ => {
            m.latency_kind = "run to exit".to_string();
            m.latency_ms = Some(ms(exited - started));
        }
    }
    m.stderr_first = String::from_utf8_lossy(&stderr_bytes)
        .lines()
        .next()
        .map(|l| l.chars().take(200).collect());

    read_stdout(&mut m, &stdout_bytes, case.expect.docs);

    if let Ok(mut all) = ctx.bare.lock() {
        all.extend(procs::bare_pids_in(&fixture.dir));
    }
    m.provider_pid = procs::ledger(&fixture.dir)
        .into_iter()
        .find(|(role, _)| role == "provider")
        .map(|(_, pid)| pid);
    // Survivors, measured by marker and heartbeat, whatever delega said.
    let except = [delega_pid];
    let at_exit = procs::survivors_sampled(&fixture.marker, &fixture.dir, &except, 3);
    let mut labels = procs::roles(&at_exit);
    if heartbeat_fresh(&fixture.dir) {
        labels.push("relay-heartbeat".to_string());
        labels.sort();
        labels.dedup();
    }
    m.survivors_at_exit = labels;
    std::thread::sleep(SETTLE);
    let settled = procs::survivors_sampled(&fixture.marker, &fixture.dir, &except, 4);
    let mut labels = procs::roles(&settled);
    if heartbeat_fresh(&fixture.dir) {
        labels.push("relay-heartbeat".to_string());
        labels.sort();
        labels.dedup();
    }
    m.survivors = labels;
    // The relay stops at the file `stop`; everything else is killed.
    let _ = std::fs::write(fixture.dir.join("stop"), b"");
    procs::kill_all(&at_exit);
    procs::kill_all(&settled);
    let left = procs::sweep(
        &fixture.marker,
        &fixture.dir,
        &except,
        Duration::from_secs(10),
    );
    if !left.is_empty() {
        m.problems.push(format!(
            "the harness could not kill: {}",
            procs::roles(&left).join(",")
        ));
    }

    m.honest = m
        .cleanup_confirmed
        .map(|confirmed| m.survivors.is_empty() || !confirmed);
    judge(&mut m, case);
    m
}

/// Read standard output: how many JSON documents, and whether the one is a
/// valid `RunResult`.
fn read_stdout(m: &mut Measured, bytes: &[u8], docs: Docs) {
    if docs == Docs::Unread {
        return;
    }
    let mut count = 0;
    let mut broken = false;
    let mut first: Option<serde_json::Value> = None;
    for item in serde_json::Deserializer::from_slice(bytes).into_iter::<serde_json::Value>() {
        match item {
            Ok(v) => {
                count += 1;
                first.get_or_insert(v);
            }
            Err(_) => {
                broken = true;
                break;
            }
        }
    }
    m.stdout_docs = Some(count);
    let parsed = first
        .filter(|_| count == 1 && !broken)
        .and_then(|v| serde_json::from_value::<RunResult>(v).ok());
    match parsed {
        Some(result) => {
            m.result_valid = Some(result.validate().is_ok());
            m.outcome = Some(
                serde_json::to_value(result.outcome)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default(),
            );
            m.error_kind = result.error.as_ref().map(|e| e.kind.clone());
            m.error_message = result
                .error
                .as_ref()
                .map(|e| e.message.chars().take(500).collect());
            match &result.cleanup {
                Cleanup::Confirmed => m.cleanup_confirmed = Some(true),
                Cleanup::Unconfirmed {
                    reason,
                    pids,
                    process_groups,
                } => {
                    m.cleanup_confirmed = Some(false);
                    m.cleanup_pids.clone_from(pids);
                    m.cleanup_groups.clone_from(process_groups);
                    m.cleanup_reason = Some(reason.chars().take(300).collect());
                    m.cleanup_late_listing_only =
                        delega_core::process::is_late_listing_only(&result.cleanup);
                }
            }
        }
        None => m.result_valid = Some(false),
    }
}

/// ` (error_kind: message)` for a result that carries an error, else nothing.
fn error_suffix(m: &Measured) -> String {
    match (&m.error_kind, &m.error_message) {
        (Some(kind), Some(message)) => format!(" (error {kind}: {message})"),
        (Some(kind), None) => format!(" (error {kind})"),
        _ => String::new(),
    }
}

/// The problem `judge` records when the cleanup verdict differs from the
/// expectation. [`crate::run_all`] builds the same text with this function to
/// recognise a run whose only fault is a late listing, so the wording is in
/// one place.
pub fn cleanup_mismatch(actual: Option<bool>, expected: Option<bool>) -> String {
    format!("cleanup.confirmed is {actual:?} where {expected:?} was expected")
}

/// Compare the measurement with the expectation and set the verdict.
///
/// Anything but criterion 2 that differs from the expectation is a `fail`: the
/// outcome, the exit status, the document on standard output, the cleanup
/// verdict (`confirmed`, and the reason when it is not) and a hang. Criterion 2
/// is judged on the survivors. Most rows list none, and any survivor fails
/// them. A known-gap row says a survivor of the listed roles may remain: all
/// of them present is `expected_failure` (the gap showed). None present is
/// `gap_not_shown` when the gap depends on timing (relay, orphan, bare: whether
/// a late process is caught varies, and a gap shown on one run and not on the
/// next is not a regression) and `unexpected_pass` when it does not (SIGKILL of
/// `delega`, after which the worker always survives), which fails.
/// The run does not fail on `gap_not_shown`; [`crate::run_all::close_gaps`]
/// turns a row that did not show its gap in every repeat of a run into
/// `gap_closed`. Anything else (a survivor of another role, a subset of the
/// listed roles, an unrecorded survivor) fails.
fn judge(m: &mut Measured, case: &Case) {
    let expect = &case.expect;
    let mut problems = std::mem::take(&mut m.problems);
    if m.hang {
        problems.push("delega did not exit within 30 s".to_string());
    }
    match m
        .exit_code
        .map(Exit::Code)
        .or(m.exit_signal.map(Exit::Signal))
    {
        Some(actual) if actual == expect.exit => {}
        Some(actual) => problems.push(format!(
            "exit status {} where {} was expected",
            actual.label(),
            expect.exit.label()
        )),
        None => problems.push("delega has no exit status".to_string()),
    }
    match expect.docs {
        Docs::One => {
            if m.stdout_docs != Some(1) {
                problems.push(format!(
                    "standard output held {} JSON documents, expected one",
                    m.stdout_docs.map_or("?".to_string(), |n| n.to_string())
                ));
            } else if m.result_valid != Some(true) {
                problems.push("the document is not a valid RunResult".to_string());
            } else {
                if m.outcome.as_deref() != Some(expect.outcome) {
                    problems.push(format!(
                        "outcome {} where {} was expected{}",
                        m.outcome.as_deref().unwrap_or("none"),
                        expect.outcome,
                        error_suffix(m)
                    ));
                }
                if m.cleanup_confirmed != expect.confirmed {
                    problems.push(cleanup_mismatch(m.cleanup_confirmed, expect.confirmed));
                }
                if expect.confirmed == Some(false) {
                    check_unconfirmed(m, expect.reason, &mut problems);
                }
            }
        }
        Docs::None => {
            if m.stdout_docs != Some(0) {
                problems.push("a killed delega wrote output".to_string());
            }
        }
        Docs::Unread => {}
    }

    let mut actual: Vec<&str> = m.survivors.iter().map(String::as_str).collect();
    actual.sort_unstable();
    let wanted: Vec<&str> = m.expected_survivors.iter().map(String::as_str).collect();
    let mut wanted_sorted = wanted.clone();
    wanted_sorted.sort_unstable();
    // Survivors alongside a report of confirmed cleanup, or no report at all.
    let unreported = !actual.is_empty() && m.cleanup_confirmed != Some(false);

    if !problems.is_empty() {
        m.verdict = "fail".to_string();
        if actual != wanted_sorted {
            problems.push(format!(
                "survivors {} where {} was expected",
                list(&actual),
                list(&wanted_sorted)
            ));
        }
        m.problems = problems;
        return;
    }
    let gap = m.expected_leak != LeakClass::None;
    if actual == wanted_sorted {
        if !gap {
            if unreported {
                m.verdict = "fail".to_string();
                m.problems.push(format!(
                    "survivors {} with cleanup reported confirmed",
                    list(&actual)
                ));
            } else {
                m.verdict = "pass".to_string();
            }
        } else if unreported {
            m.verdict = "expected_failure".to_string();
            m.problems.push(format!(
                "survivors {}; cleanup not reported as unconfirmed",
                list(&actual)
            ));
        } else {
            m.verdict = "unexpected_pass".to_string();
            m.problems
                .push("a row expected to fail criterion 2 passed; correct the table".to_string());
        }
    } else if gap && actual.is_empty() && expect.timing_dependent {
        m.verdict = GAP_NOT_SHOWN.to_string();
        m.problems.push(format!(
            "no survivor where {} was allowed: the gap did not show in this run",
            list(&wanted_sorted)
        ));
    } else if gap && actual.is_empty() {
        m.verdict = "unexpected_pass".to_string();
        m.problems.push(format!(
            "no survivor where {} is certain: this gap does not depend on timing, so the row no longer shows it; correct the table or find the regression",
            list(&wanted_sorted)
        ));
    } else {
        m.verdict = "fail".to_string();
        m.problems.push(format!(
            "survivors {} where {} was expected",
            list(&actual),
            list(&wanted_sorted)
        ));
    }
}

fn list(roles: &[&str]) -> String {
    if roles.is_empty() {
        "none".to_string()
    } else {
        roles.join(",")
    }
}

/// An unconfirmed cleanup must say why, and say what could not be checked.
/// With no process listing it cannot name the processes that remain, so
/// `pids` and `process_groups` list only what is known to be alive (nothing,
/// when the worker's group is gone); the reason carries the caveat.
fn check_unconfirmed(m: &Measured, reason: Option<&str>, problems: &mut Vec<String>) {
    let text = m.cleanup_reason.as_deref().unwrap_or("");
    if let Some(wanted) = reason
        && !text.contains(wanted)
    {
        problems.push(format!(
            "cleanup.reason does not contain {wanted:?}: {text:?}"
        ));
    }
    if !text.contains("could not be checked") {
        problems.push(format!(
            "cleanup.reason does not say what could not be checked: {text:?}"
        ));
    }
}

#[cfg(test)]
mod judge_tests {
    use super::*;

    fn relay_row() -> Case {
        crate::matrix::all()
            .into_iter()
            .find(|r| r.id == "claude-a-relay")
            .expect("the relay row")
    }

    /// A measurement of `row` that met everything but the survivors.
    fn measured(row: &Case, survivors: &[&str]) -> Measured {
        let mut m: Measured = serde_json::from_str(
            r#"{"id":"x","repeat":1,"provider":"claude","case":"a","variant":"relay",
            "behaviour":"success","action":"","timeout_secs":null,"exit_code":0,
            "exit_signal":null,"hang":false,"stdout_docs":1,"result_valid":true,
            "outcome":"completed","error_kind":null,"cleanup_confirmed":true,
            "cleanup_reason":null,"cleanup_pids":[],"cleanup_groups":[],
            "provider_pid":null,"survivors_at_exit":[],"survivors":[],"honest":true,
            "latency_ms":null,"latency_kind":"","elapsed_ms":null,
            "expected_outcome":"completed","expected_exit":{"Code":0},
            "expected_confirmed":true,"expected_survivors":[],
            "expected_leak":"none","expected_reason":null,
            "verdict":"","problems":[],"stderr_first":null}"#,
        )
        .expect("a measurement");
        m.expected_leak = row.expect.leak;
        m.timing_dependent = row.expect.timing_dependent;
        m.expected_survivors = row
            .expect
            .survivors
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        m.survivors = survivors.iter().map(|s| (*s).to_owned()).collect();
        judge(&mut m, row);
        m
    }

    #[test]
    fn a_known_gap_row_with_its_survivor_is_an_expected_failure() {
        let m = measured(&relay_row(), &["relay-heartbeat"]);
        assert_eq!(m.verdict, "expected_failure", "{:?}", m.problems);
    }

    #[test]
    fn a_known_gap_row_without_its_survivor_is_gap_not_shown() {
        let m = measured(&relay_row(), &[]);
        assert_eq!(m.verdict, GAP_NOT_SHOWN, "{:?}", m.problems);
    }

    fn sigkill_row() -> Case {
        crate::matrix::all()
            .into_iter()
            .find(|r| r.id == "claude-k-plain")
            .expect("the SIGKILL row")
    }

    /// The SIGKILL rows measured with `delega` killed: no output, signal 9.
    fn killed(row: &Case, survivors: &[&str]) -> Measured {
        let mut m = measured(row, survivors);
        m.exit_code = None;
        m.exit_signal = Some(9);
        m.stdout_docs = Some(0);
        m.problems.clear();
        m.survivors = survivors.iter().map(|s| (*s).to_owned()).collect();
        judge(&mut m, row);
        m
    }

    #[test]
    fn a_gap_that_does_not_depend_on_timing_is_strict() {
        let row = sigkill_row();
        assert!(!row.expect.timing_dependent);
        assert!(relay_row().expect.timing_dependent);
        assert_eq!(killed(&row, &["provider"]).verdict, "expected_failure");
        // No survivor after SIGKILL is not load noise: the worker cannot have gone.
        assert_eq!(killed(&row, &[]).verdict, "unexpected_pass");
        assert_eq!(killed(&row, &["child"]).verdict, "fail");
    }

    #[test]
    fn an_unrecorded_survivor_still_fails_a_known_gap_row() {
        let m = measured(&relay_row(), &["relay-heartbeat", "unrecorded"]);
        assert_eq!(m.verdict, "fail");
        let m = measured(&relay_row(), &["unrecorded"]);
        assert_eq!(m.verdict, "fail");
    }

    #[test]
    fn a_survivor_of_another_role_still_fails_a_known_gap_row() {
        let m = measured(&relay_row(), &["child"]);
        assert_eq!(m.verdict, "fail");
        let m = measured(&relay_row(), &["relay-heartbeat", "child"]);
        assert_eq!(m.verdict, "fail");
    }

    #[test]
    fn a_wrong_exit_status_fails_a_known_gap_row_whether_or_not_the_gap_shows() {
        let row = relay_row();
        let mut m = measured(&row, &[]);
        m.exit_code = Some(1);
        m.problems.clear();
        judge(&mut m, &row);
        assert_eq!(m.verdict, "fail");
    }

    #[test]
    fn a_row_with_no_gap_fails_on_any_survivor_and_passes_on_none() {
        let row = crate::matrix::all()
            .into_iter()
            .find(|r| r.id == "claude-a-child")
            .expect("a strict row");
        assert_eq!(measured(&row, &[]).verdict, "pass");
        assert_eq!(measured(&row, &["child"]).verdict, "fail");
        assert_eq!(measured(&row, &["relay-heartbeat"]).verdict, "fail");
    }
}
