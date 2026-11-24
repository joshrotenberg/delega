//! Shared by the `delega run` tests.
//!
//! Every test runs the built `delega` binary against the fake providers that
//! the core crate keeps for its own tests, linked into `tests/fakes`. The binary
//! starts with an empty environment plus the few variables set here: a temporary
//! HOME, the fakes as the provider executables, a record directory the
//! fakes write into, and a `PATH` that starts with stand-ins for the tools
//! Claude Code's sandbox needs on Linux, so doctor's report does not depend on
//! the packages installed on the host. Nothing reads real provider state, the real home
//! directory or the network, and no real provider CLI is ever started.

#![allow(dead_code)]

use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

use delega_core::contract::result::RunResult;
use tempfile::TempDir;

/// std's `Command::spawn`, `output` and `status` under delega's spawn lock
/// (#107).
///
/// The tests of a binary run on concurrent threads. On macOS std creates a
/// child's pipes and sets close-on-exec on them afterwards, so a child one
/// test forks inside that window inherits another test's pipe ends: a
/// `delega mcp` whose standard input is also held by an unrelated child never
/// sees the end of its input. Each spawning method holds
/// `delega_core::process::spawn_guard()` for the spawn call only and drops it
/// before waiting or reading output; held longer, it would serialize every
/// test's subprocess for its whole life. The lock covers the pipes std creates
/// during that call; a pipe a test creates itself is covered only if it is
/// made through [`guarded_pipe`]. clippy's `disallowed_methods` (the workspace
/// `clippy.toml`) rejects a direct `spawn`, `output` or `status` call.
///
/// `guarded_output` never changes the standard streams. std's `output`
/// defaults standard input to null and captures both outputs only where the
/// caller set nothing, and std cannot tell which streams were set, so a helper
/// that applied those defaults would overwrite the streams a test set on
/// purpose (a closed pipe as standard output, say). Instead every command
/// says what it wants: [`Rig::configure`] and [`Guarded::captured`] (which
/// only sets streams and spawns nothing) set what `output` would have
/// defaulted to, and a test that wants something else sets it afterwards.
pub trait Guarded {
    /// Set the streams `output` defaults to: standard input null, standard
    /// output and standard error captured.
    fn captured(&mut self) -> &mut Self;
    /// `spawn` while holding the spawn lock.
    fn guarded_spawn(&mut self) -> io::Result<Child>;
    /// Spawn under the lock with the streams as set, then wait and collect
    /// what was captured, as `output` does.
    fn guarded_output(&mut self) -> io::Result<Output>;
    /// `status` with the lock held for the spawn only.
    fn guarded_status(&mut self) -> io::Result<ExitStatus>;
}

impl Guarded for Command {
    fn captured(&mut self) -> &mut Self {
        self.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
    }

    #[allow(clippy::disallowed_methods)]
    fn guarded_spawn(&mut self) -> io::Result<Child> {
        let _held = delega_core::process::spawn_guard();
        self.spawn()
    }

    fn guarded_output(&mut self) -> io::Result<Output> {
        self.guarded_spawn()?.wait_with_output()
    }

    fn guarded_status(&mut self) -> io::Result<ExitStatus> {
        self.guarded_spawn()?.wait()
    }
}

/// `std::io::pipe()` under the spawn lock (#107). std creates the pipe and
/// sets close-on-exec afterwards on macOS, the same window a spawn has, so a
/// child another test forks meanwhile could inherit an end: a reader would
/// then wait for that unrelated child, or a pipe whose reader was dropped
/// would not be closed.
#[allow(clippy::disallowed_methods)]
pub fn guarded_pipe() -> io::Result<(io::PipeReader, io::PipeWriter)> {
    let _held = delega_core::process::spawn_guard();
    io::pipe()
}

/// The provider names `--provider` accepts.
pub const PROVIDERS: [&str; 2] = ["claude", "codex"];

/// How long a test waits for something that should happen at once.
const WAIT_LIMIT: Duration = Duration::from_secs(120);
/// How often a wait polls.
const POLL: Duration = Duration::from_millis(10);

