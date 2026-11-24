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
use crate::spawn::Guarded;

/// Prefix of every case marker. `--sweep` looks for it.
pub const MARKER_PREFIX: &str = "accmk-";

/// The marker of case `n` of run `run_id`. It ends in `/`, the separator that
/// follows it in every path it appears in, so no marker is contained in
/// another (`accmk-7-1/` is not in `accmk-7-10/`).
pub fn case_marker(run_id: &str, n: u64) -> String {
    format!("{MARKER_PREFIX}{run_id}-{n}/")
}

/// A case that has not ended this long after it started is a hang.
const HARD_LIMIT: Duration = Duration::from_secs(30);
/// How long after `delega` exits the harness waits before it measures what is
/// left, so a process that is merely slow to die is not counted.
const SETTLE: Duration = Duration::from_millis(1000);
/// How much longer the harness waits, after a SIGKILL of `delega`, for the
/// keeper to stop the tree: the default stop grace (1.5 s), the kill, and
/// room for a loaded machine.
const KEEPER_SETTLE: Duration = Duration::from_millis(5000);

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
    redirect_system_root: bool,
}

impl Context {
    /// What every case marker of this run starts with.
    pub fn run_marker(&self) -> String {
        format!("{MARKER_PREFIX}{}-", self.run_id)
    }

    pub fn new(delega: PathBuf, base: PathBuf, redirect_system_root: bool) -> Self {
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
            redirect_system_root,
        }
    }
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
    pub provider_pid: Option<i32>,
    pub survivors_at_exit: Vec<String>,
    pub survivors: Vec<String>,
    /// Consistency of the cleanup verdict where the matrix gates on it.
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
    /// Whether the documented survivor roles were all observed.
    #[serde(default)]
    pub gap_shown: Option<bool>,
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
        .guarded_status()
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
        for d in [&dir, &dir.join("pids"), &work, &home, &record] {
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
        if ctx.redirect_system_root {
            command.env("DELEGA_TEST_SYSTEM_ROOT", self.dir.join("system"));
        }
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

fn heartbeat(dir: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(dir.join("beat"))
        .and_then(|m| m.modified())
        .ok()
}

fn heartbeat_fresh(dir: &Path) -> bool {
    heartbeat(dir)
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
        gap_shown: None,
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

    // Timed from when the spawn lock is held, so waiting for another job's
    // spawn is not counted as part of this case.
    let (mut child, started): (Child, Instant) =
        match fixture.command(ctx, case).guarded_spawn_timed() {
            Ok(spawned) => spawned,
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
                Action::KillWithKeeper(_) => {
                    for keeper in procs::keepers_of(delega_pid) {
                        procs::signal(keeper, Signal::KILL);
                    }
                    procs::signal(delega_pid, Signal::KILL);
                }
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
        (Action::Signal(..) | Action::KillWithKeeper(_), _, Some(at)) => {
            m.latency_kind = "signal to exit".to_string();
            m.latency_ms = Some(ms(exited - at));
        }
        _ => {
            m.latency_kind = "run to exit".to_string();
            m.latency_ms = Some(ms(exited - started));
        }
    }
    m.provider_pid = procs::ledger(&fixture.dir)
        .into_iter()
        .find(|(role, _)| role == "provider")
        .map(|(_, pid)| pid);
    // Survivors, measured by marker and heartbeat, whatever delega said.
    let except = [delega_pid];
    let beat_at_exit = heartbeat(&fixture.dir);
    let at_exit = procs::survivors_sampled(&fixture.marker, &fixture.dir, &except, 3);
    let mut labels = procs::roles(&at_exit);
    if heartbeat(&fixture.dir) > beat_at_exit {
        labels.push("relay-heartbeat".to_string());
        labels.sort();
        labels.dedup();
    }
    m.survivors_at_exit = labels;
    std::thread::sleep(SETTLE);
    // After a SIGKILL of delega its keeper stops the tree by itself, TERM,
    // then KILL after the grace, which is longer than the settle time.
    if matches!(action, Action::Signal(Sig::Kill, _)) {
        let until = Instant::now() + KEEPER_SETTLE;
        while Instant::now() < until
            && (!procs::survivors_sampled(&fixture.marker, &fixture.dir, &except, 1).is_empty()
                || heartbeat_fresh(&fixture.dir))
        {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let beat_before_settled = heartbeat(&fixture.dir);
    let settled = procs::survivors_sampled(&fixture.marker, &fixture.dir, &except, 4);
    let mut labels = procs::roles(&settled);
    if heartbeat(&fixture.dir) > beat_before_settled {
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
    if let Ok(mut all) = ctx.bare.lock() {
        all.extend(procs::bare_pids_in(&fixture.dir));
    }
    if !left.is_empty() {
        m.problems.push(format!(
            "the harness could not kill: {}",
            procs::roles(&left).join(",")
        ));
    }

    let stdout_bytes = stdout_reader
        .map(|h| h.join().unwrap_or_default())
        .unwrap_or_default();
    let stderr_bytes = stderr_reader
        .map(|h| h.join().unwrap_or_default())
        .unwrap_or_default();

    m.stderr_first = String::from_utf8_lossy(&stderr_bytes)
        .lines()
        .next()
        .map(|l| l.chars().take(200).collect());

    read_stdout(&mut m, &stdout_bytes, case.expect.docs);

    judge(&mut m, case, std::env::consts::OS);
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
                    m.cleanup_reason = Some(reason.clone());
                }
            }
        }
        None if docs == Docs::None && bytes.is_empty() => {}
        None => m.result_valid = Some(false),
    }
}

