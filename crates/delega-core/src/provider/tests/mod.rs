//! Tests for the provider layer, against fake provider executables.
//!
//! The fakes are `/bin/sh` scripts in `tests/fakes`. Every test runs them with
//! a temporary home and configuration directory and a parent environment it
//! builds itself, so no test reads or writes real provider state or the real
//! environment. Every test that starts a process checks that the process and
//! its descendant are gone.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use tempfile::TempDir;

use crate::contract::request::{Provider, Request};
use crate::contract::result::RunResult;
use crate::provider::{ProviderOptions, run};
use crate::test_spawn::Guarded;

mod applied;
mod deny;
mod dry;
mod enforce;
mod env;
mod excerpt;
mod hook;
mod injection;
mod managed;
mod mapping;
mod native;
mod outcomes;
mod prepare;
mod progress;
mod rules;
mod sandbox;
mod streams;
mod strict;
mod usage;

/// A private working area per test: working directory, record directory, home
/// and configuration directory.
pub(super) struct Rig {
    _tmp: TempDir,
    pub work: PathBuf,
    pub record: PathBuf,
    pub home: PathBuf,
    pub config: PathBuf,
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
            record: make("record"),
            home: make("home"),
            config: make("config"),
            _tmp: tmp,
        }
    }

    pub fn fake(provider: Provider) -> PathBuf {
        let name = match provider {
            Provider::Claude => "fake-claude.sh",
            Provider::Codex => "fake-codex.sh",
        };
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src/provider/tests/fakes")
            .join(name)
    }

    /// Options that run the fake for `provider` with a controlled environment.
    pub fn options(&self, provider: Provider) -> ProviderOptions {
        let text = |p: &PathBuf| p.to_string_lossy().into_owned();
        ProviderOptions {
            system_paths: super::managed::SystemPaths::under(self.work.join("system")),
            executable: Some(Self::fake(provider)),
            parent_env: Some(vec![
                (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
                (OsString::from("HOME"), self.home.clone().into_os_string()),
            ]),
            env_set: vec![
                ("FAKE_RECORD_DIR".into(), text(&self.record)),
                ("XDG_CONFIG_HOME".into(), text(&self.config)),
                ("CLAUDE_CONFIG_DIR".into(), text(&self.config)),
                ("CODEX_HOME".into(), text(&self.config)),
            ],
            stop_grace: Some(Duration::from_millis(200)),
            ..ProviderOptions::default()
        }
    }

    pub fn request(&self, provider: Provider, prompt: &str) -> Request {
        let mut request = Request::new(provider, prompt);
        request.cwd = Some(self.work.clone());
        request
    }

    fn read(&self, name: &str) -> Vec<u8> {
        std::fs::read(self.record.join(name)).unwrap_or_default()
    }

    /// The arguments the fake received.
    pub fn argv(&self) -> Vec<String> {
        split_nul(&self.read("argv"))
    }

    /// The arguments the fake received for `mcp list`.
    pub fn mcp_argv(&self) -> Vec<String> {
        split_nul(&self.read("mcp-argv"))
    }

    /// What the fake read on standard input.
    pub fn stdin(&self) -> String {
        String::from_utf8_lossy(&self.read("stdin")).into_owned()
    }

    /// The names of the environment variables the fake saw.
    pub fn env_names(&self) -> Vec<String> {
        String::from_utf8_lossy(&self.read("env"))
            .lines()
            .map(str::to_string)
            .collect()
    }

    pub fn cwd_seen(&self) -> String {
        String::from_utf8_lossy(&self.read("cwd"))
            .trim()
            .to_string()
    }

    pub fn pid(&self, name: &str) -> Option<i32> {
        String::from_utf8_lossy(&self.read(name))
            .trim()
            .parse()
            .ok()
    }

    /// Assert that the fake and its background child, when they started, are
    /// gone.
    pub async fn assert_nothing_left(&self) {
        for name in ["pid", "child.pid"] {
            if let Some(pid) = self.pid(name) {
                assert_gone(name, pid).await;
            }
        }
    }

    /// A hook command that runs the real guard: this test executable, started
    /// so that it runs only `hook::hook_process_entry`.
    pub fn guard_hook() -> crate::provider::HookCommand {
        let exe = std::env::current_exe().expect("test executable path");
        crate::provider::HookCommand::new(
            exe,
            [
                "--exact",
                "provider::tests::hook::hook_process_entry",
                "--nocapture",
            ],
        )
    }

    /// Wait until the fake has signalled that it is running, by creating the
    /// empty record `started`. The wait ends a test whose event never comes,
    /// after [`AWAIT`], and costs nothing when the event arrives.
    pub async fn wait_started(&self) {
        let began = Instant::now();
        while !self.record.join("started").exists() {
            assert!(
                began.elapsed() < AWAIT,
                "waited {AWAIT:?} and the fake provider never started"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Wait until the fake has written something to the record `name`. The wait
    /// ends a test whose event never comes, after [`AWAIT`].
    pub async fn wait_record(&self, name: &str) {
        let began = Instant::now();
        while !std::fs::metadata(self.record.join(name)).is_ok_and(|m| m.len() > 0) {
            assert!(
                began.elapsed() < AWAIT,
                "waited {AWAIT:?} and the fake provider never wrote {name}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

fn split_nul(bytes: &[u8]) -> Vec<String> {
    let mut parts: Vec<String> = bytes
        .split(|b| *b == 0)
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect();
    // The record ends with a terminator, so the last piece is empty.
    parts.pop();
    parts
}

/// Whether a process is alive. A zombie counts as gone.
pub(super) fn is_running(pid: i32) -> bool {
    let out = std::process::Command::new("/bin/ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .guarded_output()
        .expect("run ps");
    let stat = String::from_utf8_lossy(&out.stdout);
    let stat = stat.trim();
    !stat.is_empty() && !stat.starts_with('Z')
}

/// How long a test waits for an event it does not control. Far longer than any
/// load makes it take; it only ends a test whose event never comes.
pub(super) const AWAIT: Duration = Duration::from_secs(90);

/// Wait for a process to be gone (see [`AWAIT`]), then assert it.
pub(super) async fn assert_gone(what: &str, pid: i32) {
    let began = Instant::now();
    while is_running(pid) {
        assert!(
            began.elapsed() < AWAIT,
            "{what} (pid {pid}) is still running after {AWAIT:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Run a request against the fake and check the result is self-consistent.
pub(super) async fn run_checked(request: Request, options: ProviderOptions) -> RunResult {
    let result = run(request, options).await;
    assert_eq!(result.validate(), Ok(()), "{result:#?}");
    result
}

/// Generate one test per provider from an async scenario function.
macro_rules! both {
    ($name:ident, $scenario:path) => {
        mod $name {
            use super::*;

            #[tokio::test]
            async fn claude() {
                $scenario(crate::contract::request::Provider::Claude).await;
            }

            #[tokio::test]
            async fn codex() {
                $scenario(crate::contract::request::Provider::Codex).await;
            }
        }
    };
}
pub(super) use both;