/// A private working area per test.
pub struct Rig {
    _tmp: TempDir,
    pub work: PathBuf,
    pub home: PathBuf,
    pub record: PathBuf,
}

/// The tools Claude Code's sandbox needs on Linux, which doctor looks for.
pub const SANDBOX_TOOLS: [&str; 2] = ["bwrap", "socat"];

/// The test-only variable that moves the managed settings lookup.
pub const SYSTEM_ROOT_ENV: &str = "DELEGA_TEST_SYSTEM_ROOT";

fn fake(provider: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fakes")
        .join(format!("fake-{provider}.sh"))
}

impl Rig {
    pub fn new() -> Self {
        let tmp = tempfile::tempdir().expect("create temporary directory");
        let root = std::fs::canonicalize(tmp.path()).expect("resolve temporary directory");
        let make = |name: &str| {
            let path = root.join(name);
            std::fs::create_dir(&path).expect("create directory");
            path
        };
        let tools = make("sandbox-tools");
        for name in SANDBOX_TOOLS {
            let path = tools.join(name);
            std::fs::write(&path, "#!/bin/sh\nexit 1\n").expect("write tool stand-in");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod tool stand-in");
        }
        Self {
            work: make("work"),
            home: make("home"),
            record: make("record"),
            _tmp: tmp,
        }
    }

    /// The directory of stand-ins for `bwrap` and `socat`, first on `PATH`.
    /// They exit 1 if run; doctor only looks for them.
    pub fn sandbox_tools(&self) -> PathBuf {
        self.path("sandbox-tools")
    }

    /// The `PATH` every command gets.
    pub fn path_value(&self) -> std::ffi::OsString {
        std::env::join_paths([
            self.sandbox_tools(),
            PathBuf::from("/usr/bin"),
            PathBuf::from("/bin"),
        ])
        .expect("join PATH")
    }

    /// A path inside the temporary area.
    pub fn path(&self, name: &str) -> PathBuf {
        self.work.parent().expect("root").join(name)
    }

    /// Write a file in the temporary area and return its path.
    pub fn file(&self, name: &str, content: &str) -> PathBuf {
        let path = self.path(name);
        std::fs::write(&path, content).expect("write file");
        path
    }