/// Check the result and cleanup guarantees independently of the gap observation.
fn judge(m: &mut Measured, case: &Case, os: &str) {
    let documented_residual = case.expect.leak == LeakClass::DocumentedGap;
    if m.hang {
        m.problems
            .push("delega did not exit within 30 s".to_string());
    }
    let exit = m
        .exit_code
        .map(Exit::Code)
        .or(m.exit_signal.map(Exit::Signal));
    if exit != Some(case.expect.exit) {
        m.problems.push(format!(
            "expected exit {}, got {}",
            case.expect.exit.label(),
            exit.map_or_else(|| "no exit status".to_string(), Exit::label),
        ));
    }
    let no_survivors = m.survivors_at_exit.is_empty() && m.survivors.is_empty();
    match case.expect.docs {
        Docs::One => {
            if m.stdout_docs != Some(1) || m.result_valid != Some(true) {
                m.problems
                    .push("standard output must hold exactly one valid RunResult".to_string());
            }
            if m.outcome.as_deref() != Some(case.expect.outcome) {
                m.problems.push(format!(
                    "expected outcome {}, got {}",
                    case.expect.outcome,
                    m.outcome.as_deref().unwrap_or("no outcome"),
                ));
            }
            if m.cleanup_confirmed == Some(false)
                && !m
                    .cleanup_reason
                    .as_deref()
                    .is_some_and(documented_cleanup_reason)
            {
                m.problems.push(format!(
                    "undocumented unconfirmed cleanup reason: {:?}",
                    m.cleanup_reason
                ));
            }
            match case.expect.confirmed {
                Some(false) => {
                    if m.cleanup_confirmed != Some(false) {
                        m.problems
                            .push("expected Unconfirmed with process listing disabled".into());
                    }
                    if m.survivors_at_exit
                        .iter()
                        .chain(&m.survivors)
                        .any(|role| !case.expect.survivors.contains(&role.as_str()))
                    {
                        m.problems.push(format!(
                            "unexpected survivors with process listing disabled (at exit: {:?}, after settling: {:?})",
                            m.survivors_at_exit, m.survivors
                        ));
                    }
                }
                Some(true)
                    if !documented_residual
                        && m.cleanup_confirmed != Some(true)
                        && !(m.cleanup_confirmed == Some(false)
                            && late_listing_with_allowed_survivors(m, os)) =>
                {
                    m.problems.push(
                        "expected Confirmed or a late listing alone with only allowed survivors"
                            .into(),
                    );
                }
                _ => {}
            }
        }
        Docs::None | Docs::Unread => {
            // The caller killed delega or made its result unreadable. Check the
            // observable termination and the cleanup the case can require.
            if case.expect.docs == Docs::None
                && (m.stdout_docs != Some(0) || m.result_valid.is_some())
            {
                m.problems.push("a killed delega wrote output".to_string());
            }
            if !documented_residual && !m.survivors.is_empty() {
                m.problems.push(format!(
                    "survivors after delega exited: {}",
                    m.survivors.join(", ")
                ));
            }
        }
    }

    m.honest = m
        .cleanup_confirmed
        .filter(|_| !documented_residual)
        .map(|confirmed| !confirmed || no_survivors);
    if m.honest == Some(false) {
        m.problems.push(format!(
            "cleanup was Confirmed with survivors (at exit: {:?}, after settling: {:?})",
            m.survivors_at_exit, m.survivors
        ));
    }
    m.gap_shown = documented_residual.then(|| {
        case.expect
            .survivors
            .iter()
            .all(|role| m.survivors.iter().any(|observed| observed == role))
    });
    m.verdict = if m.problems.is_empty() {
        "pass"
    } else {
        "fail"
    }
    .to_string();
}

