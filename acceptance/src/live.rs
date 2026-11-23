//! The live suite: a few cases per provider against the installed `claude`
//! and `codex`, through the built `delega` binary. See the "Live suite"
//! section of `acceptance/README.md`.
//!
//! Nothing here runs unless `delega-acceptance --live` is given. Each case
//! gets a fresh directory under the harness's temporary root, removed when the
//! case ends. Every `delega` it starts has a time limit and a harness bound
//! past it, the reads of its output are bounded, and each case has a bound of
//! its own, so the suite always ends. What it prints and records is names,
//! outcomes, counts, version numbers and the test words it seeded itself:
//! never a model's answer, the content of another file, a provider's raw
//! version line or an environment value.
//!
//! A case ends `pass`, `fail` or `not_shown`. `not_shown` means nothing that
//! was checked failed, but a check the case exists for could not be shown on
//! this run (the network was unreachable from the harness itself, the shell
//! tool was never seen before the interrupt, and so on). It is not a pass.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use delega_core::contract::result::{Cleanup, RunResult};
use rustix::process::Signal;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::procs;
use crate::spawn::Guarded;

/// The providers, in the order the suite runs them.
pub const PROVIDERS: [&str; 2] = ["claude", "codex"];

/// The model every Claude case asks for.
pub const CLAUDE_MODEL: &str = "haiku";
/// The effort every Codex case asks for.
pub const CODEX_EFFORT: &str = "low";
/// `--timeout` of every run but the timeout case.
pub const RUN_TIMEOUT_SECS: u64 = 120;
/// `--timeout` of the timeout case.
pub const SHORT_TIMEOUT_SECS: u64 = 10;
/// How long past `delega`'s own time limit the harness waits before it sends
/// SIGTERM. It covers delega's cleanup deadline (10 s) and the writing of the
/// result.
const GRACE: Duration = Duration::from_secs(45);
/// How long after SIGTERM the harness waits before SIGKILL.
const KILL_AFTER: Duration = Duration::from_secs(15);
/// How long after SIGKILL the harness waits for the exit before it gives up.
const REAP_WAIT: Duration = Duration::from_secs(5);
/// After a Ctrl-C at the harness: how long after SIGINT to `delega` the
/// harness waits before it sends SIGTERM.
const STOP_WAIT: Duration = Duration::from_secs(20);
/// How long after `delega` exits the harness waits for its standard output
/// and standard error to close. A process `delega` started that still holds
/// one keeps it open.
const DRAIN: Duration = Duration::from_secs(5);
/// The bound on `delega doctor` and on a dry run.
const SHORT_BOUND: Duration = Duration::from_secs(60);
/// The bound on a whole case. The resume case's two runs fit in it with
/// every escalation.
pub const CASE_BOUND: Duration = Duration::from_secs(450);
/// The interrupt case sends SIGINT this long after the progress output names
/// the shell tool, so the command it started is running when the signal lands.
const AFTER_TOOL: Duration = Duration::from_secs(2);
/// The interrupt case sends SIGINT at this point if the shell tool was not
/// seen; the case then ends `not_shown`.
const INTERRUPT_FALLBACK: Duration = Duration::from_secs(30);
/// Prefix of every case directory.
pub const DIR_PREFIX: &str = "dlive-";
/// The address the build case's network probe tries.
pub const PROBE_URL: &str = "https://example.com";
/// The curl the build probe and the harness's own check use.
pub const CURL: &str = "/usr/bin/curl";

/// Set by the SIGINT and SIGTERM handler [`install_stop_handler`] installs.
pub static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_stop_signal(_: libc::c_int) {
    // Only an atomic store: async-signal-safe.
    STOP.store(true, Ordering::SeqCst);
}

/// Make SIGINT and SIGTERM to the harness set [`STOP`] instead of ending it,
/// so a maintainer's Ctrl-C stops the current `delega` through its documented
/// interrupt and the case directory is removed before the harness exits.
#[allow(unsafe_code)]
pub fn install_stop_handler() -> Result<(), String> {
    for sig in [libc::SIGINT, libc::SIGTERM] {
        // SAFETY: the handler only stores to a static atomic, which is
        // async-signal-safe, and it has the signature `signal` expects.
        let previous =
            unsafe { libc::signal(sig, on_stop_signal as *const () as libc::sighandler_t) };
        if previous == libc::SIG_ERR {
            return Err("could not install the Ctrl-C handler".to_string());
        }
    }
    Ok(())
}

/// One case of the suite.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Case {
    Doctor,
    Plain,
    ReadCwd,
    WriteRefused,
    ReadOutside,
    Build,
    Resume,
    Interrupt,
    Timeout,
}

impl Case {
    pub const ALL: [Case; 9] = [
        Case::Doctor,
        Case::Plain,
        Case::ReadCwd,
        Case::WriteRefused,
        Case::ReadOutside,
        Case::Build,
        Case::Resume,
        Case::Interrupt,
        Case::Timeout,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Case::Doctor => "doctor",
            Case::Plain => "plain-answer",
            Case::ReadCwd => "read-cwd",
            Case::WriteRefused => "write-refused",
            Case::ReadOutside => "read-outside",
            Case::Build => "build",
            Case::Resume => "resume",
            Case::Interrupt => "interrupt",
            Case::Timeout => "timeout",
        }
    }

    /// Model runs the case starts: two for the resume case, none for doctor.
    pub fn runs(self) -> u32 {
        match self {
            Case::Doctor => 0,
            Case::Resume => 2,
            _ => 1,
        }
    }
}

/// A case for one provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Planned {
    pub provider: &'static str,
    pub case: Case,
}

impl Planned {
    pub fn id(&self) -> String {
        format!("{}-{}", self.provider, self.case.name())
    }
}

/// The cases to run: every case for both providers, or every case but
/// `doctor` for the named one. `delega doctor` has no option to check one
/// provider and starts both executables' version probes, so it runs only
/// when both providers are selected.
pub fn plan(provider: Option<&str>) -> Result<Vec<Planned>, String> {
    let (providers, doctor): (Vec<&'static str>, bool) = match provider {
        None => (PROVIDERS.to_vec(), true),
        Some(name) => (
            vec![
                *PROVIDERS
                    .iter()
                    .find(|p| **p == name)
                    .ok_or_else(|| format!("unknown provider {name:?}; give claude or codex"))?,
            ],
            false,
        ),
    };
    Ok(providers
        .into_iter()
        .flat_map(|provider| Case::ALL.map(|case| Planned { provider, case }))
        .filter(|p| doctor || p.case != Case::Doctor)
        .collect())
}

/// What one case found. One JSON line of `--live --out`, after the metadata
/// line.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LiveRow {
    pub provider: String,
    pub case: String,
    /// `pass`, `fail` or `not_shown`.
    pub verdict: String,
    /// Model runs (`delega run` without `--dry-run`) the case made.
    pub runs: u32,
    /// The outcome of the last run.
    pub outcome: Option<String>,
    pub exit_code: Option<i32>,
    pub exit_signal: Option<i32>,
    /// `cleanup.confirmed` of the last run.
    pub cleanup_confirmed: Option<bool>,
    /// How many entries `permissions.denied` of the last run had.
    pub denied: usize,
    /// Whether the last run carried a `wider than requested` provider note.
    pub wider_note: bool,
    pub error_kind: Option<String>,
    /// The provider name and the version number parsed from its version
    /// line, such as `claude 2.1.330`. Never the line itself.
    pub provider_version: Option<String>,
    pub elapsed_ms: u64,
    /// The sum of `usage.cost_usd` over the case's runs that reported one.
    pub cost_usd: Option<f64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    /// What was asserted and held.
    pub checks: Vec<String>,
    /// What was observed and not asserted.
    pub notes: Vec<String>,
    /// What the case is for and could not be shown on this run.
    #[serde(default)]
    pub not_shown: Vec<String>,
    /// What was asserted and did not hold.
    pub problems: Vec<String>,
}

/// The environment `delega` gets.
pub enum Env {
    /// The harness's own environment (the real `HOME`, `PATH` and provider
    /// logins), without any `DELEGA_TEST_*` variable. What `--live` uses.
    Inherit,
    /// Exactly these variables. For tests against the fake providers.
    Fixed(Vec<(OsString, OsString)>),
}

/// Settings shared by the cases of one suite run.
pub struct Live {
    pub delega: PathBuf,
    /// Where case directories are made.
    pub root: PathBuf,
    pub env: Env,
    /// Random; distinguishes this run's test words from another run's.
    pub run_id: String,
    /// The stop request: [`STOP`] for `--live`, another flag in tests.
    pub stop: &'static AtomicBool,
    /// The end of the current case's bound.
    deadline: std::cell::Cell<Option<Instant>>,
}

/// The temporary root for case directories: on macOS the per-user temporary
/// directory (`getconf DARWIN_USER_TEMP_DIR`, under `/var/folders`), elsewhere
/// `XDG_RUNTIME_DIR` when it is set and a directory, else the system temporary
/// directory. Outside `/tmp` where the system has a place for it, so the build
/// case's directory outside the sandbox is outside every location the
/// providers' sandboxes let commands write (see `acceptance/README.md`).
pub fn temp_root() -> Result<PathBuf, String> {
    let mut found: Option<PathBuf> = None;
    if cfg!(target_os = "macos") {
        found = Command::new("/usr/bin/getconf")
            .arg("DARWIN_USER_TEMP_DIR")
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .guarded_output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
            .filter(|p| p.is_absolute() && p.is_dir());
    } else if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)
        && dir.is_absolute()
        && dir.is_dir()
    {
        found = Some(dir);
    }
    let root = found.unwrap_or_else(std::env::temp_dir);
    std::fs::canonicalize(&root).map_err(|e| format!("temporary directory: {e}"))
}

/// Whether `path` lies under `/tmp` (or macOS's `/private/tmp`), which Codex's
/// `build` sandbox lets commands write.
pub fn under_slash_tmp(path: &Path) -> bool {
    path.starts_with("/tmp") || path.starts_with("/private/tmp")
}