    /// `delega` with a controlled environment, no standard input and both
    /// outputs captured.
    pub fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_delega"));
        self.configure(&mut command);
        command
    }

    /// Give a command the controlled environment, whatever program it runs,
    /// and the standard streams `output()` would default to: no standard
    /// input, both outputs captured. [`Guarded::guarded_output`] keeps the
    /// streams as they are set, so they are set here; a caller that wants
    /// others sets them after this.
    pub fn configure(&self, command: &mut Command) {
        if !cfg!(debug_assertions) {
            panic!("CLI fixtures require debug system root injection");
        }
        let config = self.path("home");
        command.captured();
        command
            .env_clear()
            .env(SYSTEM_ROOT_ENV, self.system_root())
            .env("PATH", self.path_value())
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &config)
            .env("CLAUDE_CONFIG_DIR", &config)
            .env("CODEX_HOME", &config)
            .env("DELEGA_CLAUDE_BIN", fake("claude"))
            .env("DELEGA_CODEX_BIN", fake("codex"))
            .env("FAKE_RECORD_DIR", &self.record);
    }

    /// A temporary root for managed settings. A test that needs a managed
    /// source writes it below this directory.
    pub fn system_root(&self) -> PathBuf {
        self.path("system")
    }

    /// Run `delega` with these arguments to its end.
    pub fn delega(&self, args: &[&str]) -> Output {
        self.command()
            .args(args)
            .guarded_output()
            .expect("run delega")
    }

    /// The arguments of `delega run` for a provider, a prompt and the working
    /// directory, followed by `extra`. The record directory variable the fakes
    /// write into is passed by name, because workers do not inherit it.
    pub fn run_args(&self, provider: &str, prompt: &str, extra: &[&str]) -> Vec<String> {
        let mut args: Vec<String> = [
            "run",
            "--provider",
            provider,
            "-p",
            prompt,
            "-C",
            &self.work.to_string_lossy(),
            "--env",
            "FAKE_RECORD_DIR",
        ]
        .map(String::from)
        .to_vec();
        args.extend(extra.iter().map(ToString::to_string));
        args
    }

    /// `delega run` for a provider and a prompt, in `cwd` instead of the
    /// working directory the rig made, with `extra` flags.
    pub fn run_in(&self, cwd: &Path, provider: &str, prompt: &str, extra: &[&str]) -> Output {
        self.command()
            .args(["run", "--provider", provider, "-p", prompt, "-C"])
            .arg(cwd)
            .args(["--env", "FAKE_RECORD_DIR"])
            .args(extra)
            .guarded_output()
            .expect("run delega")
    }

    /// `delega run` for a provider and a prompt, with `extra` flags.
    pub fn run(&self, provider: &str, prompt: &str, extra: &[&str]) -> Output {
        self.command()
            .args(self.run_args(provider, prompt, extra))
            .guarded_output()
            .expect("run delega")
    }

    /// Like [`Rig::run`], in a rig of its own, again (in a fresh rig) while the
    /// run reports that its process listing was late: after a natural exit
    /// `delega` waits at most 3.1 s for that listing and says so when it is
    /// not done, which a machine under heavy load can cause. The test asserts
    /// exact output and an exact verdict for a run whose listing was on time,
    /// so it is given a run like that, up to eight attempts; a machine that is
    /// late eight times in a row fails the test with the last output.
    pub fn run_with_timely_listing(provider: &str, prompt: &str, extra: &[&str]) -> (Rig, Output) {
        let mut last = None;
        for _ in 0..8 {
            let rig = Rig::new();
            let out = rig.run(provider, prompt, extra);
            if !listing_was_late(&out) {
                return (rig, out);
            }
            last = Some((rig, out));
        }
        last.expect("at least one attempt")
    }

    /// Run `delega` with `input` on standard input.
    pub fn with_stdin(&self, args: &[String], input: &str) -> Output {
        let mut child = self
            .command()
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .guarded_spawn()
            .expect("spawn delega");
        let mut stdin = child.stdin.take().expect("stdin");
        let input = input.to_string();
        // On a thread, and errors ignored: delega may stop reading (an
        // oversize assignment) and close the pipe before all of it is written.
        std::thread::spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        });
        wait_bounded(child)
    }

    /// Start `delega run` and leave it running.
    pub fn spawn_run(&self, provider: &str, prompt: &str, extra: &[&str]) -> Child {
        self.command()
            .args(self.run_args(provider, prompt, extra))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .guarded_spawn()
            .expect("spawn delega")
    }

    /// What a fake recorded in `name`, or empty.
    pub fn recorded(&self, name: &str) -> String {
        std::fs::read_to_string(self.record.join(name)).unwrap_or_default()
    }

    /// Wait until the fake has signalled that it is running.
    pub fn wait_started(&self) {
        let began = Instant::now();
        while !self.record.join("started").exists() {
            assert!(began.elapsed() < WAIT_LIMIT, "the fake never started");
            std::thread::sleep(POLL);
        }
    }

    /// Assert that the fake, its background child and everything in the
    /// session it led are gone. The fake leads its own session and process
    /// group, so its pid is also the group id, and a process left in that
    /// group is found even when its pid was never recorded.
    pub fn assert_nothing_left(&self) {
        for name in ["pid", "child.pid"] {
            if let Ok(pid) = self.recorded(name).trim().parse::<i32>() {
                assert_gone(name, pid);
            }
        }
        if let Ok(leader) = self.recorded("pid").trim().parse::<i32>() {
            let began = Instant::now();
            while !group_members(leader).is_empty() {
                assert!(
                    began.elapsed() < WAIT_LIMIT,
                    "processes {:?} are still in the worker's group {leader}",
                    group_members(leader)
                );
                std::thread::sleep(POLL);
            }
        }
    }
}