fn late_listing_with_allowed_survivors(m: &Measured, os: &str) -> bool {
    // A late listing can miss an orphan in its own session on macOS.
    m.survivors_at_exit
        .iter()
        .chain(&m.survivors)
        .all(|role| os == "macos" && matches!(role.as_str(), "leaf-setsid" | "bare"))
        && m.cleanup_reason.as_ref().is_some_and(|reason| {
            delega_core::process::is_late_listing_only(&Cleanup::Unconfirmed {
                reason: reason.clone(),
                pids: m.cleanup_pids.clone(),
                process_groups: m.cleanup_groups.clone(),
            })
        })
}

/// Cleanup can combine the probe and provider reasons with "; ". Match
/// complete static reasons first because some of them contain that separator.
fn documented_cleanup_reason(mut reason: &str) -> bool {
    use delega_core::process::{
        FREEZE_INCOMPLETE_REASON, KEEPER_LOST_REASON, KEEPER_UNANSWERED_REASON,
        KEEPER_UNPROVEN_REASON, LATE_LISTING_REASON, NO_REAP_ACK_REASON, NO_SUBREAPER_REASON,
        PIPES_HELD_REASON, TREE_LEFT_AT_REAP_REASON, UNVERIFIED_SIGNAL_REASON,
    };
    let fixed = [
        FREEZE_INCOMPLETE_REASON,
        KEEPER_LOST_REASON,
        KEEPER_UNANSWERED_REASON,
        KEEPER_UNPROVEN_REASON,
        LATE_LISTING_REASON,
        NO_REAP_ACK_REASON,
        NO_SUBREAPER_REASON,
        PIPES_HELD_REASON,
        UNVERIFIED_SIGNAL_REASON,
    ];
    loop {
        let tail = fixed
            .iter()
            .find_map(|known| reason.strip_prefix(known))
            .or_else(|| {
                let (count, rest) = reason.split_once(' ')?;
                count.parse::<u32>().ok()?;
                rest.strip_prefix(TREE_LEFT_AT_REAP_REASON)
            })
            .or_else(|| {
                let rest = reason.strip_prefix("process inspection was unavailable (")?;
                let (_, rest) = rest.split_once(
                    "); only the child's process group was checked with kill(-pgid, 0): ",
                )?;
                let (_, rest) = rest.split_once(". Descendants outside")?;
                let tail = rest.strip_prefix(" the group and session could not be checked.")?;
                Some(tail.strip_prefix(" Descendants seen at the latest scan are listed because they cannot be checked.").unwrap_or(tail))
            });
        match tail {
            Some("") => return true,
            Some(rest) => match rest.strip_prefix("; ") {
                Some(next) => reason = next,
                None => return false,
            },
            None => {
                return reason.strip_prefix("stopping was not confirmed within ")
                    .and_then(|text| text.split_once(": "))
                    .is_some_and(|(wait, detail)| !wait.is_empty() && !detail.is_empty())
                    || reason.split_once("; a best-effort SIGKILL was sent to the process group but no check confirmed it").is_some_and(|(why, _)| !why.is_empty())
                    || matches!(reason, "the run task panicked; the provider process, if one started, was sent an interrupt but nothing confirmed it stopped" | "the runtime shut down before the run finished; the provider process, if one started, was sent an interrupt but nothing confirmed it stopped");
            }
        }
    }
}