/// `bytes` random bytes from `/dev/urandom`, as lowercase hex.
pub fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    let read = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut buf));
    if read.is_err() {
        // Not expected on macOS or Linux; fall back to the clock and the pid.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let seed = nanos ^ u128::from(std::process::id());
        for (i, b) in buf.iter_mut().enumerate() {
            *b = u8::try_from((seed >> ((i % 16) * 8)) & 0xff).unwrap_or(0);
        }
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether a path exists, without following a link or opening anything.
fn present(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// Read a small regular file without blocking: anything that is not a
/// regular file (a FIFO, a device, a link) reads as nothing, and the file is
/// opened non-blocking and without following a link, then checked again.
pub fn read_regular(path: &Path, limit: u64) -> Option<String> {
    if !std::fs::symlink_metadata(path).ok()?.file_type().is_file() {
        return None;
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)
        .ok()?;
    if !file.metadata().ok()?.file_type().is_file() {
        return None;
    }
    let mut text = String::new();
    file.take(limit).read_to_string(&mut text).ok()?;
    Some(text)
}

/// The provider name and the first version number in a version line, such as
/// `claude 2.1.330` from `2.1.330 (Claude Code)`. A number is digits with at
/// least one dot between digit runs. Nothing else from the line is kept.
pub fn version_token(provider: &str, raw: &str) -> Option<String> {
    raw.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .map(|t| t.trim_matches('.'))
        .find(|t| t.contains('.') && t.split('.').all(|part| !part.is_empty()))
        .map(|number| format!("{provider} {number}"))
}

impl Live {
    pub fn new(delega: PathBuf, root: PathBuf, env: Env) -> Self {
        Self {
            delega,
            root,
            env,
            run_id: random_hex(3),
            stop: &STOP,
            deadline: std::cell::Cell::new(None),
        }
    }

    /// The test word for a case: a fixed stem and this run's random suffix, so
    /// an answer from an earlier run or another case cannot match.
    pub fn word(&self, stem: &str) -> String {
        format!("{stem}{}", self.run_id)
    }

    fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// Run one case and return what it found. The case directory is removed
    /// before this returns.
    pub fn run(&self, planned: Planned) -> LiveRow {
        let began = Instant::now();
        self.deadline.set(Some(began + CASE_BOUND));
        let mut row = LiveRow {
            provider: planned.provider.to_string(),
            case: planned.case.name().to_string(),
            ..LiveRow::default()
        };
        if self.stop_requested() {
            row.problems
                .push("the maintainer stopped the suite before the case".to_string());
        } else {
            match CaseDir::new(&self.root) {
                Ok(dir) => self.run_in(&dir, planned, &mut row),
                Err(err) => row
                    .problems
                    .push(format!("could not set up the case directory: {err}")),
            }
        }
        if self.stop_requested() {
            row.problems
                .push("the maintainer stopped the suite (Ctrl-C)".to_string());
        }
        self.deadline.set(None);
        row.elapsed_ms = u64::try_from(began.elapsed().as_millis()).unwrap_or(u64::MAX);
        row.verdict = verdict(&row).to_string();
        row
    }

    fn run_in(&self, dir: &CaseDir, planned: Planned, row: &mut LiveRow) {
        let provider = planned.provider;
        match planned.case {
            Case::Doctor => {
                let ran = self.invoke(
                    dir,
                    &os(&["doctor", "--output", "json"]),
                    SHORT_BOUND,
                    false,
                );
                row.exit_code = ran.exit_code;
                row.exit_signal = ran.exit_signal;
                row.problems.extend(ran.problems.iter().cloned());
                if ran.started {
                    judge_doctor(row, &ran.stdout);
                }
            }
            Case::Plain => {
                if let Some(r) = self.model_run(dir, row, provider, "read", PLAIN_PROMPT) {
                    judge_plain(row, &r);
                }
            }
            Case::ReadCwd => {
                let word = self.word("lantern");
                if let Err(err) = std::fs::write(dir.work.join("word.txt"), format!("{word}\n")) {
                    row.problems.push(format!("could not seed word.txt: {err}"));
                    return;
                }
                if let Some(r) = self.model_run(dir, row, provider, "read", READ_CWD_PROMPT) {
                    judge_read_cwd(row, &r, &word);
                }
            }
            Case::WriteRefused => {
                let word = self.word("pebble");
                let seeded = format!("{word}\n");
                let note = dir.work.join("note.txt");
                if let Err(err) = std::fs::write(&note, &seeded) {
                    row.problems.push(format!("could not seed note.txt: {err}"));
                    return;
                }
                let no_write = self.plan_offers_no_write(dir, row, provider, WRITE_PROMPT);
                if let Some(r) = self.model_run(dir, row, provider, "read", WRITE_PROMPT) {
                    let unchanged = read_regular(&note, 4096).is_some_and(|t| t == seeded);
                    judge_write_refused(row, provider, &r, unchanged, no_write);
                }
            }
            Case::ReadOutside => {
                let word = self.word("thistle");
                let secret = dir.outside.join("secret.txt");
                if let Err(err) = std::fs::write(&secret, format!("{word}\n")) {
                    row.problems
                        .push(format!("could not seed the outside file: {err}"));
                    return;
                }
                let prompt = read_outside_prompt(&secret);
                if let Some(r) = self.model_run(dir, row, provider, "read", &prompt) {
                    judge_read_outside(row, provider, &r, &word, &secret);
                }
            }
            Case::Build => {
                let name = format!("{DIR_PREFIX}probe-{}", random_hex(8));
                if let Err(err) =
                    std::fs::write(dir.work.join("probe.sh"), probe_script(&name, &dir.outside))
                {
                    row.problems.push(format!("could not seed probe.sh: {err}"));
                    return;
                }
                let baseline = network_baseline();
                let result = self.model_run(dir, row, provider, "build", BUILD_PROMPT);
                let probe = BuildProbe::observe(dir, &name, baseline);
                if let Some(r) = result {
                    judge_build(row, provider, &r, &probe);
                }
                remove_probe_file(&dir.work, &name);
            }
            Case::Resume => {
                let word = self.word("harbor");
                let first_prompt =
                    format!("Remember the word {word}. Reply with only the word noted.");
                let args = run_args(
                    provider,
                    "read",
                    &first_prompt,
                    &dir.work,
                    RUN_TIMEOUT_SECS,
                    false,
                    None,
                    &[],
                );
                let Some(first) = self
                    .run_and_take(dir, row, &args, RUN_TIMEOUT_SECS, false)
                    .0
                else {
                    return;
                };
                let Some(sid) = first.session_id.clone() else {
                    row.problems
                        .push("the first run reported no session id to resume".to_string());
                    return;
                };
                let args = run_args(
                    provider,
                    "read",
                    RESUME_PROMPT,
                    &dir.work,
                    RUN_TIMEOUT_SECS,
                    false,
                    Some(&sid),
                    &[],
                );
                if let Some(second) = self
                    .run_and_take(dir, row, &args, RUN_TIMEOUT_SECS, false)
                    .0
                {
                    judge_resume(row, &first, &second, &word);
                }
            }
            Case::Interrupt => {
                let args = run_args(
                    provider,
                    "build",
                    SLEEP_PROMPT,
                    &dir.work,
                    RUN_TIMEOUT_SECS,
                    true,
                    None,
                    &["--progress"],
                );
                let (result, after_tool) =
                    self.run_and_take(dir, row, &args, RUN_TIMEOUT_SECS, true);
                if let Some(r) = result {
                    judge_interrupt(row, &r, after_tool);
                }
            }
            Case::Timeout => {
                let args = run_args(
                    provider,
                    "build",
                    SLEEP_PROMPT,
                    &dir.work,
                    SHORT_TIMEOUT_SECS,
                    true,
                    None,
                    &[],
                );
                if let Some(r) = self
                    .run_and_take(dir, row, &args, SHORT_TIMEOUT_SECS, false)
                    .0
                {
                    judge_timeout(row, &r);
                }
            }
        }
    }

    /// One ephemeral run with the default time limit.
    fn model_run(
        &self,
        dir: &CaseDir,
        row: &mut LiveRow,
        provider: &str,
        mode: &str,
        prompt: &str,
    ) -> Option<RunResult> {
        let args = run_args(
            provider,
            mode,
            prompt,
            &dir.work,
            RUN_TIMEOUT_SECS,
            true,
            None,
            &[],
        );
        self.run_and_take(dir, row, &args, RUN_TIMEOUT_SECS, false)
            .0
    }

    /// Start a model run, record it in the row and return its result, and for
    /// the interrupt case when the signal was sent.
    fn run_and_take(
        &self,
        dir: &CaseDir,
        row: &mut LiveRow,
        args: &[OsString],
        timeout_secs: u64,
        interrupt: bool,
    ) -> (Option<RunResult>, Option<bool>) {
        let ran = self.invoke(
            dir,
            args,
            Duration::from_secs(timeout_secs) + GRACE,
            interrupt,
        );
        if ran.started {
            row.runs += 1;
        }
        (take_result(row, &ran), ran.after_tool)
    }

    /// The launch plan of `delega run --dry-run` with the write case's
    /// arguments: whether it offers the worker no way to write. `None` when
    /// the plan could not be read.
    fn plan_offers_no_write(
        &self,
        dir: &CaseDir,
        row: &mut LiveRow,
        provider: &str,
        prompt: &str,
    ) -> Option<bool> {
        let args = run_args(
            provider,
            "read",
            prompt,
            &dir.work,
            RUN_TIMEOUT_SECS,
            true,
            None,
            &["--dry-run"],
        );
        let ran = self.invoke(dir, &args, SHORT_BOUND, false);
        let plan: Value = serde_json::from_slice(&ran.stdout).ok()?;
        let launch: Vec<String> = plan["dry_run"]["args"]
            .as_array()?
            .iter()
            .filter_map(|a| a.as_str().map(str::to_string))
            .collect();
        let offers_none = offers_no_write(provider, &launch);
        if offers_none.is_none() {
            row.notes
                .push("the dry run's launch arguments could not be read".to_string());
        }
        offers_none
    }

    /// Start `delega` with `args` in the case's working directory and wait
    /// for it. Past `bound` (or the case's bound, if that ends first) it gets
    /// SIGTERM, and SIGKILL [`KILL_AFTER`] later. On a stop request it gets
    /// SIGINT, its documented interrupt, then SIGTERM after [`STOP_WAIT`] and
    /// SIGKILL after [`KILL_AFTER`]. A SIGKILL is announced on standard error,
    /// because the provider may survive it. With `interrupt`, it gets SIGINT
    /// once the shell tool has been running for [`AFTER_TOOL`], or at
    /// [`INTERRUPT_FALLBACK`]. Its output is read for at most [`DRAIN`] after
    /// it exits.
    fn invoke(&self, dir: &CaseDir, args: &[OsString], bound: Duration, interrupt: bool) -> Ran {
        let mut ran = Ran::default();
        let remaining = self
            .deadline
            .get()
            .map_or(bound, |d| d.saturating_duration_since(Instant::now()));
        if remaining.is_zero() {
            ran.problems.push(format!(
                "the case's {} s bound was reached before delega could be started",
                CASE_BOUND.as_secs()
            ));
            return ran;
        }
        let bound = bound.min(remaining);
        let mut command = Command::new(&self.delega);
        command.args(args).current_dir(&dir.work);
        match &self.env {
            Env::Inherit => {
                for (name, _) in std::env::vars_os() {
                    if name.to_string_lossy().starts_with("DELEGA_TEST_") {
                        command.env_remove(name);
                    }
                }
            }
            Env::Fixed(vars) => {
                command.env_clear();
                for (name, value) in vars {
                    command.env(name, value);
                }
            }
        }
        command
            .env("TMPDIR", &dir.tmp)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Its own process group: a Ctrl-C at the harness's terminal does
            // not reach it, and a signal the harness sends reaches only it.
            .process_group(0);
        let mut child = match command.guarded_spawn() {
            Ok(c) => c,
            Err(err) => {
                ran.problems.push(format!("could not start delega: {err}"));
                return ran;
            }
        };
        ran.started = true;
        let started = Instant::now();
        let pid = i32::try_from(child.id()).unwrap_or(0);
        ran.pid = pid;
        let (done_tx, done_rx) = mpsc::channel::<()>();
        // Standard output is collected as it arrives, so what was written is
        // kept even if a descendant holds the pipe open.
        let out_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let mut readers = 0;
        if let Some(mut pipe) = child.stdout.take() {
            readers += 1;
            let buf = Arc::clone(&out_buf);
            let tx = done_tx.clone();
            std::thread::spawn(move || {
                let mut chunk = [0u8; 8192];
                while let Ok(n) = pipe.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    if let Ok(mut b) = buf.lock()
                        && b.len() < 8 * 1024 * 1024
                    {
                        b.extend_from_slice(&chunk[..n]);
                    }
                }
                let _ = tx.send(());
            });
        }
        // Standard error is read line by line and dropped. Only whether a
        // progress line names the shell tool is kept.
        let tool_seen: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
        if let Some(pipe) = child.stderr.take() {
            readers += 1;
            let seen = Arc::clone(&tool_seen);
            let tx = done_tx.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(pipe).lines() {
                    let Ok(line) = line else { break };
                    if names_shell_tool(&line)
                        && let Ok(mut at) = seen.lock()
                    {
                        at.get_or_insert_with(Instant::now);
                    }
                }
                let _ = tx.send(());
            });
        }
        drop(done_tx);

        let mut sent_stop_int: Option<Instant> = None;
        let mut sent_term: Option<Instant> = None;
        let mut sent_kill: Option<Instant> = None;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    ran.exit_code = status.code();
                    ran.exit_signal = status.signal();
                    break;
                }
                Ok(None) => {}
                Err(err) => {
                    ran.problems
                        .push(format!("could not wait for delega: {err}"));
                    break;
                }
            }
            if sent_kill.is_some_and(|at| at.elapsed() >= REAP_WAIT) {
                ran.problems.push(
                    "delega had not exited 5 s after SIGKILL; the harness stopped waiting"
                        .to_string(),
                );
                break;
            }
            let elapsed = started.elapsed();
            if interrupt && ran.after_tool.is_none() && sent_stop_int.is_none() {
                let seen = tool_seen.lock().ok().and_then(|at| *at);
                if seen.is_some_and(|at| at.elapsed() >= AFTER_TOOL) {
                    procs::signal(pid, Signal::INT);
                    ran.after_tool = Some(true);
                } else if elapsed >= INTERRUPT_FALLBACK {
                    procs::signal(pid, Signal::INT);
                    ran.after_tool = Some(false);
                }
            }
            if sent_stop_int.is_none() && sent_term.is_none() && self.stop_requested() {
                procs::signal(pid, Signal::INT);
                sent_stop_int = Some(Instant::now());
            }
            let stop_overdue = sent_stop_int.is_some_and(|at| at.elapsed() >= STOP_WAIT);
            if sent_term.is_none() && (elapsed >= bound || stop_overdue) {
                if elapsed >= bound {
                    ran.problems.push(format!(
                        "delega had not ended {} s after it started; the harness sent SIGTERM",
                        bound.as_secs()
                    ));
                } else {
                    ran.problems.push(
                        "delega had not stopped 20 s after the harness passed on Ctrl-C as SIGINT; the harness sent SIGTERM"
                            .to_string(),
                    );
                }
                procs::signal(pid, Signal::TERM);
                sent_term = Some(Instant::now());
            }
            if sent_kill.is_none() && sent_term.is_some_and(|at| at.elapsed() >= KILL_AFTER) {
                procs::signal(pid, Signal::KILL);
                sent_kill = Some(Instant::now());
                eprintln!(
                    "delega-acceptance: delega did not stop after SIGTERM and was killed with SIGKILL; its provider may still be running (delega issue 163)"
                );
                ran.problems
                    .push("delega was killed with SIGKILL".to_string());
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        // Bounded wait for both pipes to close.
        let drain_until = Instant::now() + DRAIN;
        let mut closed = 0;
        while closed < readers {
            let left = drain_until.saturating_duration_since(Instant::now());
            match done_rx.recv_timeout(left) {
                Ok(()) => closed += 1,
                Err(_) => break,
            }
        }
        if closed < readers {
            ran.problems.push(format!(
                "standard output or standard error was still open {} s after delega exited (a process it started may hold it); the harness stopped waiting",
                DRAIN.as_secs()
            ));
        }
        ran.stdout = out_buf.lock().map(|b| b.clone()).unwrap_or_default();
        ran
    }
}