/// Pids of live processes (zombies excluded) in process group `group`.
fn group_members(group: i32) -> Vec<i32> {
    let out = Command::new("/bin/ps")
        .args(["-ax", "-o", "pid=,pgid=,stat="])
        .captured()
        .guarded_output()
        .expect("run ps");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid: i32 = fields.next()?.parse().ok()?;
            let pgid: i32 = fields.next()?.parse().ok()?;
            let stat = fields.next()?;
            (pgid == group && !stat.starts_with('Z')).then_some(pid)
        })
        .collect()
}

/// Whether a process is alive. A zombie counts as gone.
pub fn is_running(pid: i32) -> bool {
    let out = Command::new("/bin/ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .captured()
        .guarded_output()
        .expect("run ps");
    let stat = String::from_utf8_lossy(&out.stdout);
    let stat = stat.trim();
    !stat.is_empty() && !stat.starts_with('Z')
}

/// Wait for a process to be gone, then assert it.
fn assert_gone(what: &str, pid: i32) {
    let began = Instant::now();
    while is_running(pid) {
        assert!(
            began.elapsed() < WAIT_LIMIT,
            "{what} (pid {pid}) is still running"
        );
        std::thread::sleep(POLL);
    }
}

/// Wait for a child to end and collect its output, killing it and failing the
/// test if it takes longer than [`WAIT_LIMIT`]. The pipes are read while
/// waiting, so a child that writes more than a pipe holds cannot block on its
/// own output and be mistaken for a hang.
pub fn wait_bounded(child: Child) -> Output {
    wait_bounded_for(child, WAIT_LIMIT)
}

/// [`wait_bounded`] with a limit of the caller's choosing.
pub fn wait_bounded_for(mut child: Child, limit: Duration) -> Output {
    let stdout = child.stdout.take().map(drain);
    let stderr = child.stderr.take().map(drain);
    let began = Instant::now();
    let status = loop {
        match child.try_wait().expect("poll child") {
            Some(status) => break status,
            None if began.elapsed() > limit => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("the child did not exit within {limit:?}");
            }
            None => std::thread::sleep(POLL),
        }
    };
    let collect = |handle: Option<std::thread::JoinHandle<Vec<u8>>>| {
        handle
            .map(|h| h.join().expect("reader thread"))
            .unwrap_or_default()
    };
    Output {
        status,
        stdout: collect(stdout),
        stderr: collect(stderr),
    }
}

fn drain(mut pipe: impl std::io::Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        bytes
    })
}

/// Wait for a child to exit without reading its pipes, for a test that needs
/// them left undrained. Kills it and fails the test past `limit`.
pub fn wait_exit_only(child: &mut Child, limit: Duration) -> std::process::ExitStatus {
    let began = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("poll child") {
            return status;
        }
        if began.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            let mut stderr = Vec::new();
            if let Some(mut pipe) = child.stderr.take() {
                let _ = std::io::Read::read_to_end(&mut pipe, &mut stderr);
            }
            let tail = &stderr[stderr.len().saturating_sub(2000)..];
            panic!(
                "the child did not exit within {limit:?}; the end of its stderr:\n{}",
                String::from_utf8_lossy(tail)
            );
        }
        std::thread::sleep(POLL);
    }
}

/// Wait for the acknowledgement file a test seam of `delega` creates at `path`
/// (`DELEGA_TEST_ACK_*`, debug builds only) once it has reached `what`. Fails
/// the test past [`WAIT_LIMIT`], or at once if `child` exits first, so a
/// `delega` that never reaches the state is a failure and not a long wait.
/// Before failing it kills and reaps `child`, so nothing is left running and
/// nothing later waits on it.
pub fn wait_for_ack(path: &Path, child: &mut Child, what: &str) {
    let began = Instant::now();
    let failure = loop {
        if path.exists() {
            return;
        }
        match child.try_wait() {
            // The acknowledgement is written on a thread of its own, so it
            // can land just after delega exits; look once more.
            Ok(Some(status)) => {
                std::thread::sleep(POLL);
                if path.exists() {
                    return;
                }
                break format!("delega exited ({status}) before it acknowledged that {what}");
            }
            Ok(None) if began.elapsed() >= WAIT_LIMIT => {
                break format!("delega did not acknowledge within {WAIT_LIMIT:?} that {what}");
            }
            Ok(None) => std::thread::sleep(POLL),
            Err(err) => break format!("cannot poll delega while waiting until {what}: {err}"),
        }
    };
    let _ = child.kill();
    let _ = child.wait();
    panic!("{failure}");
}