#[cfg(test)]
mod judge_tests {
    use super::*;

    use crate::matrix::{Machine, all_for};

    fn fixture_run(os: &'static str, id: &str) -> (Case, Measured) {
        let case = all_for(Machine {
            os,
            hides_system_env: os == "macos",
        })
        .into_iter()
        .find(|r| r.id == id)
        .expect("the matrix row");
        let mut measured: Measured = serde_json::from_str(
            r#"{"id":"claude-a-relay","repeat":1,"provider":"claude","case":"a",
            "variant":"relay","behaviour":"success","action":"","exit_code":0,
            "hang":false,"stdout_docs":1,"result_valid":true,
            "cleanup_confirmed":true,"cleanup_pids":[],"cleanup_groups":[],
            "survivors_at_exit":[],"survivors":[],"latency_kind":"",
            "expected_outcome":"completed","expected_exit":{"Code":0},
            "expected_leak":"documented_gap","expected_survivors":["relay-heartbeat"],
            "verdict":"","problems":[]}"#,
        )
        .expect("measurement");
        measured.outcome = Some(case.expect.outcome.into());
        if let Exit::Code(code) = case.expect.exit {
            measured.exit_code = Some(code);
        }
        (case, measured)
    }

    fn relay_run(os: &'static str) -> (Case, Measured) {
        fixture_run(os, "claude-a-relay")
    }

    #[test]
    fn a_measured_gap_label_cannot_excuse_a_linux_relay_survivor() {
        for at_exit in [true, false] {
            // The sample claims to be a gap. The Linux matrix row decides.
            let (case, mut m) = relay_run("linux");
            let survivors = if at_exit {
                &mut m.survivors_at_exit
            } else {
                &mut m.survivors
            };
            survivors.push("relay-heartbeat".into());
            judge(&mut m, &case, "linux");
            assert_eq!(m.gap_shown, None);
            assert_eq!(m.honest, Some(false));
            assert_eq!(m.verdict, "fail");
        }
    }

    #[test]
    fn a_documented_gap_can_show_or_disappear_with_either_cleanup_verdict() {
        for confirmed in [true, false] {
            for gap_shown in [true, false] {
                let (case, mut m) = relay_run("macos");
                m.cleanup_confirmed = Some(confirmed);
                m.cleanup_reason =
                    (!confirmed).then(|| delega_core::process::LATE_LISTING_REASON.into());
                if gap_shown {
                    m.survivors_at_exit.push("relay-heartbeat".into());
                    m.survivors.push("relay-heartbeat".into());
                }
                judge(&mut m, &case, "macos");
                assert_eq!(m.gap_shown, Some(gap_shown));
                assert_eq!(m.honest, None);
                assert_eq!(m.verdict, "pass");
            }
        }
    }

    #[test]
    fn a_gap_row_requires_one_valid_result() {
        for (docs, valid) in [(0, false), (2, true), (1, false)] {
            let (case, mut m) = relay_run("macos");
            m.stdout_docs = Some(docs);
            m.result_valid = Some(valid);
            judge(&mut m, &case, "macos");
            assert_eq!(m.verdict, "fail");
        }
    }

    #[test]
    fn a_valid_result_with_the_wrong_outcome_fails_on_both_platforms() {
        for (os, id, outcome) in [
            ("macos", "claude-d-plain", "failed"),
            ("macos", "codex-e-plain", "completed"),
            ("macos", "claude-d-relay", "failed"),
            ("linux", "claude-d-relay", "failed"),
        ] {
            let (case, mut m) = fixture_run(os, id);
            m.outcome = Some(outcome.into());
            judge(&mut m, &case, os);
            assert_eq!(m.verdict, "fail", "{id} on {os}");
            assert!(m.problems[0].starts_with("expected outcome "));
        }
    }

    #[test]
    fn a_timed_out_result_with_a_success_exit_fails_even_on_a_residual() {
        for id in ["codex-c-plain", "codex-c-relay"] {
            let (case, mut m) = fixture_run("macos", id);
            assert_eq!(m.outcome.as_deref(), Some("timed_out"));
            m.exit_code = Some(0);
            judge(&mut m, &case, "macos");
            assert_eq!(m.verdict, "fail");
            assert_eq!(m.problems, ["expected exit 124, got 0"]);
        }
    }

    #[test]
    fn a_late_listing_on_linux_is_allowed_only_when_nothing_survived() {
        let (case, mut m) = relay_run("linux");
        m.cleanup_confirmed = Some(false);
        m.cleanup_reason = Some(delega_core::process::LATE_LISTING_REASON.into());
        judge(&mut m, &case, "linux");
        assert_eq!(m.verdict, "pass");

        for at_exit in [true, false] {
            let mut m = m.clone();
            let survivors = if at_exit {
                &mut m.survivors_at_exit
            } else {
                &mut m.survivors
            };
            survivors.push("relay-heartbeat".into());
            judge(&mut m, &case, "linux");
            assert_eq!(m.verdict, "fail");
        }
    }

    #[test]
    fn a_late_listing_allows_only_detached_survivors_on_macos() {
        for (os, role, expected) in [
            ("macos", "leaf-setsid", "pass"),
            ("macos", "bare", "pass"),
            ("macos", "child", "fail"),
            ("macos", "child-ignore", "fail"),
            ("macos", "relay-heartbeat", "fail"),
            ("linux", "leaf-setsid", "fail"),
        ] {
            for at_exit in [true, false] {
                let (case, mut m) = fixture_run(os, "claude-d-setsid");
                m.cleanup_confirmed = Some(false);
                m.cleanup_reason = Some(delega_core::process::LATE_LISTING_REASON.into());
                let survivors = if at_exit {
                    &mut m.survivors_at_exit
                } else {
                    &mut m.survivors
                };
                survivors.push(role.into());
                judge(&mut m, &case, os);
                assert_eq!(m.verdict, expected, "{os}: {role}, at_exit={at_exit}");
            }
        }
    }

    #[test]
    fn another_documented_reason_cannot_replace_expected_confirmation() {
        let (case, mut m) = relay_run("linux");
        m.cleanup_confirmed = Some(false);
        m.cleanup_reason = Some(delega_core::process::PIPES_HELD_REASON.into());
        judge(&mut m, &case, "linux");
        assert_eq!(m.verdict, "fail");
        assert_eq!(
            m.problems,
            ["expected Confirmed or a late listing alone with only allowed survivors"]
        );
    }

    #[test]
    fn a_result_cannot_confirm_cleanup_with_listing_disabled() {
        for os in ["linux", "macos"] {
            let (case, mut m) = fixture_run(os, "claude-u-setsid");
            judge(&mut m, &case, os);
            assert_eq!(m.verdict, "fail");
            assert_eq!(
                m.problems,
                ["expected Unconfirmed with process listing disabled"]
            );
        }
    }

    #[test]
    fn linux_keeper_must_clean_up_even_with_listing_disabled() {
        for (id, role) in [
            ("claude-u-setsid", "leaf-setsid"),
            ("claude-u-orphan", "leaf-setsid"),
            ("claude-u-bare", "bare"),
        ] {
            let (case, mut m) = fixture_run("linux", id);
            m.cleanup_confirmed = Some(false);
            m.cleanup_reason = Some(delega_core::process::PIPES_HELD_REASON.into());
            m.survivors.push(role.into());
            judge(&mut m, &case, "linux");
            assert_eq!(m.verdict, "fail");
            assert!(
                m.problems[0].starts_with("unexpected survivors with process listing disabled")
            );
        }
    }

    #[test]
    fn unconfirmed_cleanup_rejects_unknown_reasons_and_accepts_combined_reasons() {
        use delega_core::process::{LATE_LISTING_REASON, PIPES_HELD_REASON};

        let (case, mut m) = relay_run("macos");
        m.cleanup_confirmed = Some(false);
        m.cleanup_reason = Some("could not be checked".into());
        judge(&mut m, &case, "macos");
        assert_eq!(m.verdict, "fail");
        assert!(documented_cleanup_reason(&format!(
            "{LATE_LISTING_REASON}; {PIPES_HELD_REASON}"
        )));
        assert!(!documented_cleanup_reason(&format!(
            "{LATE_LISTING_REASON}; unknown reason"
        )));
    }
}