/// The verdict of a finished row.
pub fn verdict(row: &LiveRow) -> &'static str {
    if !row.problems.is_empty() {
        "fail"
    } else if !row.not_shown.is_empty() {
        "not_shown"
    } else {
        "pass"
    }
}

fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

/// Whether a `--progress` line is about the shell tool: Claude Code's `Bash`
/// tool call or a Codex `command_execution` item.
pub fn names_shell_tool(line: &str) -> bool {
    line.starts_with("progress: ") && (line.contains("Bash") || line.contains("command_execution"))
}

/// Whether launch arguments from a dry run offer the worker no way to write:
/// for Claude Code, `--tools` names none of `Edit`, `Write`, `NotebookEdit`
/// and `Bash`; for Codex, the sandbox is `read-only`. `None` when the
/// arguments do not say.
pub fn offers_no_write(provider: &str, launch: &[String]) -> Option<bool> {
    if provider == "claude" {
        let tools = launch
            .iter()
            .position(|a| a == "--tools")
            .and_then(|i| launch.get(i + 1))?;
        Some(
            !tools
                .split(',')
                .any(|t| matches!(t.trim(), "Edit" | "Write" | "NotebookEdit" | "Bash")),
        )
    } else {
        let sandbox = launch.iter().find(|a| a.starts_with("sandbox_mode="))?;
        Some(sandbox == "sandbox_mode=\"read-only\"")
    }
}

/// The arguments of one `delega run`.
#[allow(clippy::too_many_arguments)]
pub fn run_args(
    provider: &str,
    mode: &str,
    prompt: &str,
    work: &Path,
    timeout_secs: u64,
    ephemeral: bool,
    resume: Option<&str>,
    extra: &[&str],
) -> Vec<OsString> {
    let mut args = os(&["run", "--output", "json", "--provider", provider]);
    args.push(format!("--{mode}").into());
    args.push("-C".into());
    args.push(work.as_os_str().to_owned());
    args.push("--timeout".into());
    args.push(format!("{timeout_secs}s").into());
    match provider {
        "claude" => args.extend(os(&["--model", CLAUDE_MODEL])),
        _ => args.extend(os(&["--effort", CODEX_EFFORT])),
    }
    if ephemeral {
        args.push("--ephemeral".into());
    }
    if let Some(id) = resume {
        args.push("--resume".into());
        args.push(id.into());
    }
    args.extend(extra.iter().map(OsString::from));
    args.push(format!("--prompt={prompt}").into());
    args
}

pub const PLAIN_PROMPT: &str = "Reply with only the word ready.";
pub const READ_CWD_PROMPT: &str =
    "Read the file word.txt in the current directory and reply with only the word it contains.";
pub const WRITE_PROMPT: &str = "Replace the contents of the file note.txt in the current directory with the word changed. If you cannot, reply with only the word unable.";
pub const BUILD_PROMPT: &str = "Run the command `sh probe.sh` in the current directory once with your shell tool, without changing it, then reply with only the word finished.";
pub const RESUME_PROMPT: &str = "Reply with only the word I asked you to remember.";
pub const SLEEP_PROMPT: &str =
    "Run the command `sleep 120` with your shell tool, then reply with only the word finished.";

fn read_outside_prompt(path: &Path) -> String {
    format!(
        "Read the file {} and reply with only the word it contains. If you cannot read it, reply with only the word unable.",
        path.display()
    )
}

/// The script the build case asks the worker to run. Each step's effect is a
/// file the harness checks afterwards; `probe-done` says the script ran to
/// its end. The path of the temporary file is written before the file is
/// made, so the harness can find and remove it even if the script is stopped
/// in between. The file is made with `set -C`, so a file already there is
/// never written; `name` is random per case, so none is expected.
pub fn probe_script(name: &str, outside: &Path) -> String {
    format!(
        "touch made-in-cwd\n\
         t=\"${{TMPDIR:-/tmp}}/{name}\"\n\
         printf '%s\\n' \"$t\" > tmp-path\n\
         if (set -C; : > \"$t\") 2>/dev/null; then : > tmp-made; fi\n\
         touch '{outside}/written' 2>/dev/null\n\
         if {CURL} -s -m 5 -o /dev/null {PROBE_URL} 2>/dev/null; then touch net-reached; fi\n\
         touch probe-done\n",
        outside = outside.display()
    )
}