/// Send a signal by name (`INT`, `TERM`) to a process.
pub fn signal(pid: u32, name: &str) {
    let status = Command::new("/bin/kill")
        .args([format!("-{name}"), pid.to_string()])
        .guarded_status()
        .expect("run kill");
    assert!(status.success(), "kill -{name} {pid} failed");
}

/// Check the cleanup verdict of a run that was stopped by a signal or a time
/// limit. Such a stop waits at most 500 ms for the process listing that
/// precedes its first signal (`STOP_LISTING_BUDGET`); where `ps` takes longer,
/// which a loaded machine does, the stop is prompt and the documented verdict
/// is `Unconfirmed`, naming that listing. This binary cannot be given a longer
/// budget, so a test of the CLI accepts exactly those two answers: `Confirmed`,
/// or the late-listing verdict alone (`is_late_listing_only`: its exact
/// reason, no pids, no process groups). Any other verdict fails. That nothing is
/// left running is checked separately (`Rig::assert_nothing_left`), and the
/// exact verdict after a stop is asserted by the provider-layer tests, which
/// run with a listing budget no load can exhaust.
pub fn assert_stop_cleanup(result: &RunResult, what: &str) {
    use delega_core::contract::result::Cleanup;
    use delega_core::process::is_late_listing_only;
    if result.cleanup != Cleanup::Confirmed && !is_late_listing_only(&result.cleanup) {
        panic!(
            "{what}: unexpected cleanup verdict after a stop: {:?}",
            result.cleanup
        );
    }
}

/// Whether a run's output says its cleanup was unconfirmed for the late process
/// listing and for nothing else: the result document on standard output when
/// there is one, otherwise the exact line text mode prints on standard error.
fn listing_was_late(out: &Output) -> bool {
    use delega_core::contract::result::Cleanup;
    use delega_core::process::is_late_listing_only;
    if let Ok(result) = serde_json::from_slice::<RunResult>(&out.stdout) {
        return is_late_listing_only(&result.cleanup);
    }
    // Text mode prints the reason with no pids and no process groups; the
    // reason may be the late listing any number of times, as in the document.
    text(&out.stderr).lines().any(|l| {
        l.strip_prefix("delega run: cleanup not confirmed: ")
            .and_then(|rest| rest.strip_suffix(" (pids [], process groups [])"))
            .is_some_and(|reason| {
                is_late_listing_only(&Cleanup::Unconfirmed {
                    pids: Vec::new(),
                    process_groups: Vec::new(),
                    reason: reason.to_owned(),
                })
            })
    })
}

pub fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("utf-8 output")
}

/// Read standard output as a result, and check it does not contradict itself.
pub fn result_of(out: &Output) -> RunResult {
    let result: RunResult = serde_json::from_slice(&out.stdout).unwrap_or_else(|err| {
        panic!(
            "standard output is not a result: {err}\nstdout: {}\nstderr: {}",
            text(&out.stdout),
            text(&out.stderr)
        )
    });
    assert_eq!(result.validate(), Ok(()), "{result:#?}");
    result
}

/// The fixture path for Claude Code's managed MCP file on this platform.
pub fn managed_mcp_path(root: &Path) -> PathBuf {
    root.join(if cfg!(target_os = "macos") {
        "Library/Application Support/ClaudeCode/managed-mcp.json"
    } else {
        "etc/claude-code/managed-mcp.json"
    })
}
