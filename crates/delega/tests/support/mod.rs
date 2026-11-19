//! Shared by the `delega run` tests.
//!
//! Every test runs the built `delega` binary against the fake providers that
//! the core crate keeps for its own tests (`crates/delega-core/src/provider/
//! tests/fakes`), referenced by path so there is one copy. The binary starts
//! with an empty environment plus the few variables set here: a temporary
//! HOME, the fakes as the provider executables, and a record directory the
//! fakes write into. Nothing reads real provider state, the real home
//! directory or the network, and no real provider CLI is ever started.

#![allow(dead_code)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use delega_core::contract::result::RunResult;
use tempfile::TempDir;

/// The provider names `--provider` accepts.
pub const PROVIDERS: [&str; 2] = ["claude", "codex"];

/// How long a test waits for something that should happen at once.
const WAIT_LIMIT: Duration = Duration::from_secs(30);
/// How often a wait polls.
const POLL: Duration = Duration::from_millis(10);

/// A private working area per test.
pub struct Rig {
    _tmp: TempDir,
    pub work: PathBuf,
    pub home: PathBuf,
    pub record: PathBuf,
}

fn fake(provider: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../delega-core/src/provider/tests/fakes")
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
        Self {
            work: make("work"),
            home: make("home"),
            record: make("record"),
            _tmp: tmp,
        }
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

    /// `delega` with a controlled environment and no standard input.
    pub fn command(&self) -> Command {
        let config = self.path("home");
        let mut command = Command::new(env!("CARGO_BIN_EXE_delega"));
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &config)
            .env("CLAUDE_CONFIG_DIR", &config)
            .env("CODEX_HOME", &config)
            .env("DELEGA_CLAUDE_BIN", fake("claude"))
            .env("DELEGA_CODEX_BIN", fake("codex"))
            .env("FAKE_RECORD_DIR", &self.record)
            .stdin(Stdio::null());
        command
    }

    /// Run `delega` with these arguments to its end.
    pub fn delega(&self, args: &[&str]) -> Output {
        self.command().args(args).output().expect("run delega")
    }

    /// The arguments of `delega run` for a provider, a prompt and the working
    /// directory, followed by `extra`.
    pub fn run_args(&self, provider: &str, prompt: &str, extra: &[&str]) -> Vec<String> {
        let mut args: Vec<String> = [
            "run",
            "--provider",
            provider,
            "-p",
            prompt,
            "-C",
            &self.work.to_string_lossy(),
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
            .args(extra)
            .output()
            .expect("run delega")
    }

    /// `delega run` for a provider and a prompt, with `extra` flags.
    pub fn run(&self, provider: &str, prompt: &str, extra: &[&str]) -> Output {
        self.command()
            .args(self.run_args(provider, prompt, extra))
            .output()
            .expect("run delega")
    }

    /// Run `delega` with `input` on standard input.
    pub fn with_stdin(&self, args: &[String], input: &str) -> Output {
        let mut child = self
            .command()
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn delega");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(input.as_bytes())
            .expect("write stdin");
        wait_bounded(child)
    }

    /// Start `delega run` and leave it running.
    pub fn spawn_run(&self, provider: &str, prompt: &str, extra: &[&str]) -> Child {
        self.command()
            .args(self.run_args(provider, prompt, extra))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
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

    /// Assert that the fake and its background child, when they started, are
    /// gone.
    pub fn assert_nothing_left(&self) {
        for name in ["pid", "child.pid"] {
            if let Ok(pid) = self.recorded(name).trim().parse::<i32>() {
                assert_gone(name, pid);
            }
        }
    }
}

/// Whether a process is alive. A zombie counts as gone.
fn is_running(pid: i32) -> bool {
    let out = Command::new("/bin/ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
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

/// Wait for a child to end, killing it and failing the test if it takes too
/// long, so a hang cannot hold a test run.
pub fn wait_bounded(mut child: Child) -> Output {
    let began = Instant::now();
    loop {
        match child.try_wait().expect("poll delega") {
            Some(_) => return child.wait_with_output().expect("collect output"),
            None if began.elapsed() > WAIT_LIMIT => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("delega did not exit within {WAIT_LIMIT:?}");
            }
            None => std::thread::sleep(POLL),
        }
    }
}

/// Send a signal by name (`INT`, `TERM`) to a process.
pub fn signal(pid: u32, name: &str) {
    let status = Command::new("/bin/kill")
        .args([format!("-{name}"), pid.to_string()])
        .status()
        .expect("run kill");
    assert!(status.success(), "kill -{name} {pid} failed");
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