/// Whether the harness itself reaches [`PROBE_URL`] with [`CURL`]: `None`
/// when curl is missing. Bounded by curl's own 5 s limit.
fn network_baseline() -> Option<bool> {
    if !Path::new(CURL).is_file() {
        return None;
    }
    Some(
        Command::new(CURL)
            .args(["-s", "-m", "5", "-o", "/dev/null", PROBE_URL])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .guarded_status()
            .is_ok_and(|s| s.success()),
    )
}

/// The path the build probe recorded for its temporary file, if it names a
/// file called `name` at an absolute path.
fn probe_tmp_path(work: &Path, name: &str) -> Option<PathBuf> {
    read_regular(&work.join("tmp-path"), 4096)
        .map(|t| PathBuf::from(t.trim()))
        .filter(|p| p.is_absolute() && p.file_name().is_some_and(|n| n == name))
}

/// Remove the build probe's temporary file: only a regular file (not a link)
/// whose name is the case's random `name`, at the path the probe recorded.
/// Nothing else outside the case directory is ever removed.
pub fn remove_probe_file(work: &Path, name: &str) {
    if let Some(path) = probe_tmp_path(work, name)
        && std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file())
    {
        let _ = std::fs::remove_file(&path);
    }
}

/// What a `delega` invocation left.
#[derive(Default)]
struct Ran {
    /// Whether `delega` was started at all.
    started: bool,
    pid: i32,
    exit_code: Option<i32>,
    exit_signal: Option<i32>,
    stdout: Vec<u8>,
    /// What went wrong with the invocation itself: a bound reached, a signal
    /// needed, a pipe left open.
    problems: Vec<String>,
    /// For the interrupt case: whether SIGINT was sent after the shell tool
    /// was seen (`Some(true)`), at the fallback (`Some(false)`), or not at all.
    after_tool: Option<bool>,
}

/// A case's directories, removed on drop: `work` (the worker's working
/// directory), `tmp` (the `TMPDIR` `delega` gets) and `outside` (a sibling of
/// both that no run is given).
struct CaseDir {
    _root: tempfile::TempDir,
    work: PathBuf,
    tmp: PathBuf,
    outside: PathBuf,
}

impl CaseDir {
    fn new(root: &Path) -> std::io::Result<Self> {
        let made = tempfile::Builder::new()
            .prefix(DIR_PREFIX)
            .tempdir_in(root)?;
        let top = std::fs::canonicalize(made.path())?;
        let work = top.join("work");
        let tmp = top.join("tmp");
        let outside = top.join("outside");
        for d in [&work, &tmp, &outside] {
            std::fs::create_dir(d)?;
        }
        Ok(Self {
            _root: made,
            work,
            tmp,
            outside,
        })
    }
}

/// Parse standard output as exactly one valid `RunResult`.
pub fn parse_result(bytes: &[u8]) -> Result<RunResult, String> {
    let mut docs = serde_json::Deserializer::from_slice(bytes).into_iter::<Value>();
    let first = match docs.next() {
        Some(Ok(v)) => v,
        Some(Err(_)) => return Err("standard output is not JSON".to_string()),
        None => return Err("standard output held no result".to_string()),
    };
    if docs.next().is_some() {
        return Err("standard output held more than one JSON document".to_string());
    }
    let result: RunResult = serde_json::from_value(first)
        .map_err(|_| "standard output is not a RunResult".to_string())?;
    result
        .validate()
        .map_err(|_| "the result does not validate".to_string())?;
    Ok(result)
}

fn outcome_name(r: &RunResult) -> String {
    serde_json::to_value(r.outcome)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn provider_name(r: &RunResult) -> Option<String> {
    r.provider
        .and_then(|p| serde_json::to_value(p).ok())
        .and_then(|v| v.as_str().map(str::to_string))
}

/// The exit status `delega run` gives an outcome.
pub fn exit_for(outcome: &str) -> Option<i32> {
    match outcome {
        "completed" => Some(0),
        "failed" => Some(1),
        "refused" => Some(2),
        "timed_out" => Some(124),
        "interrupted" => Some(130),
        _ => None,
    }
}

/// Record what every run reports and check what holds for every run: one
/// valid result naming the case's provider, and an exit status that follows
/// its outcome. Problems of the invocation itself are the row's problems.
fn take_result(row: &mut LiveRow, ran: &Ran) -> Option<RunResult> {
    row.exit_code = ran.exit_code;
    row.exit_signal = ran.exit_signal;
    row.problems.extend(ran.problems.iter().cloned());
    if !ran.started {
        return None;
    }
    let r = match parse_result(&ran.stdout) {
        Ok(r) => r,
        Err(err) => {
            row.problems.push(err);
            return None;
        }
    };
    record(row, &r);
    let outcome = outcome_name(&r);
    if ran.exit_code != exit_for(&outcome) {
        row.problems.push(format!(
            "exit status {:?} does not follow the outcome {outcome}",
            ran.exit_code
        ));
    }
    let named = provider_name(&r);
    let case_provider = row.provider.clone();
    check(
        row,
        named.as_deref() == Some(case_provider.as_str()),
        "the result names the case's provider",
    );
    Some(r)
}

/// Copy the reportable fields of a result into the row. Never the answer
/// text, a denial's text or a raw version line.
pub fn record(row: &mut LiveRow, r: &RunResult) {
    row.outcome = Some(outcome_name(r));
    row.cleanup_confirmed = Some(matches!(r.cleanup, Cleanup::Confirmed));
    row.denied = r.permissions.denied.len();
    row.wider_note = r
        .provider_notes
        .iter()
        .any(|n| n.starts_with("wider than requested"));
    row.error_kind = r.error.as_ref().map(|e| e.kind.clone());
    if let Some(v) = r
        .provenance
        .provider_version
        .as_deref()
        .and_then(|raw| version_token(&row.provider, raw))
    {
        row.provider_version = Some(v);
    }
    if let Some(usage) = &r.usage {
        if let Some(c) = usage.cost_usd {
            row.cost_usd = Some(row.cost_usd.unwrap_or(0.0) + c);
        }
        if let Some(t) = usage.input_tokens {
            row.input_tokens = Some(row.input_tokens.unwrap_or(0) + t);
        }
        if let Some(t) = usage.output_tokens {
            row.output_tokens = Some(row.output_tokens.unwrap_or(0) + t);
        }
    }
}

fn check(row: &mut LiveRow, holds: bool, label: &str) {
    if holds {
        row.checks.push(label.to_string());
    } else {
        row.problems.push(format!("not so: {label}"));
    }
}

fn check_outcome(row: &mut LiveRow, r: &RunResult, allowed: &[&str]) {
    let outcome = outcome_name(r);
    if allowed.contains(&outcome.as_str()) {
        row.checks.push(format!("outcome {outcome}"));
    } else {
        let kind = r
            .error
            .as_ref()
            .map_or(String::new(), |e| format!(" (error {})", e.kind));
        row.problems.push(format!(
            "outcome {outcome}{kind} where {} was expected",
            allowed.join(" or ")
        ));
    }
}

fn check_cleanup(row: &mut LiveRow, r: &RunResult) {
    check(
        row,
        matches!(r.cleanup, Cleanup::Confirmed),
        "cleanup confirmed",
    );
}

fn contains_word(text: &str, word: &str) -> bool {
    text.to_lowercase().contains(&word.to_lowercase())
}

pub fn judge_plain(row: &mut LiveRow, r: &RunResult) {
    check_outcome(row, r, &["completed"]);
    check_cleanup(row, r);
    check(row, !r.text.trim().is_empty(), "the answer is not empty");
}

pub fn judge_read_cwd(row: &mut LiveRow, r: &RunResult, word: &str) {
    check_outcome(row, r, &["completed"]);
    check_cleanup(row, r);
    check(
        row,
        contains_word(&r.text, word),
        "the answer has the word seeded in the working directory",
    );
}

/// Whether a denial is of a write: for Claude Code a request for `Write`,
/// `Edit`, `NotebookEdit` or `Bash`; for Codex any declined command.
fn denies_a_write(provider: &str, r: &RunResult) -> bool {
    r.permissions.denied.iter().any(|d| {
        provider != "claude"
            || ["Write", "Edit", "NotebookEdit", "Bash"]
                .iter()
                .any(|tool| d.request.starts_with(tool))
    })
}

/// The file must be unchanged. The case shows something only if the worker
/// tried to write and was denied (a write denial is listed), or was offered
/// no way to write (the dry run's launch plan, `no_write`). Without either,
/// an unchanged file could mean the worker never tried, so the case is
/// `not_shown`.
pub fn judge_write_refused(
    row: &mut LiveRow,
    provider: &str,
    r: &RunResult,
    unchanged: bool,
    no_write: Option<bool>,
) {
    check_outcome(row, r, &["completed", "refused"]);
    check_cleanup(row, r);
    check(row, unchanged, "the seeded file is unchanged");
    if denies_a_write(provider, r) {
        row.checks
            .push("permissions.denied lists a write the worker attempted".to_string());
    } else if no_write == Some(true) {
        row.checks.push(
            if provider == "claude" {
                "the launch plan offered no Edit, Write, NotebookEdit or Bash tool"
            } else {
                "the launch plan ran Codex in its read-only sandbox"
            }
            .to_string(),
        );
    } else {
        row.not_shown
            .push("that the worker attempted a write or was offered no way to write".to_string());
    }
}

/// Claude Code scopes `Read` to the working directory, so the read is denied
/// and the denial names the outside file. Codex does not confine reads, so its
/// run must say it is wider than requested, and whether the word came back is
/// recorded, not asserted.
pub fn judge_read_outside(
    row: &mut LiveRow,
    provider: &str,
    r: &RunResult,
    word: &str,
    path: &Path,
) {
    if provider == "claude" {
        check_outcome(row, r, &["completed", "refused"]);
        check_cleanup(row, r);
        check(
            row,
            !contains_word(&r.text, word),
            "the answer does not have the word seeded outside",
        );
        let full = path.to_string_lossy().to_string();
        let short = full
            .strip_prefix("/private")
            .map(str::to_string)
            .unwrap_or_else(|| full.clone());
        check(
            row,
            r.permissions
                .denied
                .iter()
                .any(|d| d.request.contains(&full) || d.request.contains(&short)),
            "permissions.denied names the outside file",
        );
    } else {
        check_outcome(row, r, &["completed"]);
        check_cleanup(row, r);
        check(
            row,
            r.provider_notes
                .iter()
                .any(|n| n.starts_with("wider than requested")),
            "a provider note says the run is wider than requested",
        );
        row.notes.push(format!(
            "the answer {} the word seeded outside",
            if contains_word(&r.text, word) {
                "has"
            } else {
                "does not have"
            }
        ));
    }
}

/// Where the build probe's temporary file went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TmpPlace {
    /// The `TMPDIR` the harness gave `delega`.
    Harness,
    /// Claude Code's per-user directory, `/tmp/claude-<uid>`.
    ClaudeUser,
    /// Elsewhere under `/tmp`.
    SlashTmp,
    /// The working directory, which is not a temporary directory.
    Work,
    /// Anywhere else, which is not counted as a temporary directory.
    Other,
}

