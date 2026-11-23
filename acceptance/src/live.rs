//! The live suite: a few cases per provider against the installed `claude`
//! and `codex`, through the built `delega` binary. See the "Live suite"
//! section of `acceptance/README.md`.
//!
//! Nothing here runs unless `delega-acceptance --live` is given. Each case
//! gets a fresh directory under the harness's temporary root, removed when the
//! case ends, and every `delega` it starts has a time limit and a harness
//! bound past it, so the suite always ends. What it prints and records is
//! names, outcomes, counts, versions and the test words it seeded itself:
//! never a model's answer, the content of another file or an environment
//! value.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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
/// The bound on `delega doctor`.
const DOCTOR_BOUND: Duration = Duration::from_secs(60);
/// The interrupt case sends SIGINT this long after the progress output names
/// the shell tool, so the command it started is running when the signal lands.
const AFTER_TOOL: Duration = Duration::from_secs(2);
/// The interrupt case sends SIGINT at this point whether or not the shell tool
/// was seen.
const INTERRUPT_FALLBACK: Duration = Duration::from_secs(30);
/// Prefix of every case directory.
pub const DIR_PREFIX: &str = "dlive-";

/// One case of the suite. Every case runs for each selected provider.
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

/// The cases to run: every case for the named provider, or for both.
pub fn plan(provider: Option<&str>) -> Result<Vec<Planned>, String> {
    let providers: Vec<&'static str> = match provider {
        None => PROVIDERS.to_vec(),
        Some(name) => vec![
            *PROVIDERS
                .iter()
                .find(|p| **p == name)
                .ok_or_else(|| format!("unknown provider {name:?}; give claude or codex"))?,
        ],
    };
    Ok(providers
        .into_iter()
        .flat_map(|provider| Case::ALL.map(|case| Planned { provider, case }))
        .collect())
}

/// What one case found. One JSON line of `--live --out`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LiveRow {
    pub provider: String,
    pub case: String,
    /// `pass` or `fail`.
    pub verdict: String,
    /// `delega run` invocations the case made.
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
    /// Distinguishes this run's test words and file names from another run's.
    pub run_id: String,
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

