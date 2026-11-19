//! Unit tests for what the integration tests in `tests/supervisor` cannot
//! reach: the bounded activity buffer, and a run's task that panics, which
//! needs a launcher that fails.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use tempfile::TempDir;

use crate::contract::outcome::Outcome;
use crate::contract::request::{Provider, Request};
use crate::contract::result::Cleanup;
use crate::provider::{self, ProgressEvent, ProviderOptions};

use super::activity::Activity;
use super::limits::{COMPACT_ACTIVITY, MAX_ACTIVITY, MAX_PARTIAL_TEXT_CHARS};
use super::{
    Detail, Error, ResultView, RunState, Supervisor, SupervisorConfig, Target, WaitTargets,
    WorkerState,
};

/// A directory with a copy of the Claude fake, so the path is in the command
/// line of every process the test starts.
struct Dir {
    tmp: TempDir,
}

impl Dir {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("create temporary directory");
        let root = std::fs::canonicalize(tmp.path()).expect("resolve temporary directory");
        for name in ["fakes", "work", "home"] {
            std::fs::create_dir(root.join(name)).expect("create directory");
        }
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src/provider/tests/fakes/fake-claude.sh");
        std::fs::copy(source, root.join("fakes/fake-claude.sh")).expect("copy the fake");
        Self { tmp }
    }

    fn root(&self) -> PathBuf {
        std::fs::canonicalize(self.tmp.path()).expect("resolve temporary directory")
    }

    fn config(&self) -> SupervisorConfig {
        let root = self.root();
        SupervisorConfig {
            provider: ProviderOptions {
                executable: Some(root.join("fakes/fake-claude.sh")),
                parent_env: Some(vec![
                    (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
                    (OsString::from("HOME"), root.join("home").into_os_string()),
                ]),
                stop_grace: Some(Duration::from_millis(200)),
                ..ProviderOptions::default()
            },
            ..SupervisorConfig::default()
        }
    }

    fn request(&self, prompt: &str) -> Request {
        let mut request = Request::new(Provider::Claude, prompt);
        request.cwd = Some(self.root().join("work"));
        request
    }

    fn gate(&self) -> PathBuf {
        self.root().join("gate")
    }

    async fn gate_started(&self) {
        let marker = self.root().join("gate.started");
        for _ in 0..1500 {
            if marker.exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the held run never started");
    }

    async fn assert_nothing_left(&self) {
        let marker = self.root().to_string_lossy().into_owned();
        let mut left = Vec::new();
        for _ in 0..1000 {
            let out = std::process::Command::new("/bin/ps")
                .args(["-axww", "-o", "pid=,command="])
                .output()
                .expect("run ps");
            left = String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter(|l| l.contains(&marker))
                .map(str::to_string)
                .collect();
            if left.is_empty() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("processes are still running: {left:#?}");
    }
}

fn panicking_launcher() -> super::runner::Launch {
    Arc::new(|request, options| {
        assert!(
            !request.prompt.starts_with("panic"),
            "the launcher was told to panic"
        );
        provider::start(request, options)
    })
}

async fn finished_result(supervisor: &Supervisor, run: &str) -> crate::contract::result::RunResult {
    let targets = WaitTargets {
        runs: vec![run.to_string()],
        workers: Vec::new(),
    };
    let waited = supervisor
        .wait(targets, Some(60), None)
        .await
        .expect("wait");
    assert!(!waited.timed_out, "{waited:#?}");
    let read = supervisor
        .read(Target::Run(run.to_string()), Detail::Full, None)
        .await
        .expect("read");
    let Some(ResultView::Full(result)) = read.result else {
        panic!("{read:#?}");
    };
    assert_eq!(result.validate(), Ok(()), "{result:#?}");
    *result
}

#[tokio::test]
async fn a_panic_in_one_runs_task_fails_that_run_and_leaves_the_rest_working() {
    let dir = Dir::new();
    let supervisor = Supervisor::with_launcher(dir.config(), panicking_launcher());
    let bystander = supervisor
        .start(dir.request(&format!("gate:{}", dir.gate().display())))
        .await
        .expect("start");
    dir.gate_started().await;
    let doomed = supervisor
        .start(dir.request("panic now"))
        .await
        .expect("start");

    let result = finished_result(&supervisor, &doomed.run).await;
    assert_eq!(result.outcome, Outcome::Failed);
    let error = result.error.as_ref().expect("error");
    assert_eq!(error.kind, "internal_error");
    assert!(error.message.contains("panicked"), "{error:?}");
    assert!(matches!(result.cleanup, Cleanup::Unconfirmed { .. }));

    // The bystander runs on, and the supervisor still answers and starts runs.
    let still = supervisor
        .read(Target::Run(bystander.run.clone()), Detail::Compact, None)
        .await
        .expect("read");
    assert_eq!(still.state, Some(RunState::Running));
    let list = supervisor.list().await.expect("list");
    assert_eq!(
        list.limits.running_runs, 1,
        "the panicked run freed its slot"
    );
    let worker = list
        .workers
        .iter()
        .find(|w| Some(&w.brief.worker) == doomed.worker.as_ref())
        .expect("the worker of the panicked run");
    assert_eq!(worker.brief.state, WorkerState::Idle);
    let after = supervisor
        .start(dir.request("echo:after"))
        .await
        .expect("start");
    assert_eq!(
        finished_result(&supervisor, &after.run).await.outcome,
        Outcome::Completed
    );

    std::fs::write(dir.gate(), b"").expect("open the gate");
    assert_eq!(
        finished_result(&supervisor, &bystander.run).await.outcome,
        Outcome::Completed
    );
    supervisor.shutdown().await;
    dir.assert_nothing_left().await;
}

#[test]
fn partial_text_is_bounded_and_keeps_its_end() {
    let activity = Activity::default();
    let cursor = AtomicU64::new(0);
    let block = "x".repeat(MAX_PARTIAL_TEXT_CHARS / 2);
    for _ in 0..3 {
        activity.record(
            ProgressEvent::Message {
                text: block.clone(),
            },
            &cursor,
        );
    }
    activity.record(
        ProgressEvent::Message {
            text: "the end".to_string(),
        },
        &cursor,
    );
    let snapshot = activity.snapshot();
    assert!(snapshot.text.chars().count() <= MAX_PARTIAL_TEXT_CHARS);
    assert!(snapshot.text.ends_with("the end"));
    assert!(snapshot.text_truncated);
    assert!(
        snapshot.entries.is_empty(),
        "a message is text, not an entry"
    );
}

#[test]
fn partial_text_cuts_on_a_character_boundary() {
    let activity = Activity::default();
    let cursor = AtomicU64::new(0);
    let text = "é".repeat(MAX_PARTIAL_TEXT_CHARS + 10);
    activity.record(ProgressEvent::Message { text }, &cursor);
    let snapshot = activity.snapshot();
    assert_eq!(snapshot.text.chars().count(), MAX_PARTIAL_TEXT_CHARS);
}

#[test]
fn the_activity_ring_drops_the_oldest_and_counts_them() {
    let activity = Activity::default();
    let cursor = AtomicU64::new(0);
    let total = MAX_ACTIVITY + COMPACT_ACTIVITY;
    for n in 0..total {
        activity.record(
            ProgressEvent::Provider {
                kind: "assistant".to_string(),
                summary: format!("n={n}"),
            },
            &cursor,
        );
    }
    let snapshot = activity.snapshot();
    assert_eq!(snapshot.entries.len(), MAX_ACTIVITY);
    assert_eq!(snapshot.total, total as u64);
    assert_eq!(snapshot.omitted, COMPACT_ACTIVITY as u64);
    assert_eq!(snapshot.entries[0].summary, format!("n={COMPACT_ACTIVITY}"));
    let cursors: Vec<u64> = snapshot.entries.iter().map(|e| e.cursor).collect();
    assert!(cursors.windows(2).all(|w| w[0] < w[1]), "cursors increase");
}

#[test]
fn no_activity_entry_appears_at_or_below_a_cursor_already_returned() {
    let cursor = Arc::new(AtomicU64::new(0));
    let watched = Arc::new(Activity::default());
    let stop = Arc::new(AtomicBool::new(false));
    // Several threads number entries from the one shared counter, as the runs
    // of a supervisor do, and one of them writes the buffer that is read.
    let writers: Vec<_> = (0..4)
        .map(|n| {
            let (cursor, stop) = (cursor.clone(), stop.clone());
            let own = if n == 0 {
                watched.clone()
            } else {
                Arc::new(Activity::default())
            };
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    own.record(
                        ProgressEvent::Provider {
                            kind: "assistant".to_string(),
                            summary: String::new(),
                        },
                        &cursor,
                    );
                }
            })
        })
        .collect();

    let mut previous: Option<(u64, HashSet<u64>)> = None;
    for _ in 0..20_000 {
        // What `read` does: take the cursor, then copy the buffer.
        let returned = cursor.load(Ordering::SeqCst);
        let seen: HashSet<u64> = watched
            .snapshot()
            .entries
            .iter()
            .map(|e| e.cursor)
            .collect();
        if let Some((before, earlier)) = &previous {
            for entry in &seen {
                assert!(
                    entry > before || earlier.contains(entry),
                    "entry {entry} appeared after a read that returned cursor {before}"
                );
            }
        }
        previous = Some((returned, seen));
    }
    stop.store(true, Ordering::Relaxed);
    for writer in writers {
        writer.join().expect("writer thread");
    }
}

#[tokio::test]
async fn a_panic_in_the_supervisors_own_task_stops_every_child_and_shutdown_says_it_failed() {
    let dir = Dir::new();
    let supervisor = Supervisor::new(dir.config());
    supervisor
        .start(dir.request(&format!("gate:{}", dir.gate().display())))
        .await
        .expect("start");
    dir.gate_started().await;

    supervisor.inject_panic();
    // The child is stopped on the way out of the panicking task, with nobody
    // asking for it.
    dir.assert_nothing_left().await;

    let mut failure = None;
    for _ in 0..1000 {
        match supervisor.list().await {
            Err(error @ Error::Failed(_)) => {
                failure = Some(error);
                break;
            }
            Err(Error::Stopped) | Ok(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            Err(other) => panic!("unexpected {other:?}"),
        }
    }
    let failure = failure.expect("calls report that the supervisor's task failed");
    assert!(failure.to_string().contains("panicked"), "{failure}");
    assert!(matches!(
        supervisor
            .follow_up("worker-x", "more", None)
            .await
            .expect_err("failed"),
        Error::Failed(_)
    ));

    let report = supervisor.shutdown().await;
    let why = report.failed.expect("not a clean report");
    assert!(why.contains("nothing confirmed"), "{why}");
}