impl TmpPlace {
    pub fn label(&self) -> &'static str {
        match self {
            TmpPlace::Harness => "the TMPDIR the harness gave delega",
            TmpPlace::ClaudeUser => "Claude Code's per-user temporary directory",
            TmpPlace::SlashTmp => "/tmp",
            TmpPlace::Work => "the working directory",
            TmpPlace::Other => "another directory",
        }
    }
}

/// What the build probe left behind.
#[derive(Debug, Clone, Default)]
pub struct BuildProbe {
    /// `probe-done` exists: the script ran to its end.
    pub done: bool,
    pub cwd_file: bool,
    /// The temporary file the script made and where it is.
    pub tmp_file: Option<TmpPlace>,
    pub outside_file: bool,
    pub net_reached: bool,
    /// Whether the harness itself reached the probe's address before the
    /// run; `None` when curl is missing.
    pub net_baseline: Option<bool>,
    /// The outside directory lies under `/tmp`, which Codex's sandbox lets
    /// commands write.
    pub outside_under_tmp: bool,
}

impl BuildProbe {
    fn observe(dir: &CaseDir, name: &str, net_baseline: Option<bool>) -> Self {
        let tmp_file = probe_tmp_path(&dir.work, name)
            .filter(|p| {
                present(&dir.work.join("tmp-made"))
                    && std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_file())
            })
            .map(|p| classify_tmp(&p, &dir.tmp, &dir.work));
        Self {
            done: present(&dir.work.join("probe-done")),
            cwd_file: present(&dir.work.join("made-in-cwd")),
            tmp_file,
            outside_file: present(&dir.outside.join("written")),
            net_reached: present(&dir.work.join("net-reached")),
            net_baseline,
            outside_under_tmp: under_slash_tmp(&dir.outside),
        }
    }
}

pub fn classify_tmp(path: &Path, harness_tmp: &Path, work: &Path) -> TmpPlace {
    let text = path.to_string_lossy();
    if path.starts_with(work) {
        TmpPlace::Work
    } else if path.starts_with(harness_tmp) {
        TmpPlace::Harness
    } else if text.starts_with("/tmp/claude-") || text.starts_with("/private/tmp/claude-") {
        TmpPlace::ClaudeUser
    } else if under_slash_tmp(path) {
        TmpPlace::SlashTmp
    } else {
        TmpPlace::Other
    }
}

pub fn judge_build(row: &mut LiveRow, provider: &str, r: &RunResult, probe: &BuildProbe) {
    check_outcome(row, r, &["completed"]);
    check_cleanup(row, r);
    check(row, probe.done, "the probe script ran to its end");
    check(
        row,
        probe.cwd_file,
        "a command wrote a file in the working directory",
    );
    const TMP_WRITE: &str = "a command wrote a file in the temporary directory";
    match &probe.tmp_file {
        None | Some(TmpPlace::Work) => row.problems.push(format!("not so: {TMP_WRITE}")),
        Some(TmpPlace::Other) => row.not_shown.push(format!(
            "that {TMP_WRITE}: the file was written in {}, which is not counted as one",
            TmpPlace::Other.label()
        )),
        Some(place) => {
            row.checks.push(TMP_WRITE.to_string());
            row.notes
                .push(format!("the temporary file was in {}", place.label()));
        }
    }
    const OUTSIDE: &str =
        "a write to a directory outside the working and temporary directories was refused";
    if provider == "codex" && probe.outside_under_tmp {
        row.not_shown.push(format!(
            "that {OUTSIDE}: the outside directory is under /tmp, which Codex's build sandbox lets commands write"
        ));
    } else {
        check(row, !probe.outside_file, OUTSIDE);
    }
    match probe.net_baseline {
        None => row
            .not_shown
            .push(format!("that the network was off: {CURL} is missing")),
        Some(false) => row.not_shown.push(format!(
            "that the network was off: the harness itself could not reach {PROBE_URL}"
        )),
        Some(true) => check(row, !probe.net_reached, "the network was off"),
    }
}

pub fn judge_resume(row: &mut LiveRow, first: &RunResult, second: &RunResult, word: &str) {
    check(
        row,
        outcome_name(first) == "completed",
        "the first run completed",
    );
    check_outcome(row, second, &["completed"]);
    check_cleanup(row, first);
    check_cleanup(row, second);
    check(
        row,
        first.session_id.is_some() && second.session_id == first.session_id,
        "the follow-up reports the first run's session",
    );
    check(
        row,
        contains_word(&second.text, word),
        "the follow-up recalls the word from the first prompt",
    );
}

/// The interrupt counts only when it landed while the shell tool was
/// running: SIGINT sent after a progress line named it. At the fallback the
/// case is `not_shown`; with no signal sent at all, `delega` ended on its own,
/// which fails.
pub fn judge_interrupt(row: &mut LiveRow, r: &RunResult, after_tool: Option<bool>) {
    check_outcome(row, r, &["interrupted"]);
    check_cleanup(row, r);
    check(row, row.exit_code == Some(130), "exit status 130");
    match after_tool {
        Some(true) => row
            .checks
            .push("SIGINT was sent 2 s after a progress line named the shell tool".to_string()),
        Some(false) => row.not_shown.push(
            "an interrupt while the shell tool ran: no progress line named it, and SIGINT was sent 30 s after the start"
                .to_string(),
        ),
        None => row
            .problems
            .push("delega ended before the harness sent SIGINT".to_string()),
    }
}

pub fn judge_timeout(row: &mut LiveRow, r: &RunResult) {
    check_outcome(row, r, &["timed_out"]);
    check_cleanup(row, r);
    check(row, row.exit_code == Some(124), "exit status 124");
    check(
        row,
        r.timing.timeout_secs == SHORT_TIMEOUT_SECS,
        "the result states the time limit given",
    );
}

/// `delega doctor --output json`: the provider is ready, and its version
/// number is recorded.
pub fn judge_doctor(row: &mut LiveRow, stdout: &[u8]) {
    let Ok(report) = serde_json::from_slice::<Value>(stdout) else {
        row.problems
            .push("doctor did not print a JSON report".to_string());
        return;
    };
    let entry = report["providers"]
        .as_array()
        .and_then(|list| list.iter().find(|p| p["provider"] == row.provider.as_str()));
    let Some(entry) = entry else {
        row.problems
            .push("doctor's report has no entry for the provider".to_string());
        return;
    };
    row.provider_version = entry["version"]
        .as_str()
        .and_then(|raw| version_token(&row.provider, raw));
    check(
        row,
        entry["ready"] == true,
        "doctor reports the provider ready",
    );
    let blocking = report["problems"].as_array().map_or(0, |list| {
        list.iter()
            .filter(|p| p["provider"] == row.provider.as_str() && p["blocks"] == "run")
            .count()
    });
    row.notes
        .push(format!("{blocking} problems that block a run"));
}

/// One line per case for the terminal.
pub fn line(row: &LiveRow) -> String {
    let mut out = format!(
        "{:<6} {:<13} {:<9} {:<11} {:>6.1} s",
        row.provider,
        row.case,
        row.verdict,
        row.outcome.as_deref().unwrap_or("-"),
        row.elapsed_ms as f64 / 1000.0
    );
    for problem in &row.problems {
        out.push_str("\n    ");
        out.push_str(problem);
    }
    for item in &row.not_shown {
        out.push_str("\n    not shown: ");
        out.push_str(item);
    }
    out
}

/// The summary lines and whether no case failed. A `not_shown` case does not
/// fail the run; it is counted and listed.
pub fn summary(rows: &[LiveRow]) -> (String, bool) {
    let count = |v: &str| rows.iter().filter(|r| r.verdict == v).count();
    let failed = count("fail");
    let mut out = format!(
        "{} live cases: {} pass, {failed} fail, {} not shown\n",
        rows.len(),
        count("pass"),
        count("not_shown")
    );
    for provider in PROVIDERS {
        let mine: Vec<&LiveRow> = rows.iter().filter(|r| r.provider == provider).collect();
        if mine.is_empty() {
            continue;
        }
        let version = mine
            .iter()
            .find_map(|r| r.provider_version.clone())
            .unwrap_or_else(|| format!("{provider}, version unknown"));
        let costs: Vec<f64> = mine.iter().filter_map(|r| r.cost_usd).collect();
        let cost = if costs.is_empty() {
            "no cost reported".to_string()
        } else {
            format!("reported cost ${:.4}", costs.iter().sum::<f64>())
        };
        let input: u64 = mine.iter().filter_map(|r| r.input_tokens).sum();
        let output: u64 = mine.iter().filter_map(|r| r.output_tokens).sum();
        let runs: u32 = mine.iter().map(|r| r.runs).sum();
        out.push_str(&format!(
            "{version}: {runs} runs, {cost}, {input} input and {output} output tokens reported\n"
        ));
    }
    (out, failed == 0)
}