impl Live {
    pub fn new(delega: PathBuf, root: PathBuf, env: Env) -> Self {
        let run_id = format!(
            "{}{}",
            std::process::id() % 1000,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs() % 10_000)
        );
        Self {
            delega,
            root,
            env,
            run_id,
        }
    }

    /// The test word for a case: a fixed stem and this run's digits, so an
    /// answer from an earlier run or another case cannot match.
    pub fn word(&self, stem: &str) -> String {
        format!("{stem}{}", self.run_id)
    }

    /// Run one case and return what it found. The case directory is removed
    /// before this returns.
    pub fn run(&self, planned: Planned) -> LiveRow {
        let began = Instant::now();
        let mut row = LiveRow {
            provider: planned.provider.to_string(),
            case: planned.case.name().to_string(),
            ..LiveRow::default()
        };
        match CaseDir::new(&self.root) {
            Ok(dir) => self.run_in(&dir, planned, &mut row),
            Err(err) => row
                .problems
                .push(format!("could not set up the case directory: {err}")),
        }
        row.elapsed_ms = u64::try_from(began.elapsed().as_millis()).unwrap_or(u64::MAX);
        row.verdict = if row.problems.is_empty() {
            "pass"
        } else {
            "fail"
        }
        .to_string();
        row
    }

    fn run_in(&self, dir: &CaseDir, planned: Planned, row: &mut LiveRow) {
        let provider = planned.provider;
        match planned.case {
            Case::Doctor => {
                match self.invoke(
                    dir,
                    &os(&["doctor", "--output", "json"]),
                    DOCTOR_BOUND,
                    false,
                ) {
                    Ok(ran) => {
                        if ran.hang {
                            row.problems.push(hang_problem(DOCTOR_BOUND));
                        }
                        row.exit_code = ran.exit_code;
                        row.exit_signal = ran.exit_signal;
                        judge_doctor(row, &ran.stdout);
                    }
                    Err(err) => row.problems.push(err),
                }
            }
            Case::Plain => {
                if let Some(r) = self.one_run(dir, row, provider, "read", PLAIN_PROMPT, &[]) {
                    judge_plain(row, &r);
                }
            }
            Case::ReadCwd => {
                let word = self.word("lantern");
                if let Err(err) = std::fs::write(dir.work.join("word.txt"), format!("{word}\n")) {
                    row.problems.push(format!("could not seed word.txt: {err}"));
                    return;
                }
                if let Some(r) = self.one_run(dir, row, provider, "read", READ_CWD_PROMPT, &[]) {
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
                if let Some(r) = self.one_run(dir, row, provider, "read", WRITE_PROMPT, &[]) {
                    let unchanged = std::fs::read_to_string(&note).is_ok_and(|t| t == seeded);
                    judge_write_refused(row, &r, unchanged);
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
                if let Some(r) = self.one_run(dir, row, provider, "read", &prompt, &[]) {
                    judge_read_outside(row, provider, &r, &word);
                }
            }
            Case::Build => {
                let name = format!("{DIR_PREFIX}probe-{}", self.run_id);
                if let Err(err) =
                    std::fs::write(dir.work.join("probe.sh"), probe_script(&name, &dir.outside))
                {
                    row.problems.push(format!("could not seed probe.sh: {err}"));
                    return;
                }
                let result = self.one_run(dir, row, provider, "build", BUILD_PROMPT, &[]);
                let probe = BuildProbe::observe(dir, &name);
                if let Some(r) = result {
                    judge_build(row, provider, &r, &probe);
                }
                probe.remove_tmp_file();
            }
            Case::Resume => {
                let word = self.word("harbor");
                let first_prompt =
                    format!("Remember the word {word}. Reply with only the word noted.");
                let Some(first) = self.one_run_kept(dir, row, provider, &first_prompt, None) else {
                    return;
                };
                let Some(sid) = first.session_id.clone() else {
                    row.problems
                        .push("the first run reported no session id to resume".to_string());
                    return;
                };
                if let Some(second) =
                    self.one_run_kept(dir, row, provider, RESUME_PROMPT, Some(&sid))
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
                let bound = Duration::from_secs(RUN_TIMEOUT_SECS) + GRACE;
                match self.invoke(dir, &args, bound, true) {
                    Ok(ran) => {
                        row.runs += 1;
                        if let Some(r) = take_result(row, &ran, bound) {
                            match ran.after_tool {
                                Some(true) => row.notes.push(
                                    "SIGINT was sent 2 s after the progress output named the shell tool"
                                        .to_string(),
                                ),
                                Some(false) => row.notes.push(
                                    "the shell tool was not seen; SIGINT was sent 30 s after the start"
                                        .to_string(),
                                ),
                                None => row.notes.push(
                                    "delega ended before the harness sent SIGINT".to_string(),
                                ),
                            }
                            judge_interrupt(row, &r);
                        }
                    }
                    Err(err) => row.problems.push(err),
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
                let bound = Duration::from_secs(SHORT_TIMEOUT_SECS) + GRACE;
                match self.invoke(dir, &args, bound, false) {
                    Ok(ran) => {
                        row.runs += 1;
                        if let Some(r) = take_result(row, &ran, bound) {
                            judge_timeout(row, &r);
                        }
                    }
                    Err(err) => row.problems.push(err),
                }
            }
        }
    }

    /// One ephemeral run with the default time limit.
    fn one_run(
        &self,
        dir: &CaseDir,
        row: &mut LiveRow,
        provider: &str,
        mode: &str,
        prompt: &str,
        extra: &[&str],
    ) -> Option<RunResult> {
        let args = run_args(
            provider,
            mode,
            prompt,
            &dir.work,
            RUN_TIMEOUT_SECS,
            true,
            None,
            extra,
        );
        let bound = Duration::from_secs(RUN_TIMEOUT_SECS) + GRACE;
        match self.invoke(dir, &args, bound, false) {
            Ok(ran) => {
                row.runs += 1;
                take_result(row, &ran, bound)
            }
            Err(err) => {
                row.problems.push(err);
                None
            }
        }
    }

    /// A run whose session is kept, so it can be resumed, in `read` mode.
    fn one_run_kept(
        &self,
        dir: &CaseDir,
        row: &mut LiveRow,
        provider: &str,
        prompt: &str,
        resume: Option<&str>,
    ) -> Option<RunResult> {
        let args = run_args(
            provider,
            "read",
            prompt,
            &dir.work,
            RUN_TIMEOUT_SECS,
            false,
            resume,
            &[],
        );
        let bound = Duration::from_secs(RUN_TIMEOUT_SECS) + GRACE;
        match self.invoke(dir, &args, bound, false) {
            Ok(ran) => {
                row.runs += 1;
                take_result(row, &ran, bound)
            }
            Err(err) => {
                row.problems.push(err);
                None
            }
        }
    }

    /// Start `delega` with `args` in the case's working directory and wait
    /// for it, at most `bound` and then SIGTERM, and SIGKILL after
    /// [`KILL_AFTER`]. With `interrupt`, send SIGINT once the shell tool has
    /// been running for [`AFTER_TOOL`], or at [`INTERRUPT_FALLBACK`].
    fn invoke(
        &self,
        dir: &CaseDir,
        args: &[OsString],
        bound: Duration,
        interrupt: bool,
    ) -> Result<Ran, String> {
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
        let mut child = command
            .guarded_spawn()
            .map_err(|e| format!("could not start delega: {e}"))?;
        let started = Instant::now();
        let pid = i32::try_from(child.id()).unwrap_or(0);
        let stdout = child.stdout.take().map(|mut pipe| {
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let _ = pipe.read_to_end(&mut bytes);
                bytes
            })
        });
        // Standard error is read line by line and dropped. Only whether a
        // progress line names the shell tool is kept.
        let tool_seen: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
        let stderr = child.stderr.take().map(|pipe| {
            let seen = Arc::clone(&tool_seen);
            std::thread::spawn(move || {
                for line in BufReader::new(pipe).lines() {
                    let Ok(line) = line else { break };
                    if names_shell_tool(&line)
                        && let Ok(mut at) = seen.lock()
                    {
                        at.get_or_insert_with(Instant::now);
                    }
                }
            })
        });
        let mut ran = Ran::default();
        let mut termed: Option<Instant> = None;
        let mut killed = false;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    ran.exit_code = status.code();
                    ran.exit_signal = status.signal();
                    break;
                }
                Ok(None) => {}
                Err(err) => return Err(format!("could not wait for delega: {err}")),
            }
            let elapsed = started.elapsed();
            if interrupt && ran.after_tool.is_none() {
                let seen = tool_seen.lock().ok().and_then(|at| *at);
                if seen.is_some_and(|at| at.elapsed() >= AFTER_TOOL) {
                    procs::signal(pid, Signal::INT);
                    ran.after_tool = Some(true);
                } else if elapsed >= INTERRUPT_FALLBACK {
                    procs::signal(pid, Signal::INT);
                    ran.after_tool = Some(false);
                }
            }
            if termed.is_none() && elapsed >= bound {
                ran.hang = true;
                procs::signal(pid, Signal::TERM);
                termed = Some(Instant::now());
            }
            if !killed && termed.is_some_and(|at| at.elapsed() >= KILL_AFTER) {
                procs::signal(pid, Signal::KILL);
                killed = true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        ran.stdout = stdout.and_then(|h| h.join().ok()).unwrap_or_default();
        if let Some(h) = stderr {
            let _ = h.join();
        }
        Ok(ran)
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
/// its end.
pub fn probe_script(name: &str, outside: &Path) -> String {
    format!(
        "touch made-in-cwd\n\
         t=\"${{TMPDIR:-/tmp}}/{name}\"\n\
         if touch \"$t\"; then printf '%s\\n' \"$t\" > tmp-path; fi\n\
         touch '{outside}/written' 2>/dev/null\n\
         if /usr/bin/curl -s -m 5 -o /dev/null https://example.com 2>/dev/null; then touch net-reached; fi\n\
         touch probe-done\n",
        outside = outside.display()
    )
}

/// What a `delega` invocation left.
#[derive(Default)]
struct Ran {
    exit_code: Option<i32>,
    exit_signal: Option<i32>,
    stdout: Vec<u8>,
    /// The harness had to stop it.
    hang: bool,
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

fn hang_problem(bound: Duration) -> String {
    format!(
        "delega had not ended {} s after it started; the harness stopped it",
        bound.as_secs()
    )
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
/// valid result, an exit status that follows its outcome, and cleanup
/// confirmed.
fn take_result(row: &mut LiveRow, ran: &Ran, bound: Duration) -> Option<RunResult> {
    row.exit_code = ran.exit_code;
    row.exit_signal = ran.exit_signal;
    if ran.hang {
        row.problems.push(hang_problem(bound));
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
    Some(r)
}

/// Copy the reportable fields of a result into the row. Never the answer text.
pub fn record(row: &mut LiveRow, r: &RunResult) {
    row.outcome = Some(outcome_name(r));
    row.cleanup_confirmed = Some(matches!(r.cleanup, Cleanup::Confirmed));
    row.denied = r.permissions.denied.len();
    row.wider_note = r
        .provider_notes
        .iter()
        .any(|n| n.starts_with("wider than requested"));
    row.error_kind = r.error.as_ref().map(|e| e.kind.clone());
    if let Some(v) = &r.provenance.provider_version {
        row.provider_version = Some(v.clone());
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

/// `read` gives Claude Code no `Edit` or `Write` tool and runs Codex read-only,
/// so the file must be unchanged. A denial is recorded, not required: a model
/// that is offered no write tool has nothing to be denied.
pub fn judge_write_refused(row: &mut LiveRow, r: &RunResult, unchanged: bool) {
    check_outcome(row, r, &["completed", "refused"]);
    check_cleanup(row, r);
    check(row, unchanged, "the seeded file is unchanged");
    row.notes
        .push(format!("{} denials listed", r.permissions.denied.len()));
}

/// Claude Code scopes `Read` to the working directory, so the read is denied.
/// Codex does not confine reads, so its run must say it is wider than
/// requested, and whether the word came back is recorded, not asserted.
pub fn judge_read_outside(row: &mut LiveRow, provider: &str, r: &RunResult, word: &str) {
    if provider == "claude" {
        check_outcome(row, r, &["completed", "refused"]);
        check_cleanup(row, r);
        check(
            row,
            !contains_word(&r.text, word),
            "the answer does not have the word seeded outside",
        );
        check(
            row,
            !r.permissions.denied.is_empty(),
            "permissions.denied lists the read",
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
    pub tmp_file: Option<(PathBuf, TmpPlace)>,
    pub outside_file: bool,
    pub net_reached: bool,
    pub curl_available: bool,
    /// The outside directory lies under `/tmp`, which Codex's sandbox lets
    /// commands write.
    pub outside_under_tmp: bool,
}

impl BuildProbe {
    fn observe(dir: &CaseDir, name: &str) -> Self {
        let tmp_file = std::fs::read_to_string(dir.work.join("tmp-path"))
            .ok()
            .map(|t| PathBuf::from(t.trim()))
            .filter(|p| p.is_absolute() && p.file_name().is_some_and(|n| n == name) && p.is_file())
            .map(|p| {
                let place = classify_tmp(&p, &dir.tmp, &dir.work);
                (p, place)
            });
        Self {
            done: dir.work.join("probe-done").exists(),
            cwd_file: dir.work.join("made-in-cwd").exists(),
            tmp_file,
            outside_file: dir.outside.join("written").exists(),
            net_reached: dir.work.join("net-reached").exists(),
            curl_available: Path::new("/usr/bin/curl").exists(),
            outside_under_tmp: under_slash_tmp(&dir.outside),
        }
    }

    /// Remove the file the probe made in a temporary directory outside the
    /// case directory. Only that file: its name is unique to this run.
    fn remove_tmp_file(&self) {
        if let Some((path, place)) = &self.tmp_file
            && !matches!(place, TmpPlace::Harness | TmpPlace::Work)
        {
            let _ = std::fs::remove_file(path);
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
    match &probe.tmp_file {
        Some((_, TmpPlace::Work)) | None => row
            .problems
            .push("not so: a command wrote a file in the temporary directory".to_string()),
        Some((_, place)) => {
            row.checks
                .push("a command wrote a file in the temporary directory".to_string());
            row.notes
                .push(format!("the temporary file was in {}", place.label()));
        }
    }
    if provider == "codex" && probe.outside_under_tmp {
        row.notes.push(
            "the outside directory is under /tmp, which Codex's build sandbox lets commands write, so the outside write was not asserted"
                .to_string(),
        );
    } else {
        check(
            row,
            !probe.outside_file,
            "a write to a directory outside the working and temporary directories was refused",
        );
    }
    if probe.curl_available {
        check(row, !probe.net_reached, "the network was off");
    } else {
        row.notes
            .push("/usr/bin/curl is missing, so the network was not probed".to_string());
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

pub fn judge_interrupt(row: &mut LiveRow, r: &RunResult) {
    check_outcome(row, r, &["interrupted"]);
    check_cleanup(row, r);
    check(row, row.exit_code == Some(130), "exit status 130");
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

/// `delega doctor --output json`: the provider is ready, and its version is
/// recorded.
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
    row.provider_version = entry["version"].as_str().map(str::to_string);
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
        "{:<6} {:<13} {:<4} {:<11} {:>6.1} s",
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
    out
}

/// The summary lines and whether every case passed.
pub fn summary(rows: &[LiveRow]) -> (String, bool) {
    let passed = rows.iter().filter(|r| r.verdict == "pass").count();
    let failed = rows.len() - passed;
    let mut out = format!("{} live cases: {passed} pass, {failed} fail\n", rows.len());
    for provider in PROVIDERS {
        let mine: Vec<&LiveRow> = rows.iter().filter(|r| r.provider == provider).collect();
        if mine.is_empty() {
            continue;
        }
        let version = mine
            .iter()
            .find_map(|r| r.provider_version.clone())
            .unwrap_or_else(|| "version unknown".to_string());
        let cost: f64 = mine.iter().filter_map(|r| r.cost_usd).sum();
        let input: u64 = mine.iter().filter_map(|r| r.input_tokens).sum();
        let output: u64 = mine.iter().filter_map(|r| r.output_tokens).sum();
        let runs: u32 = mine.iter().map(|r| r.runs).sum();
        out.push_str(&format!(
            "{provider} ({version}): {runs} runs, reported cost ${cost:.4}, {input} input and {output} output tokens reported\n"
        ));
    }
    (out, failed == 0)
}

/// What `--out` writes next to the rows, as `<out>.meta.json`.
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
    serde_json::json!({
        "date": run("date", &["-u", "+%Y-%m-%d"]),
        "os": run("uname", &["-srm"]),
        "delega_version": version,
        "harness_commit": run("git", &["-C", &crate::delega_bin::workspace_root().to_string_lossy(), "rev-parse", "--short", "HEAD"]),
        "providers": providers,
        "claude_model": CLAUDE_MODEL,
        "codex_effort": CODEX_EFFORT,
        "wall_secs": wall.as_secs(),
    })
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

    fn denied(mut r: RunResult) -> RunResult {
        r.permissions
            .denied
            .push(delega_core::contract::result::DeniedRequest {
                request: "Read: x".to_string(),
            });
        r
    }

    #[test]
    fn plan_runs_every_case_for_both_providers_unless_one_is_named() {
        let both = plan(None).expect("plan");
        assert_eq!(both.len(), 2 * Case::ALL.len());
        assert!(
            both[..Case::ALL.len()]
                .iter()
                .all(|p| p.provider == "claude")
        );
        let codex = plan(Some("codex")).expect("plan");
        assert_eq!(codex.len(), Case::ALL.len());
        assert!(codex.iter().all(|p| p.provider == "codex"));
        assert_eq!(codex[0].id(), "codex-doctor");
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
    fn a_recorded_completed_result_passes_the_plain_case_and_an_empty_answer_fails_it() {
        let mut ok = row("claude");
        judge_plain(&mut ok, &fixture("claude-completed"));
        assert!(ok.problems.is_empty(), "{:?}", ok.problems);
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
    fn a_changed_file_fails_the_write_case_whatever_the_outcome() {
        let mut ok = row("claude");
        judge_write_refused(&mut ok, &fixture("claude-completed"), true);
        assert!(ok.problems.is_empty(), "{:?}", ok.problems);
        let mut bad = row("claude");
        judge_write_refused(&mut bad, &fixture("claude-completed"), false);
        assert_eq!(bad.problems, vec!["not so: the seeded file is unchanged"]);
    }

    #[test]
    fn claude_must_deny_the_outside_read_and_codex_must_say_it_is_wider() {
        let word = "thistle7";
        let mut ok = row("claude");
        judge_read_outside(
            &mut ok,
            "claude",
            &denied(fixture("claude-completed")),
            word,
        );
        assert!(ok.problems.is_empty(), "{:?}", ok.problems);
        let mut no_denial = row("claude");
        judge_read_outside(&mut no_denial, "claude", &fixture("claude-completed"), word);
        assert_eq!(no_denial.problems.len(), 1, "{:?}", no_denial.problems);
        let mut leaked = row("claude");
        let r = denied(with_text(fixture("claude-completed"), "Thistle7"));
        judge_read_outside(&mut leaked, "claude", &r, word);
        assert_eq!(leaked.problems.len(), 1, "{:?}", leaked.problems);

        // Codex reads anywhere: the word coming back is a note, not a failure.
        let mut codex = row("codex");
        let r = with_text(fixture("codex-completed"), "thistle7");
        judge_read_outside(&mut codex, "codex", &r, word);
        assert!(codex.problems.is_empty(), "{:?}", codex.problems);
        assert!(codex.notes.iter().any(|n| n.contains("has the word")));
        let mut no_note = row("codex");
        let mut r = fixture("codex-completed");
        r.provider_notes
            .retain(|n| !n.starts_with("wider than requested"));
        judge_read_outside(&mut no_note, "codex", &r, word);
        assert_eq!(no_note.problems.len(), 1, "{:?}", no_note.problems);
    }

    fn good_probe() -> BuildProbe {
        BuildProbe {
            done: true,
            cwd_file: true,
            tmp_file: Some((PathBuf::from("/tmp/claude-501/x"), TmpPlace::ClaudeUser)),
            outside_file: false,
            net_reached: false,
            curl_available: true,
            outside_under_tmp: false,
        }
    }

    #[test]
    fn the_build_case_fails_on_an_outside_write_a_reached_network_or_a_script_cut_short() {
        let r = fixture("claude-completed");
        let mut ok = row("claude");
        judge_build(&mut ok, "claude", &r, &good_probe());
        assert!(ok.problems.is_empty(), "{:?}", ok.problems);
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
                    tmp_file: Some((PathBuf::from("/w/x"), TmpPlace::Work)),
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
    fn an_outside_directory_under_slash_tmp_is_not_asserted_for_codex_only() {
        let probe = BuildProbe {
            outside_file: true,
            outside_under_tmp: true,
            ..good_probe()
        };
        let mut codex = row("codex");
        judge_build(&mut codex, "codex", &fixture("codex-completed"), &probe);
        assert!(codex.problems.is_empty(), "{:?}", codex.problems);
        let mut claude = row("claude");
        judge_build(&mut claude, "claude", &fixture("claude-completed"), &probe);
        assert_eq!(claude.problems.len(), 1);
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
    fn the_probe_script_writes_outside_where_it_was_told() {
        let script = probe_script("dlive-probe-1", Path::new("/v/dlive-1/outside"));
        assert!(
            script.contains("touch '/v/dlive-1/outside/written'"),
            "{script}"
        );
        assert!(script.contains("${TMPDIR:-/tmp}/dlive-probe-1"), "{script}");
        assert!(script.ends_with("touch probe-done\n"));
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
    fn interrupt_and_timeout_need_their_outcome_exit_status_and_confirmed_cleanup() {
        let mut ok = row("codex");
        ok.exit_code = Some(130);
        judge_interrupt(&mut ok, &fixture("codex-interrupted"));
        assert!(ok.problems.is_empty(), "{:?}", ok.problems);
        let mut completed = row("codex");
        completed.exit_code = Some(0);
        judge_interrupt(&mut completed, &fixture("codex-completed"));
        assert_eq!(completed.problems.len(), 2, "{:?}", completed.problems);

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
    fn the_row_records_counts_and_versions_and_never_the_answer() {
        let mut r = with_text(fixture("claude-completed"), "a private answer");
        r = denied(r);
        let mut row = row("claude");
        record(&mut row, &r);
        record(&mut row, &r);
        assert_eq!(row.denied, 1);
        assert_eq!(
            row.provider_version.as_deref(),
            Some("2.1.400 (Claude Code)")
        );
        assert_eq!(row.cost_usd, Some(0.002));
        let json = serde_json::to_string(&row).expect("serialize");
        assert!(!json.contains("private answer"), "{json}");
        assert!(!json.contains("Read: x"), "{json}");
    }

    #[test]
    fn doctor_needs_the_provider_ready_and_records_its_version() {
        let report =
            br#"{"providers":[{"provider":"claude","ready":true,"version":"2.1.400 (Claude Code)"},
            {"provider":"codex","ready":false,"version":null}],
            "problems":[{"provider":"codex","blocks":"run","kind":"provider_not_found"}]}"#;
        let mut claude = row("claude");
        judge_doctor(&mut claude, report);
        assert!(claude.problems.is_empty(), "{:?}", claude.problems);
        assert_eq!(
            claude.provider_version.as_deref(),
            Some("2.1.400 (Claude Code)")
        );
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
    fn the_summary_fails_when_any_case_fails_and_sums_reported_cost() {
        let mut a = row("claude");
        a.verdict = "pass".to_string();
        a.cost_usd = Some(0.01);
        a.runs = 1;
        let mut b = row("claude");
        b.verdict = "fail".to_string();
        b.cost_usd = Some(0.02);
        b.runs = 2;
        let (text, ok) = summary(&[a.clone(), b]);
        assert!(!ok);
        assert!(text.contains("1 pass, 1 fail"), "{text}");
        assert!(text.contains("3 runs, reported cost $0.0300"), "{text}");
        assert!(summary(&[a]).1);
    }
}