/// The metadata of a run: the first record of `--out`.
pub fn meta(delega: &Path, rows: &[LiveRow], wall: Duration) -> Value {
    let run = |program: &str, args: &[&str]| -> String {
        Command::new(program)
            .args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .guarded_output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };
    let version = Command::new(delega)
        .args(["version", "--output", "json"])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .guarded_output()
        .ok()
        .and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
        .map_or(Value::Null, |v| v["version"].clone());
    serde_json::json!({
        "date": run("date", &["-u", "+%Y-%m-%d"]),
        "os": run("uname", &["-srm"]),
        "delega_version": version,
        "harness_commit": run("git", &["-C", &crate::delega_bin::workspace_root().to_string_lossy(), "rev-parse", "--short", "HEAD"]),
        "providers": providers_of(rows),
        "cases": rows.len(),
        "claude_model": CLAUDE_MODEL,
        "codex_effort": CODEX_EFFORT,
        "wall_secs": wall.as_secs(),
    })
}

/// The version recorded for each provider in `rows`.
fn providers_of(rows: &[LiveRow]) -> serde_json::Map<String, Value> {
    let mut providers = serde_json::Map::new();
    for provider in PROVIDERS {
        if let Some(v) = rows
            .iter()
            .filter(|r| r.provider == provider)
            .find_map(|r| r.provider_version.clone())
        {
            providers.insert(provider.to_string(), Value::String(v));
        }
    }
    providers
}

/// Write the metadata as the first line and then one line per row, to a
/// temporary file beside `path` that is renamed into place, so the file holds
/// the rows and their metadata together or is left as it was.
pub fn write_live(path: &Path, meta: &Value, rows: &[LiveRow]) -> std::io::Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut tmp = tempfile::Builder::new()
        .prefix(".live-out-")
        .tempfile_in(dir)?;
    {
        let file = tmp.as_file_mut();
        let first = serde_json::json!({ "meta": meta });
        writeln!(file, "{}", serde_json::to_string(&first)?)?;
        for row in rows {
            writeln!(file, "{}", serde_json::to_string(row)?)?;
        }
        file.flush()?;
    }
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Read a file `write_live` wrote: the metadata line and the rows. Refused
/// when the first line is not metadata, a line is not a row, or the metadata
/// does not match the rows (the number of cases, or a provider's version).
pub fn read_live(text: &str) -> Result<(Value, Vec<LiveRow>), String> {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let first: Value = lines
        .next()
        .ok_or("the live results are empty")
        .and_then(|l| serde_json::from_str(l).map_err(|_| "the first line is not JSON"))?;
    let meta = first
        .get("meta")
        .filter(|m| m.is_object())
        .cloned()
        .ok_or("the first line of the live results is not their metadata")?;
    let rows: Vec<LiveRow> = lines
        .enumerate()
        .map(|(i, l)| serde_json::from_str(l).map_err(|e| format!("live row {}: {e}", i + 1)))
        .collect::<Result<_, _>>()?;
    if meta["cases"].as_u64() != u64::try_from(rows.len()).ok() {
        return Err(format!(
            "the live metadata says {} cases and the file holds {} rows",
            meta["cases"],
            rows.len()
        ));
    }
    let versions = providers_of(&rows);
    if meta["providers"].as_object() != Some(&versions) {
        return Err("the live metadata's provider versions do not match the rows".to_string());
    }
    Ok((meta, rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> RunResult {
        let text = match name {
            "claude-completed" => include_str!("../tests/fixtures/live/claude-completed.json"),
            "codex-completed" => include_str!("../tests/fixtures/live/codex-completed.json"),
            "claude-timed-out" => include_str!("../tests/fixtures/live/claude-timed-out.json"),
            "codex-interrupted" => include_str!("../tests/fixtures/live/codex-interrupted.json"),
            _ => panic!("no fixture {name}"),
        };
        parse_result(text.as_bytes()).expect("a recorded result")
    }

    fn row(provider: &str) -> LiveRow {
        LiveRow {
            provider: provider.to_string(),
            ..LiveRow::default()
        }
    }

    fn with_text(mut r: RunResult, text: &str) -> RunResult {
        r.text = text.to_string();
        r
    }

    fn denied(mut r: RunResult, request: &str) -> RunResult {
        r.permissions
            .denied
            .push(delega_core::contract::result::DeniedRequest {
                request: request.to_string(),
            });
        r
    }

    fn ran_with(stdout: &str, exit: i32) -> Ran {
        Ran {
            started: true,
            exit_code: Some(exit),
            stdout: stdout.as_bytes().to_vec(),
            ..Ran::default()
        }
    }

    #[test]
    fn plan_runs_doctor_only_when_both_providers_are_selected() {
        let both = plan(None).expect("plan");
        assert_eq!(both.len(), 2 * Case::ALL.len());
        assert!(
            both[..Case::ALL.len()]
                .iter()
                .all(|p| p.provider == "claude")
        );
        let codex = plan(Some("codex")).expect("plan");
        assert_eq!(codex.len(), Case::ALL.len() - 1);
        assert!(codex.iter().all(|p| p.provider == "codex"));
        assert!(codex.iter().all(|p| p.case != Case::Doctor));
        assert_eq!(codex[0].id(), "codex-plain-answer");
        assert!(plan(Some("gemini")).is_err());
    }

    #[test]
    fn every_run_asks_for_the_cheap_model_or_effort_and_a_time_limit() {
        let work = Path::new("/w");
        let claude = run_args("claude", "read", "p", work, 120, true, None, &[]);
        let claude: Vec<String> = claude.iter().map(|a| a.to_string_lossy().into()).collect();
        assert!(
            claude.windows(2).any(|w| w == ["--model", "haiku"]),
            "{claude:?}"
        );
        assert!(claude.windows(2).any(|w| w == ["--timeout", "120s"]));
        assert!(claude.contains(&"--ephemeral".to_string()));
        assert!(claude.contains(&"--read".to_string()));
        let codex = run_args("codex", "build", "-p", work, 10, false, Some("sid"), &[]);
        let codex: Vec<String> = codex.iter().map(|a| a.to_string_lossy().into()).collect();
        assert!(
            codex.windows(2).any(|w| w == ["--effort", "low"]),
            "{codex:?}"
        );
        assert!(codex.windows(2).any(|w| w == ["--resume", "sid"]));
        assert!(!codex.contains(&"--ephemeral".to_string()));
        assert!(!codex.contains(&"--model".to_string()));
        // A prompt that starts with a hyphen is not read as a flag.
        assert_eq!(codex.last().map(String::as_str), Some("--prompt=-p"));
    }

    #[test]
    fn a_result_naming_another_provider_fails_the_case() {
        let text = include_str!("../tests/fixtures/live/codex-completed.json");
        let mut claude_row = row("claude");
        assert!(take_result(&mut claude_row, &ran_with(text, 0)).is_some());
        assert_eq!(
            claude_row.problems,
            vec!["not so: the result names the case's provider"]
        );
        let mut codex_row = row("codex");
        take_result(&mut codex_row, &ran_with(text, 0));
        assert!(codex_row.problems.is_empty(), "{:?}", codex_row.problems);
        let mut wrong_exit = row("codex");
        take_result(&mut wrong_exit, &ran_with(text, 1));
        assert_eq!(wrong_exit.problems.len(), 1, "{:?}", wrong_exit.problems);
    }

    #[test]
    fn a_version_line_is_reduced_to_the_provider_and_its_number() {
        assert_eq!(
            version_token("claude", "2.1.330 (Claude Code)").as_deref(),
            Some("claude 2.1.330")
        );
        assert_eq!(
            version_token("codex", "codex-cli 0.171.0").as_deref(),
            Some("codex 0.171.0")
        );
        assert_eq!(
            version_token("codex", "build 7 for user jo 1.2-beta").as_deref(),
            Some("codex 1.2")
        );
        assert_eq!(version_token("claude", "no number here 7"), None);
        let mut r = row("claude");
        record(&mut r, &fixture("claude-completed"));
        assert_eq!(r.provider_version.as_deref(), Some("claude 2.1.400"));
    }

    #[test]
    fn a_recorded_completed_result_passes_the_plain_case_and_an_empty_answer_fails_it() {
        let mut ok = row("claude");
        judge_plain(&mut ok, &fixture("claude-completed"));
        assert_eq!(verdict(&ok), "pass", "{:?}", ok.problems);
        let mut empty = row("claude");
        judge_plain(&mut empty, &with_text(fixture("claude-completed"), "  "));
        assert_eq!(empty.problems.len(), 1, "{:?}", empty.problems);
    }

    #[test]
    fn the_read_case_needs_the_seeded_word_in_the_answer() {
        let mut ok = row("codex");
        judge_read_cwd(
            &mut ok,
            &with_text(fixture("codex-completed"), "Lantern42"),
            "lantern42",
        );
        assert!(ok.problems.is_empty(), "{:?}", ok.problems);
        let mut bad = row("codex");
        judge_read_cwd(&mut bad, &fixture("codex-completed"), "lantern42");
        assert_eq!(bad.problems.len(), 1);
    }

    #[test]
    fn the_write_case_passes_only_on_a_write_denial_or_a_plan_with_no_write() {
        let r = fixture("claude-completed");
        let mut planned = row("claude");
        judge_write_refused(&mut planned, "claude", &r, true, Some(true));
        assert_eq!(verdict(&planned), "pass", "{planned:?}");
        let mut attempted = row("claude");
        let d = denied(r.clone(), "Write: /w/note.txt");
        judge_write_refused(&mut attempted, "claude", &d, true, Some(false));
        assert_eq!(verdict(&attempted), "pass", "{attempted:?}");
        // A read denial is not a write attempt, and an unreadable plan shows
        // nothing.
        let mut unknown = row("claude");
        let d = denied(r.clone(), "Read: /etc/hosts");
        judge_write_refused(&mut unknown, "claude", &d, true, None);
        assert_eq!(verdict(&unknown), "not_shown", "{unknown:?}");
        let mut changed = row("claude");
        judge_write_refused(&mut changed, "claude", &r, false, Some(true));
        assert_eq!(verdict(&changed), "fail");
        assert_eq!(
            changed.problems,
            vec!["not so: the seeded file is unchanged"]
        );
    }

    #[test]
    fn the_dry_run_plan_shows_no_write_from_the_tools_or_the_sandbox() {
        let strings = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let read = strings(&["--tools", "Read,Glob,Grep", "--allowedTools", "x"]);
        assert_eq!(offers_no_write("claude", &read), Some(true));
        let build = strings(&["--tools", "Read,Glob,Grep,Edit,Write,Bash"]);
        assert_eq!(offers_no_write("claude", &build), Some(false));
        assert_eq!(offers_no_write("claude", &strings(&["-p"])), None);
        let ro = strings(&["-c", "sandbox_mode=\"read-only\""]);
        assert_eq!(offers_no_write("codex", &ro), Some(true));
        let ww = strings(&["-c", "sandbox_mode=\"workspace-write\""]);
        assert_eq!(offers_no_write("codex", &ww), Some(false));
        assert_eq!(offers_no_write("codex", &strings(&["exec"])), None);
    }

    #[test]
    fn claude_must_deny_the_outside_file_itself_and_codex_must_say_it_is_wider() {
        let word = "thistle7";
        let path = Path::new("/private/var/x/dlive-1/outside/secret.txt");
        let r = fixture("claude-completed");
        let mut ok = row("claude");
        let d = denied(r.clone(), "Read: /private/var/x/dlive-1/outside/secret.txt");
        judge_read_outside(&mut ok, "claude", &d, word, path);
        assert!(ok.problems.is_empty(), "{:?}", ok.problems);
        let mut short = row("claude");
        let d = denied(r.clone(), "Read: /var/x/dlive-1/outside/secret.txt");
        judge_read_outside(&mut short, "claude", &d, word, path);
        assert!(short.problems.is_empty(), "{:?}", short.problems);
        // A denial of something else does not count.
        let mut other = row("claude");
        let d = denied(r.clone(), "Read: /etc/hosts");
        judge_read_outside(&mut other, "claude", &d, word, path);
        assert_eq!(
            other.problems,
            vec!["not so: permissions.denied names the outside file"]
        );
        let mut leaked = row("claude");
        let d = denied(
            with_text(r, "Thistle7"),
            "Read: /var/x/dlive-1/outside/secret.txt",
        );
        judge_read_outside(&mut leaked, "claude", &d, word, path);
        assert_eq!(leaked.problems.len(), 1, "{:?}", leaked.problems);

        // Codex reads anywhere: the word coming back is a note, not a failure.
        let mut codex = row("codex");
        let r = with_text(fixture("codex-completed"), "thistle7");
        judge_read_outside(&mut codex, "codex", &r, word, path);
        assert!(codex.problems.is_empty(), "{:?}", codex.problems);
        assert!(codex.notes.iter().any(|n| n.contains("has the word")));
        let mut no_note = row("codex");
        let mut r = fixture("codex-completed");
        r.provider_notes
            .retain(|n| !n.starts_with("wider than requested"));
        judge_read_outside(&mut no_note, "codex", &r, word, path);
        assert_eq!(no_note.problems.len(), 1, "{:?}", no_note.problems);
    }

    fn good_probe() -> BuildProbe {
        BuildProbe {
            done: true,
            cwd_file: true,
            tmp_file: Some(TmpPlace::ClaudeUser),
            outside_file: false,
            net_reached: false,
            net_baseline: Some(true),
            outside_under_tmp: false,
        }
    }

    #[test]
    fn the_build_case_fails_on_an_outside_write_a_reached_network_or_a_script_cut_short() {
        let r = fixture("claude-completed");
        let mut ok = row("claude");
        judge_build(&mut ok, "claude", &r, &good_probe());
        assert_eq!(verdict(&ok), "pass", "{ok:?}");
        for (what, probe) in [
            (
                "outside",
                BuildProbe {
                    outside_file: true,
                    ..good_probe()
                },
            ),
            (
                "network",
                BuildProbe {
                    net_reached: true,
                    ..good_probe()
                },
            ),
            (
                "cut short",
                BuildProbe {
                    done: false,
                    ..good_probe()
                },
            ),
            (
                "no cwd file",
                BuildProbe {
                    cwd_file: false,
                    ..good_probe()
                },
            ),
            (
                "no tmp file",
                BuildProbe {
                    tmp_file: None,
                    ..good_probe()
                },
            ),
            (
                "tmp file in work",
                BuildProbe {
                    tmp_file: Some(TmpPlace::Work),
                    ..good_probe()
                },
            ),
        ] {
            let mut bad = row("claude");
            judge_build(&mut bad, "claude", &r, &probe);
            assert_eq!(bad.problems.len(), 1, "{what}: {:?}", bad.problems);
        }
    }

    #[test]
    fn the_build_case_is_not_shown_when_a_check_could_not_be_made() {
        let r = fixture("codex-completed");
        for (what, probe) in [
            (
                "no curl",
                BuildProbe {
                    net_baseline: None,
                    ..good_probe()
                },
            ),
            (
                "unreachable from the harness",
                BuildProbe {
                    net_baseline: Some(false),
                    ..good_probe()
                },
            ),
            (
                "temporary file elsewhere",
                BuildProbe {
                    tmp_file: Some(TmpPlace::Other),
                    ..good_probe()
                },
            ),
            (
                "outside under /tmp for codex",
                BuildProbe {
                    outside_file: true,
                    outside_under_tmp: true,
                    ..good_probe()
                },
            ),
        ] {
            let mut shown = row("codex");
            judge_build(&mut shown, "codex", &r, &probe);
            assert_eq!(verdict(&shown), "not_shown", "{what}: {shown:?}");
            assert_eq!(shown.not_shown.len(), 1, "{what}");
        }
        // For Claude an outside directory under /tmp is still asserted.
        let probe = BuildProbe {
            outside_file: true,
            outside_under_tmp: true,
            ..good_probe()
        };
        let mut claude = row("claude");
        judge_build(&mut claude, "claude", &fixture("claude-completed"), &probe);
        assert_eq!(verdict(&claude), "fail");
    }

    #[test]
    fn the_temporary_file_is_placed_by_its_path() {
        let tmp = Path::new("/v/dlive-1/tmp");
        let work = Path::new("/v/dlive-1/work");
        let place = |p: &str| classify_tmp(Path::new(p), tmp, work);
        assert_eq!(place("/v/dlive-1/tmp/x"), TmpPlace::Harness);
        assert_eq!(place("/v/dlive-1/work/x"), TmpPlace::Work);
        assert_eq!(place("/tmp/claude-501/x"), TmpPlace::ClaudeUser);
        assert_eq!(place("/private/tmp/claude-501/x"), TmpPlace::ClaudeUser);
        assert_eq!(place("/tmp/x"), TmpPlace::SlashTmp);
        assert_eq!(place("/var/x"), TmpPlace::Other);
    }

    #[test]
    fn the_probe_script_records_its_path_first_and_never_overwrites_a_file() {
        let script = probe_script("dlive-probe-ab12", Path::new("/v/dlive-1/outside"));
        assert!(
            script.contains("touch '/v/dlive-1/outside/written'"),
            "{script}"
        );
        assert!(
            script.contains("${TMPDIR:-/tmp}/dlive-probe-ab12"),
            "{script}"
        );
        let recorded = script.find("> tmp-path").expect("records the path");
        let made = script
            .find("set -C")
            .expect("makes the file with noclobber");
        assert!(recorded < made, "{script}");
        assert!(script.ends_with("touch probe-done\n"));
        assert_ne!(random_hex(8), random_hex(8));
        assert_eq!(random_hex(8).len(), 16);
    }

    #[test]
    fn only_the_probe_file_with_the_random_name_is_removed() {
        let scratch = tempfile::tempdir().expect("scratch");
        let work = scratch.path().join("work");
        let elsewhere = scratch.path().join("elsewhere");
        std::fs::create_dir(&work).expect("work");
        std::fs::create_dir(&elsewhere).expect("elsewhere");
        let name = format!("{DIR_PREFIX}probe-{}", random_hex(8));
        let ours = elsewhere.join(&name);
        let theirs = elsewhere.join("keep.txt");
        std::fs::write(&ours, "").expect("probe file");
        std::fs::write(&theirs, "keep").expect("other file");

        // A recorded path with another name is not removed.
        std::fs::write(work.join("tmp-path"), format!("{}\n", theirs.display())).expect("path");
        remove_probe_file(&work, &name);
        assert!(theirs.exists());
        // A link with the random name is not followed or removed.
        let link_dir = scratch.path().join("links");
        std::fs::create_dir(&link_dir).expect("links");
        let link = link_dir.join(&name);
        std::os::unix::fs::symlink(&theirs, &link).expect("link");
        std::fs::write(work.join("tmp-path"), format!("{}\n", link.display())).expect("path");
        remove_probe_file(&work, &name);
        assert!(present(&link) && theirs.exists());
        // The file with the random name at the recorded path is.
        std::fs::write(work.join("tmp-path"), format!("{}\n", ours.display())).expect("path");
        remove_probe_file(&work, &name);
        assert!(!ours.exists());
        assert!(theirs.exists());
    }

    #[test]
    fn reading_a_fifo_left_by_a_worker_returns_nothing_at_once() {
        let scratch = tempfile::tempdir().expect("scratch");
        let fifo = scratch.path().join("tmp-path");
        let made = Command::new("/usr/bin/mkfifo")
            .arg(&fifo)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .guarded_status()
            .expect("run mkfifo");
        assert!(made.success());
        let began = Instant::now();
        assert_eq!(read_regular(&fifo, 4096), None);
        assert!(began.elapsed() < Duration::from_secs(1));
        let file = scratch.path().join("f");
        std::fs::write(&file, "abc").expect("file");
        assert_eq!(read_regular(&file, 2).as_deref(), Some("ab"));
    }

    #[test]
    fn the_follow_up_must_report_the_same_session_and_recall_the_word() {
        let mut first = fixture("codex-completed");
        first.session_id = Some("s1".to_string());
        let mut second = with_text(fixture("codex-completed"), "harbor9");
        second.session_id = Some("s1".to_string());
        let mut ok = row("codex");
        judge_resume(&mut ok, &first, &second, "harbor9");
        assert!(ok.problems.is_empty(), "{:?}", ok.problems);
        let mut other = second.clone();
        other.session_id = Some("s2".to_string());
        let mut bad = row("codex");
        judge_resume(&mut bad, &first, &other, "harbor9");
        assert_eq!(bad.problems.len(), 1, "{:?}", bad.problems);
        let mut forgot = row("codex");
        judge_resume(&mut forgot, &first, &fixture("codex-completed"), "harbor9");
        assert_eq!(forgot.problems.len(), 2, "{:?}", forgot.problems);
    }

    #[test]
    fn the_interrupt_passes_only_when_it_landed_after_the_shell_tool_was_seen() {
        let r = fixture("codex-interrupted");
        let judged = |after: Option<bool>| {
            let mut row = row("codex");
            row.exit_code = Some(130);
            judge_interrupt(&mut row, &r, after);
            row
        };
        assert_eq!(verdict(&judged(Some(true))), "pass");
        assert_eq!(verdict(&judged(Some(false))), "not_shown");
        assert_eq!(verdict(&judged(None)), "fail");
        let mut completed = row("codex");
        completed.exit_code = Some(0);
        judge_interrupt(&mut completed, &fixture("codex-completed"), Some(true));
        assert_eq!(completed.problems.len(), 2, "{:?}", completed.problems);
    }

    #[test]
    fn the_timeout_needs_its_outcome_exit_status_and_confirmed_cleanup() {
        let mut r = fixture("claude-timed-out");
        r.timing.timeout_secs = SHORT_TIMEOUT_SECS;
        let mut ok = row("claude");
        ok.exit_code = Some(124);
        judge_timeout(&mut ok, &r);
        assert!(ok.problems.is_empty(), "{:?}", ok.problems);
        r.cleanup = Cleanup::Unconfirmed {
            pids: vec![1],
            process_groups: vec![],
            reason: "x could not be checked".to_string(),
        };
        let mut unconfirmed = row("claude");
        unconfirmed.exit_code = Some(124);
        judge_timeout(&mut unconfirmed, &r);
        assert_eq!(unconfirmed.problems, vec!["not so: cleanup confirmed"]);
    }

    #[test]
    fn standard_output_must_hold_exactly_one_valid_result() {
        let one = include_str!("../tests/fixtures/live/claude-completed.json");
        assert!(parse_result(one.as_bytes()).is_ok());
        assert!(parse_result(b"").is_err());
        assert!(parse_result(format!("{one}{one}").as_bytes()).is_err());
        assert!(parse_result(b"{\"outcome\":\"completed\"}").is_err());
    }

    #[test]
    fn the_row_records_counts_and_version_numbers_and_never_the_answer() {
        let mut r = with_text(fixture("claude-completed"), "a private answer");
        r.provenance.provider_version = Some("2.1.400 (Claude Code) /home/someone".to_string());
        r = denied(r, "Read: x");
        let mut row = row("claude");
        record(&mut row, &r);
        record(&mut row, &r);
        assert_eq!(row.denied, 1);
        assert_eq!(row.provider_version.as_deref(), Some("claude 2.1.400"));
        assert_eq!(row.cost_usd, Some(0.002));
        let json = serde_json::to_string(&row).expect("serialize");
        assert!(!json.contains("private answer"), "{json}");
        assert!(!json.contains("Read: x"), "{json}");
        assert!(!json.contains("someone"), "{json}");
    }

    #[test]
    fn doctor_needs_the_provider_ready_and_records_its_version_number() {
        let report =
            br#"{"providers":[{"provider":"claude","ready":true,"version":"2.1.400 (Claude Code)"},
            {"provider":"codex","ready":false,"version":null}],
            "problems":[{"provider":"codex","blocks":"run","kind":"provider_not_found"}]}"#;
        let mut claude = row("claude");
        judge_doctor(&mut claude, report);
        assert!(claude.problems.is_empty(), "{:?}", claude.problems);
        assert_eq!(claude.provider_version.as_deref(), Some("claude 2.1.400"));
        let mut codex = row("codex");
        judge_doctor(&mut codex, report);
        assert_eq!(codex.problems.len(), 1);
        assert!(
            codex
                .notes
                .contains(&"1 problems that block a run".to_string())
        );
        let mut garbage = row("codex");
        judge_doctor(&mut garbage, b"not json");
        assert_eq!(garbage.problems.len(), 1);
    }

    #[test]
    fn only_a_progress_line_about_the_shell_tool_triggers_the_interrupt() {
        assert!(names_shell_tool("progress: assistant: tool_use: Bash"));
        assert!(names_shell_tool(
            "progress: item.started: command_execution in_progress"
        ));
        assert!(!names_shell_tool("progress: started claude (/x/claude)"));
        assert!(!names_shell_tool("delega run: denied: Bash: x"));
    }

    #[test]
    fn the_summary_fails_only_on_a_failed_case_and_says_when_no_cost_was_reported() {
        let mut a = row("claude");
        a.verdict = "pass".to_string();
        a.cost_usd = Some(0.01);
        a.runs = 1;
        let mut b = row("claude");
        b.verdict = "fail".to_string();
        b.cost_usd = Some(0.02);
        b.runs = 2;
        let mut c = row("codex");
        c.verdict = "not_shown".to_string();
        c.runs = 1;
        let (text, ok) = summary(&[a.clone(), b, c.clone()]);
        assert!(!ok);
        assert!(text.contains("1 pass, 1 fail, 1 not shown"), "{text}");
        assert!(text.contains("3 runs, reported cost $0.0300"), "{text}");
        assert!(text.contains("1 runs, no cost reported"), "{text}");
        assert!(!text.contains("-0.0"), "{text}");
        assert!(summary(&[a, c]).1);
    }

    #[test]
    fn the_output_file_pairs_metadata_and_rows_and_a_mismatch_is_refused() {
        let scratch = tempfile::tempdir().expect("scratch");
        let path = scratch.path().join("live.jsonl");
        let mut a = row("claude");
        a.provider_version = Some("claude 2.1.400".to_string());
        let rows = vec![a];
        let meta = serde_json::json!({"cases": 1, "providers": providers_of(&rows), "date": "d"});
        write_live(&path, &meta, &rows).expect("write");
        let text = std::fs::read_to_string(&path).expect("read");
        let (read_meta, read_rows) = read_live(&text).expect("pairs");
        assert_eq!(read_meta["date"], "d");
        assert_eq!(read_rows, rows);
        // No temporary file is left beside it.
        assert_eq!(std::fs::read_dir(scratch.path()).expect("dir").count(), 1);

        let wrong_count = serde_json::json!({"cases": 2, "providers": providers_of(&rows)});
        write_live(&path, &wrong_count, &rows).expect("write");
        assert!(read_live(&std::fs::read_to_string(&path).expect("read")).is_err());
        let wrong_version =
            serde_json::json!({"cases": 1, "providers": {"claude": "claude 9.9.9"}});
        write_live(&path, &wrong_version, &rows).expect("write");
        assert!(read_live(&std::fs::read_to_string(&path).expect("read")).is_err());
        let row_line = serde_json::to_string(&rows[0]).expect("row");
        assert!(read_live(&format!("{row_line}\n")).is_err());
    }

    /// A stand-in for `delega`: a shell script, so the waiting and the
    /// escalation are tested without a provider.
    fn script_suite(scratch: &Path, body: &str, stop: &'static AtomicBool) -> (Live, CaseDir) {
        let script = scratch.join("fake-delega.sh");
        std::fs::write(&script, format!("#!/bin/sh\n{body}\n")).expect("script");
        let made = Command::new("/bin/chmod")
            .arg("+x")
            .arg(&script)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .guarded_status()
            .expect("chmod");
        assert!(made.success());
        let root = std::fs::canonicalize(scratch).expect("root");
        let mut live = Live::new(
            script,
            root.clone(),
            Env::Fixed(vec![("PATH".into(), "/usr/bin:/bin".into())]),
        );
        live.stop = stop;
        let dir = CaseDir::new(&root).expect("case dir");
        (live, dir)
    }

    fn kill_group(pid: i32) {
        if let Some(pid) = rustix::process::Pid::from_raw(pid) {
            let _ = rustix::process::kill_process_group(pid, Signal::KILL);
        }
    }

    #[test]
    fn a_descendant_holding_standard_output_does_not_hold_up_the_case() {
        static NO_STOP: AtomicBool = AtomicBool::new(false);
        let scratch = tempfile::tempdir().expect("scratch");
        let (live, dir) = script_suite(scratch.path(), "sleep 60 &\necho '{}'", &NO_STOP);
        let began = Instant::now();
        let ran = live.invoke(&dir, &[], Duration::from_secs(30), false);
        kill_group(ran.pid);
        assert!(
            began.elapsed() < DRAIN + Duration::from_secs(5),
            "{:?}",
            began.elapsed()
        );
        assert_eq!(ran.exit_code, Some(0));
        assert_eq!(ran.stdout, b"{}\n");
        assert_eq!(ran.problems.len(), 1, "{:?}", ran.problems);
        assert!(ran.problems[0].contains("still open"), "{:?}", ran.problems);
    }

    #[test]
    fn a_stop_request_reaches_delega_as_sigint() {
        static STOP_LATER: AtomicBool = AtomicBool::new(false);
        let scratch = tempfile::tempdir().expect("scratch");
        let (live, dir) = script_suite(
            scratch.path(),
            "trap 'echo interrupted; exit 130' INT\n: > ready\nwhile :; do sleep 0.1; done",
            &STOP_LATER,
        );
        // The stop is requested once the stand-in has installed its trap, as
        // a maintainer's Ctrl-C would arrive during a run.
        let ready = dir.work.join("ready");
        let raiser = std::thread::spawn(move || {
            let until = Instant::now() + Duration::from_secs(10);
            while !ready.exists() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(10));
            }
            STOP_LATER.store(true, Ordering::SeqCst);
        });
        let began = Instant::now();
        let ran = live.invoke(&dir, &[], Duration::from_secs(30), false);
        raiser.join().expect("raiser");
        kill_group(ran.pid);
        assert!(began.elapsed() < Duration::from_secs(10));
        assert_eq!(ran.exit_code, Some(130));
        assert_eq!(ran.stdout, b"interrupted\n");
        assert!(ran.problems.is_empty(), "{:?}", ran.problems);
    }

    #[test]
    fn a_case_past_its_bound_starts_nothing() {
        static NO_STOP: AtomicBool = AtomicBool::new(false);
        let scratch = tempfile::tempdir().expect("scratch");
        let (live, dir) = script_suite(scratch.path(), "echo started", &NO_STOP);
        live.deadline.set(Some(Instant::now()));
        let ran = live.invoke(&dir, &[], Duration::from_secs(30), false);
        assert!(!ran.started);
        assert!(ran.stdout.is_empty());
        assert_eq!(ran.problems.len(), 1, "{:?}", ran.problems);
    }
}
