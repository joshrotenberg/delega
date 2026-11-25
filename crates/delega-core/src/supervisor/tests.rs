//! Unit tests for what the integration tests in `tests/supervisor` cannot
//! reach: the bounded activity buffer, and a run's task that panics, which
//! needs a launcher that fails.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use tempfile::TempDir;

use crate::contract::outcome::Outcome;
use crate::contract::request::{Provider, Request};
use crate::contract::result::Cleanup;
use crate::provider::{self, ProgressEvent, ProviderOptions};
use crate::test_spawn::Guarded;

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
        // A link, not a copy: on Linux, executing a file that any process
        // holds open for writing fails with ETXTBSY, and a fork on another
        // test thread can inherit the descriptor of a copy being written.
        std::os::unix::fs::symlink(source, root.join("fakes/fake-claude.sh"))
            .expect("link the fake");
        Self { tmp }
    }

    fn root(&self) -> PathBuf {
        std::fs::canonicalize(self.tmp.path()).expect("resolve temporary directory")
    }

    fn config(&self) -> SupervisorConfig {
        let root = self.root();
        SupervisorConfig {
            provider: ProviderOptions {
                system_paths: crate::provider::managed::SystemPaths::under(root.join("system")),
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

    /// Wait until the fake has begun the run held on the gate. The fake marks
    /// that by writing a file, so the wait ends on that event; the deadline only
    /// ends a test that would otherwise hang, and is generous for the same
    /// reason as the integration tests' `Gate::started`: on a loaded machine a
    /// version probe and start can take well over the 15 s this used to allow.
    async fn gate_started(&self) {
        let marker = self.root().join("gate.started");
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        while std::time::Instant::now() < deadline {
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
                .guarded_output()
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
        Box::new(move |cancel| {
            assert!(
                !request.prompt.starts_with("panic"),
                "the launcher was told to panic"
            );
            provider::prepare(&request, &options).launch(cancel)
        })
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
        .read(Target::Run(run.to_string()), Detail::Full, None, None)
        .await
        .expect("read");
    let Some(ResultView::Full(result)) = read.result else {
        panic!("{read:#?}");
    };
    assert_eq!(result.validate(), Ok(()), "{result:#?}");
    *result
}

/// Assert an `Unconfirmed` reason is one delega-core documents.
fn assert_documented(cleanup: &Cleanup) {
    if let Cleanup::Unconfirmed { reason, .. } = cleanup {
        assert!(
            crate::cleanup_reason::is_documented(reason),
            "an Unconfirmed reason delega-core does not document: {reason}"
        );
    }
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
    assert_documented(&result.cleanup);

    // The bystander runs on, and the supervisor still answers and starts runs.
    let still = supervisor
        .read(
            Target::Run(bystander.run.clone()),
            Detail::Compact,
            None,
            None,
        )
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
                system: false,
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
    let live = Arc::new(AtomicUsize::new(4));
    // Several threads number entries from the one shared counter, as the runs
    // of a supervisor do, and one of them writes the buffer that is read. Each
    // writes a fixed number and stops, so the test's CPU use is bounded.
    let writers: Vec<_> = (0..4)
        .map(|n| {
            let (cursor, live) = (cursor.clone(), live.clone());
            let own = if n == 0 {
                watched.clone()
            } else {
                Arc::new(Activity::default())
            };
            std::thread::spawn(move || {
                for _ in 0..30_000 {
                    own.record(
                        ProgressEvent::Provider {
                            kind: "assistant".to_string(),
                            summary: String::new(),
                            system: false,
                        },
                        &cursor,
                    );
                }
                live.fetch_sub(1, Ordering::SeqCst);
            })
        })
        .collect();

    let mut previous: Option<(u64, HashSet<u64>)> = None;
    while live.load(Ordering::SeqCst) > 0 {
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
    // After the shutdown closed the request checks, a start still reports the
    // failure, not a clean `ShutDown`.
    assert!(matches!(
        supervisor.start(dir.request("echo")).await,
        Err(Error::Failed(_))
    ));
}

#[tokio::test]
async fn an_unconfirmed_run_stays_recorded_after_a_refused_follow_up_replaced_it_as_the_latest() {
    let dir = Dir::new();
    let supervisor = Supervisor::with_launcher(dir.config(), panicking_launcher());
    let started = supervisor
        .start(dir.request("panic now"))
        .await
        .expect("start");
    let crashed = finished_result(&supervisor, &started.run).await;
    assert!(matches!(crashed.cleanup, Cleanup::Unconfirmed { .. }));
    assert_documented(&crashed.cleanup);
    let worker = started.worker.expect("worker");

    // The crashed run left no session, so the follow-up is refused at dispatch
    // with a confirmed cleanup and becomes the worker's latest run.
    let followed = supervisor
        .follow_up(&worker, "more", None)
        .await
        .expect("follow up");
    let refused = finished_result(&supervisor, &followed.run).await;
    assert_eq!(refused.cleanup, Cleanup::Confirmed, "{refused:#?}");
    let list = supervisor.list().await.expect("list");
    let latest = list.workers[0].brief.last_run.clone();
    assert_eq!(latest.as_deref(), Some(followed.run.as_str()));

    let unconfirmed = supervisor.unconfirmed().await.expect("unconfirmed");
    let runs: Vec<&str> = unconfirmed.iter().map(|(run, _)| run.as_str()).collect();
    assert_eq!(runs, [started.run.as_str()], "{unconfirmed:#?}");
    supervisor.shutdown().await;
    dir.assert_nothing_left().await;
}

/// Start a gated run, expire its registered wait, and hold the wait before
/// it asks for its view. Return the held wait and the gate that releases it.
async fn wait_held_after_its_timeout(
    dir: &Dir,
    supervisor: &Supervisor,
) -> (
    tokio::task::JoinHandle<Result<super::WaitResponse, Error>>,
    tokio::sync::oneshot::Sender<()>,
) {
    let started = supervisor
        .start(dir.request(&format!("gate:{}", dir.gate().display())))
        .await
        .expect("start");
    dir.gate_started().await;
    let mut waits = supervisor.observe_waits();
    let (reached, release) = supervisor.pause_next_wait_before_view();
    let supervisor = supervisor.clone();
    let targets = WaitTargets {
        runs: vec![started.run],
        workers: Vec::new(),
    };
    let mut wait = Box::pin(async move { supervisor.wait(targets, Some(1), None).await });
    // Send the command, then wait for the actor to park it with time running.
    tokio::select! {
        biased;
        response = &mut wait => panic!("wait answered before registration: {response:?}"),
        () = std::future::ready(()) => {}
    }
    waits.registered().await;

    tokio::time::pause();
    // Arm the timer on this task before advancing the clock.
    tokio::select! {
        biased;
        response = &mut wait => panic!("wait answered when armed: {response:?}"),
        () = std::future::ready(()) => {}
    }
    tokio::time::advance(Duration::from_millis(1001)).await;
    let waiting = tokio::spawn(wait);
    tokio::time::timeout(Duration::from_millis(10), reached)
        .await
        .expect("the wait did not expire at one second")
        .expect("the wait reached its view");
    tokio::time::resume();
    (waiting, release)
}

#[tokio::test]
async fn a_wait_held_between_its_timeout_and_its_view_takes_no_slot_from_the_next_wait() {
    let dir = Dir::new();
    let mut config = dir.config();
    config.limits.max_waits_in_flight = 1;
    let supervisor = Supervisor::new(config);
    let (waiting, release) = wait_held_after_its_timeout(&dir, &supervisor).await;
    // Its time has passed and its registration is still there, kept for the
    // view. It blocks nothing, so it is not a wait in flight.
    let listed = supervisor.list().await.expect("list");
    assert_eq!(listed.limits.max_waits_in_flight, 1);
    assert_eq!(listed.limits.waits_in_flight, 0, "{listed:#?}");
    let run = listed.workers[0].brief.current_run.clone().expect("run");
    // The one slot is free: the next wait blocks instead of being turned away.
    let mut waits = supervisor.observe_waits();
    let next = {
        let supervisor = supervisor.clone();
        let targets = WaitTargets {
            runs: vec![run],
            workers: Vec::new(),
        };
        tokio::spawn(async move { supervisor.wait(targets, Some(120), None).await })
    };
    let registration = waits.registered().await;
    assert!(
        !next.is_finished(),
        "the next wait was answered instead of blocking"
    );
    let listed = supervisor.list().await.expect("list");
    assert_eq!(listed.limits.waits_in_flight, 1, "{listed:#?}");
    release.send(()).expect("release the wait");
    let held = waiting.await.expect("task").expect("wait");
    assert!(held.timed_out, "{held:#?}");
    assert_eq!(held.limit_reached, None);
    assert_eq!(held.timeout_secs, 1);
    std::fs::write(dir.gate(), b"").expect("open the gate");
    registration.removed().await;
    let woken = next.await.expect("task").expect("wait");
    assert!(!woken.timed_out, "{woken:#?}");
    assert_eq!(woken.limit_reached, None);
    supervisor.shutdown().await;
    dir.assert_nothing_left().await;
}

#[tokio::test]
async fn a_wait_whose_timeout_elapsed_as_shutdown_began_still_says_shutdown_released_it() {
    let dir = Dir::new();
    let supervisor = Supervisor::new(dir.config());
    let (waiting, release) = wait_held_after_its_timeout(&dir, &supervisor).await;
    supervisor.shutdown().await;
    release.send(()).expect("release the wait");
    let response = waiting.await.expect("task").expect("wait");
    assert!(!response.timed_out, "{response:#?}");
    assert!(response.released_by_shutdown, "{response:#?}");
    dir.assert_nothing_left().await;
}

#[tokio::test]
async fn a_wait_that_timed_out_stays_timed_out_when_another_command_runs_before_its_view() {
    let dir = Dir::new();
    let supervisor = Supervisor::new(dir.config());
    let (waiting, release) = wait_held_after_its_timeout(&dir, &supervisor).await;
    // Any command wakes and prunes registrations; one whose receiver was
    // dropped would be read as fired.
    supervisor.list().await.expect("list");
    release.send(()).expect("release the wait");
    let response = waiting.await.expect("task").expect("wait");
    assert!(response.timed_out, "{response:#?}");
    assert!(!response.released_by_shutdown, "{response:#?}");
    supervisor.shutdown().await;
    dir.assert_nothing_left().await;
}

#[tokio::test]
async fn a_wait_on_a_queued_run_that_shutdown_discarded_is_not_marked_as_released_by_shutdown() {
    let dir = Dir::new();
    let supervisor = Supervisor::new(dir.config());
    let started = supervisor
        .start(dir.request(&format!("gate:{}", dir.gate().display())))
        .await
        .expect("start");
    dir.gate_started().await;
    let worker = started.worker.expect("worker");
    let queued = supervisor
        .follow_up(&worker, "echo:later", None)
        .await
        .expect("follow up");
    assert_eq!(queued.state, RunState::Queued);
    let mut waits = supervisor.observe_waits();
    let waiting = {
        let supervisor = supervisor.clone();
        let targets = WaitTargets {
            runs: vec![queued.run.clone()],
            workers: Vec::new(),
        };
        tokio::spawn(async move { supervisor.wait(targets, Some(300), None).await })
    };
    let registration = waits.registered().await;
    assert!(!waiting.is_finished(), "the wait returned before shutdown");
    supervisor.shutdown().await;
    registration.removed().await;
    let response = waiting.await.expect("task").expect("wait");
    assert!(!response.timed_out, "{response:#?}");
    assert_eq!(response.runs[0].state, RunState::Finished, "{response:#?}");
    assert!(
        !response.released_by_shutdown,
        "the discarded run finishing released it, not the shutdown: {response:#?}"
    );
    dir.assert_nothing_left().await;
}

#[tokio::test]
async fn a_result_that_fails_validation_is_replaced_in_every_build_and_the_supervisor_goes_on() {
    let dir = Dir::new();
    let supervisor = Supervisor::new(dir.config());
    // The supervisor's task runs on this thread, so the flag reaches the next
    // result it seals: the refusal of the request below.
    super::results::corrupt_next_seal_for_test();
    let mut invalid = dir.request("hello");
    invalid.instructions = Some("SECRET-TEXT\0".to_string());
    let refused = supervisor.start(invalid).await.expect("start");

    assert_eq!(refused.outcome, Some(Outcome::Failed), "{refused:#?}");
    let result = finished_result(&supervisor, &refused.run).await;
    assert_eq!(result.outcome, Outcome::Failed);
    let error = result.error.as_ref().expect("error");
    assert_eq!(error.kind, "internal_error");
    assert!(
        error.message.contains("inconsistent result"),
        "{}",
        error.message
    );
    let note = result
        .provider_notes
        .iter()
        .find(|n| n.contains("failed validation"))
        .unwrap_or_else(|| panic!("no note names the fault: {:?}", result.provider_notes));
    assert!(
        note.contains("error: is required when the outcome is not completed"),
        "{note}"
    );
    let shown = format!("{result:?}");
    assert!(!shown.contains("SECRET-TEXT"), "user text in {shown}");

    // The supervisor still answers, and still admits and finishes work.
    assert!(supervisor.list().await.is_ok());
    let after = supervisor
        .start(dir.request("echo:after"))
        .await
        .expect("start");
    assert_eq!(
        finished_result(&supervisor, &after.run).await.outcome,
        Outcome::Completed
    );
    supervisor.shutdown().await;
    dir.assert_nothing_left().await;
}

/// Lets go of every check a test holds open, even when the test fails first.
struct Release(Arc<std::sync::atomic::AtomicBool>);

impl Drop for Release {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn outstanding_request_checks_are_capped_and_a_start_beyond_the_cap_waits_then_times_out() {
    use super::limits::MAX_PENDING_PREFLIGHTS;
    use super::preflight::Preflight;
    use std::sync::atomic::AtomicBool;

    let dir = Dir::new();
    let released = Arc::new(AtomicBool::new(false));
    let _release = Release(released.clone());
    let entered = Arc::new(AtomicUsize::new(0));
    let check = {
        let (released, entered) = (released.clone(), entered.clone());
        Arc::new(move |request: &Request, _: &ProviderOptions| {
            if request.prompt.starts_with("hold") {
                entered.fetch_add(1, Ordering::SeqCst);
                while !released.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            None
        })
    };
    // A short bound makes each held start give up, as it does on a filesystem
    // that does not answer, while its thread stays and keeps counting.
    let preflight = Preflight::with(check, Duration::from_millis(100));
    let supervisor =
        Supervisor::with_hooks(dir.config(), super::runner::provider_launch(), preflight);

    for n in 0..MAX_PENDING_PREFLIGHTS {
        let timed_out = supervisor
            .start(dir.request(&format!("hold {n}")))
            .await
            .expect("start");
        assert_eq!(timed_out.outcome, Some(Outcome::Refused), "{timed_out:#?}");
        let error = timed_out.error.as_ref().expect("error");
        assert_eq!(error.kind, "preflight_timeout", "{error:?}");
    }
    for _ in 0..3000 {
        if entered.load(Ordering::SeqCst) == MAX_PENDING_PREFLIGHTS {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(entered.load(Ordering::SeqCst), MAX_PENDING_PREFLIGHTS);

    // Every check is outstanding: the next start waits for a slot, gets none
    // within the bound, and is refused as a check that did not answer.
    let over = supervisor
        .start(dir.request("echo:over"))
        .await
        .expect("start");
    assert_eq!(over.outcome, Some(Outcome::Refused), "{over:#?}");
    assert_eq!(over.worker, None);
    let error = over.error.as_ref().expect("error");
    assert_eq!(error.kind, "preflight_timeout", "{error:?}");
    assert!(
        error.message.starts_with("no request check slot was free"),
        "{error:?}"
    );
    assert_eq!(
        error
            .message
            .matches("request checks are not completing")
            .count(),
        1,
        "{error:?}"
    );
    let list = supervisor.list().await.expect("list");
    assert!(list.workers.is_empty(), "a refused start made a worker");

    // Once the held checks end, a later start is admitted and its run
    // completes. The short bound covers both the admission check and the run's
    // preparation when it is dispatched, so on a loaded machine either can
    // time out; each such attempt is repeated, and the worker of a run
    // refused at preparation is closed so the retries do not reach the worker
    // limit. Until the held threads have ended a start may also wait for a
    // slot.
    released.store(true, Ordering::SeqCst);
    let mut completed = None;
    for _ in 0..3000 {
        let later = supervisor
            .start(dir.request("echo:later"))
            .await
            .expect("start");
        match later.error.as_ref().map(|e| e.kind.as_str()) {
            None => {}
            Some("preflight_timeout") => {
                tokio::time::sleep(Duration::from_millis(20)).await;
                continue;
            }
            Some(other) => panic!("unexpected refusal {other}: {later:#?}"),
        }
        let result = finished_result(&supervisor, &later.run).await;
        match result.error.as_ref().map(|e| e.kind.as_str()) {
            Some("preflight_timeout") if result.outcome == Outcome::Refused => {
                let worker = later
                    .worker
                    .as_deref()
                    .expect("an admitted run has a worker");
                supervisor.close(worker).await.expect("close");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            _ => {
                completed = Some(result);
                break;
            }
        }
    }
    let completed = completed.expect("a run is admitted and prepared once the checks have ended");
    assert_eq!(completed.outcome, Outcome::Completed, "{completed:#?}");
    supervisor.shutdown().await;
    dir.assert_nothing_left().await;
}

/// A supervisor whose checks block on `hold` prompts until released, with a bound
/// of an hour so only the test decides when a waiting start ends.
fn holding_supervisor(
    dir: &Dir,
    released: &Arc<std::sync::atomic::AtomicBool>,
    bound: Duration,
) -> Supervisor {
    use super::preflight::Preflight;
    let released = released.clone();
    let check = Arc::new(move |request: &Request, _: &ProviderOptions| {
        if request.prompt.starts_with("hold") {
            while !released.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        None
    });
    let mut config = dir.config();
    config.limits.max_live_workers = 64;
    Supervisor::with_hooks(
        config,
        super::runner::provider_launch(),
        Preflight::with(check, bound),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn starts_waiting_for_a_check_slot_are_released_at_shutdown_with_shut_down() {
    use super::limits::MAX_PENDING_PREFLIGHTS;
    use std::sync::atomic::AtomicBool;

    let dir = Dir::new();
    let released = Arc::new(AtomicBool::new(false));
    let _release = Release(released.clone());
    let supervisor = holding_supervisor(&dir, &released, Duration::from_secs(3600));
    // One start per slot, held in its check, and four more waiting for a slot.
    let mut starts = tokio::task::JoinSet::new();
    for n in 0..MAX_PENDING_PREFLIGHTS + 4 {
        let (supervisor, request) = (supervisor.clone(), dir.request(&format!("hold {n}")));
        starts.spawn(async move { supervisor.start(request).await });
    }
    // Starts have no observable state until they are answered, so give the
    // tasks time to reach their waits; shutdown must release them whichever
    // wait each is in.
    tokio::time::sleep(Duration::from_millis(500)).await;
    supervisor.shutdown().await;
    let mut answered = 0;
    while let Some(joined) = starts.join_next().await {
        let outcome = joined.expect("the start task did not panic");
        assert!(matches!(outcome, Err(Error::ShutDown)), "{outcome:?}");
        answered += 1;
    }
    assert_eq!(answered, MAX_PENDING_PREFLIGHTS + 4);
    dir.assert_nothing_left().await;
}

/// The moment between `shutdown` closing the request checks and its `Shutdown`
/// reaching the supervisor's task, held open: a `start` in that moment, or one
/// released from inside its check by the close, is answered `ShutDown` and
/// starts nothing. Before the fix it sent `Start`, which the task admitted
/// because no `Shutdown` had arrived yet.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_start_after_the_checks_are_closed_is_shut_down_and_starts_nothing() {
    use super::preflight::Preflight;
    use std::sync::atomic::AtomicBool;

    let dir = Dir::new();
    let released = Arc::new(AtomicBool::new(false));
    let _release = Release(released.clone());
    let entered = Arc::new(tokio::sync::Notify::new());
    let check = {
        let (released, entered) = (released.clone(), entered.clone());
        Arc::new(move |request: &Request, _: &ProviderOptions| {
            if request.prompt.starts_with("hold") {
                entered.notify_one();
                while !released.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            None
        })
    };
    let supervisor = Supervisor::with_hooks(
        dir.config(),
        super::runner::provider_launch(),
        Preflight::with(check, Duration::from_secs(3600)),
    );
    // One start held inside its check, released by the close.
    let held = {
        let (supervisor, request) = (supervisor.clone(), dir.request("hold 0"));
        tokio::spawn(async move { supervisor.start(request).await })
    };
    tokio::time::timeout(Duration::from_secs(60), entered.notified())
        .await
        .expect("the held start entered its check");
    supervisor.shared.preflight.close();
    let late = supervisor.start(dir.request("echo")).await;
    assert!(matches!(late, Err(Error::ShutDown)), "{late:?}");
    let held = held.await.expect("the start task did not panic");
    assert!(matches!(held, Err(Error::ShutDown)), "{held:?}");
    let list = supervisor.list().await.expect("list");
    assert!(list.workers.is_empty(), "{:?}", list.workers);
    supervisor.shutdown().await;
    dir.assert_nothing_left().await;
}

/// Wait for `count` checks to report that they have entered, each named by
/// its prompt, and return the prompts. The deadline only ends a test that would
/// otherwise hang: an entry takes one thread spawn, so a minute is many times
/// what a loaded machine needs, and a check that never enters is a failure.
async fn entered(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
    count: usize,
) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let mut prompts = Vec::new();
    while prompts.len() < count {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(prompt)) => prompts.push(prompt),
            Ok(None) => panic!("the checks went away after {prompts:?}"),
            Err(_) => panic!(
                "only {} of {count} checks entered within a minute: {prompts:?}",
                prompts.len()
            ),
        }
    }
    prompts
}

/// Starts that stop waiting for a check slot, before one frees and after their
/// slot has been handed to them, give every slot back: afterwards all of them
/// can be held at once. The order of holders and waiters comes from events:
/// each check reports when it has entered, and each waiter is polled until it
/// is queued for a slot.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_start_that_stops_waiting_for_a_check_slot_does_not_keep_one() {
    use super::limits::MAX_PENDING_PREFLIGHTS;
    use super::preflight::Preflight;
    use std::future::Future;
    use std::sync::atomic::AtomicBool;
    use std::task::Poll;

    let dir = Dir::new();
    let first = Arc::new(AtomicBool::new(false));
    let second = Arc::new(AtomicBool::new(false));
    let never = Arc::new(AtomicBool::new(false));
    let (_first, _second, _never) = (
        Release(first.clone()),
        Release(second.clone()),
        Release(never.clone()),
    );
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let check = {
        let (first, second, never) = (first.clone(), second.clone(), never.clone());
        Arc::new(move |request: &Request, _: &ProviderOptions| {
            let gate = match request.prompt.split(' ').next() {
                Some("first") => &first,
                Some("second") => &second,
                Some("waiter") => &never,
                _ => return None,
            };
            let _ = tx.send(request.prompt.clone());
            while !gate.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
            None
        })
    };
    let mut config = dir.config();
    config.limits.max_live_workers = 64;
    let supervisor = Supervisor::with_hooks(
        config,
        super::runner::provider_launch(),
        Preflight::with(check, Duration::from_secs(3600)),
    );

    // Every slot is held.
    let mut holders = tokio::task::JoinSet::new();
    for n in 0..MAX_PENDING_PREFLIGHTS {
        let (supervisor, request) = (supervisor.clone(), dir.request(&format!("first {n}")));
        holders.spawn(async move { supervisor.start(request).await });
    }
    entered(&mut rx, MAX_PENDING_PREFLIGHTS).await;

    // Eight starts queue for a slot. A start's first poll runs straight to the
    // wait for a slot, so one poll that returns pending leaves it queued.
    let mut waiters: Vec<_> = (0..8)
        .map(|n| Box::pin(supervisor.start(dir.request(&format!("waiter {n}")))))
        .collect();
    for waiter in &mut waiters {
        let queued =
            std::future::poll_fn(|cx| Poll::Ready(waiter.as_mut().poll(cx).is_pending())).await;
        assert!(queued, "a start went ahead while every slot was held");
    }
    // Half stop waiting before any slot frees.
    let late_waiters = waiters.split_off(4);
    drop(waiters);

    // The holders end. A check frees its slot before it answers, so once all
    // have answered every slot has been freed, and the first four went to the
    // queued waiters, which are never polled again.
    first.store(true, Ordering::SeqCst);
    while let Some(joined) = holders.join_next().await {
        let started = joined.expect("task").expect("start");
        assert_eq!(started.error, None, "{started:#?}");
    }
    // The other half stop waiting holding the slot handed to them.
    drop(late_waiters);

    // All slots are free: as many checks as there are slots are held at once.
    let mut second_holders = tokio::task::JoinSet::new();
    for n in 0..MAX_PENDING_PREFLIGHTS {
        let (supervisor, request) = (supervisor.clone(), dir.request(&format!("second {n}")));
        second_holders.spawn(async move { supervisor.start(request).await });
    }
    let prompts = entered(&mut rx, MAX_PENDING_PREFLIGHTS).await;
    assert!(
        prompts.iter().all(|p| p.starts_with("second ")),
        "a start that stopped waiting ran its check: {prompts:?}"
    );
    second.store(true, Ordering::SeqCst);
    while let Some(joined) = second_holders.join_next().await {
        let started = joined.expect("task").expect("start");
        assert_eq!(started.error, None, "{started:#?}");
    }
    assert!(
        rx.try_recv().is_err(),
        "a start that stopped waiting ran its check"
    );
    supervisor.shutdown().await;
    dir.assert_nothing_left().await;
}

/// A start that waits most of its bound for a check slot and then meets a check
/// that does not answer is refused when the one bound, measured from the start
/// of the call, has passed: not after the wait plus a fresh bound, and not
/// before. Time is paused, so the clock moves only when no task can run, and the
/// timing does not depend on how loaded the machine is.
#[tokio::test(start_paused = true)]
async fn a_start_that_waited_for_a_check_slot_has_only_the_rest_of_its_bound_for_the_check() {
    use super::limits::MAX_PENDING_PREFLIGHTS;
    use super::preflight::Preflight;
    use std::sync::atomic::AtomicBool;
    use tokio::time::Instant;

    const BOUND: Duration = Duration::from_secs(10);
    const SLOT_WAIT: Duration = Duration::from_secs(8);
    // The timer rounds a deadline up to the next millisecond, so the refusal
    // comes at the bound or up to 1 ms after it. A fresh bound for the check
    // would make it SLOT_WAIT later, 8 s; an earlier deadline makes it early.
    const TIMER_RESOLUTION: Duration = Duration::from_millis(1);

    let dir = Dir::new();
    let released = Arc::new(AtomicBool::new(false));
    let _release = Release(released.clone());
    let entered = Arc::new(AtomicBool::new(false));
    let check = {
        let (released, entered) = (released.clone(), entered.clone());
        Arc::new(move |_: &Request, _: &ProviderOptions| {
            entered.store(true, Ordering::SeqCst);
            while !released.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
            None
        })
    };
    let preflight = Preflight::with(check, BOUND);
    // The test holds every slot itself, so it alone decides when one frees.
    let taken = preflight
        .slots()
        .acquire_many_owned(u32::try_from(MAX_PENDING_PREFLIGHTS).expect("slot count"))
        .await
        .expect("take every slot");
    let supervisor =
        Supervisor::with_hooks(dir.config(), super::runner::provider_launch(), preflight);
    let freer = {
        let entered = entered.clone();
        tokio::spawn(async move {
            tokio::time::sleep(SLOT_WAIT).await;
            let freed_at = Instant::now();
            drop(taken);
            // Paused time jumps to the next timer as soon as no task can run,
            // which would let the start time out while the check's thread is
            // still waiting to be scheduled. This task stays runnable by
            // yielding until the check reports it has entered, so the clock
            // cannot move first. The real-time limit only ends a check that
            // never enters; one thread spawn needs far less than a minute.
            let limit = std::time::Instant::now() + Duration::from_secs(60);
            while !entered.load(Ordering::SeqCst) {
                assert!(
                    std::time::Instant::now() < limit,
                    "the check never entered after its slot freed"
                );
                tokio::task::yield_now().await;
            }
            assert_eq!(
                Instant::now(),
                freed_at,
                "time moved before the check entered"
            );
        })
    };

    let begun = Instant::now();
    let refused = supervisor.start(dir.request("held")).await.expect("start");
    let took = begun.elapsed();
    freer
        .await
        .expect("the slot freed and the check entered before time moved on");

    assert_eq!(refused.outcome, Some(Outcome::Refused), "{refused:#?}");
    let error = refused.error.as_ref().expect("error");
    assert_eq!(error.kind, "preflight_timeout", "{error:?}");
    // It got a slot and timed out waiting for the check, not for the slot.
    assert!(
        error.message.starts_with("the check did not finish"),
        "{error:?}"
    );
    assert!(
        took >= BOUND && took <= BOUND + TIMER_RESOLUTION,
        "refused {took:?} after the start, not at its bound of {BOUND:?}"
    );
    supervisor.shutdown().await;
    dir.assert_nothing_left().await;
}

/// An origin as a caller that merged layers would build it.
fn origin() -> crate::contract::applied::Applied {
    use crate::contract::applied::{Applied, AppliedConfig, Layer};
    Applied {
        config: Some(AppliedConfig {
            path: "/cfg/config.toml".to_string(),
            sha256: "ab".repeat(32),
        }),
        sources: [
            ("provider".to_string(), Layer::Argument),
            ("prompt".to_string(), Layer::Argument),
            ("timeout_secs".to_string(), Layer::Config),
        ]
        .into_iter()
        .collect(),
        ..Applied::default()
    }
}

/// The parts of `applied` that only a prepared run has are absent.
fn assert_origin_only(result: &crate::contract::result::RunResult) {
    let applied = result.applied.as_ref().expect("applied");
    assert_eq!(applied.config, origin().config, "{result:#?}");
    assert_eq!(applied.sources, origin().sources, "{result:#?}");
    assert!(applied.env.is_none(), "{result:#?}");
    assert!(applied.deny_rules.is_none(), "{result:#?}");
    assert!(applied.context.is_none(), "{result:#?}");
}

#[tokio::test]
async fn a_run_refused_by_the_supervisor_for_a_name_in_use_reports_config_and_sources_only() {
    let dir = Dir::new();
    let supervisor = Supervisor::new(dir.config());
    let mut first = dir.request(&format!("gate:{}", dir.gate().display()));
    first.name = Some("same".to_string());
    supervisor
        .start_with_origin(first, origin())
        .await
        .expect("start");
    dir.gate_started().await;
    let mut second = dir.request("echo:two");
    second.name = Some("same".to_string());
    let refused = supervisor
        .start_with_origin(second, origin())
        .await
        .expect("start");
    let result = finished_result(&supervisor, &refused.run).await;
    assert_eq!(result.outcome, Outcome::Refused);
    assert_eq!(result.error.as_ref().expect("error").kind, "name_in_use");
    assert_origin_only(&result);
    std::fs::write(dir.gate(), b"").expect("open the gate");
    supervisor.shutdown().await;
    dir.assert_nothing_left().await;
}

#[tokio::test]
async fn a_run_refused_at_the_worker_limit_reports_config_and_sources_only() {
    let dir = Dir::new();
    let mut config = dir.config();
    config.limits.max_live_workers = 1;
    let supervisor = Supervisor::new(config);
    supervisor
        .start_with_origin(
            dir.request(&format!("gate:{}", dir.gate().display())),
            origin(),
        )
        .await
        .expect("start");
    dir.gate_started().await;
    let refused = supervisor
        .start_with_origin(dir.request("echo:two"), origin())
        .await
        .expect("start");
    let result = finished_result(&supervisor, &refused.run).await;
    assert_eq!(result.outcome, Outcome::Refused);
    assert_origin_only(&result);
    std::fs::write(dir.gate(), b"").expect("open the gate");
    supervisor.shutdown().await;
    dir.assert_nothing_left().await;
}

#[tokio::test]
async fn a_run_stopped_in_the_queue_reports_config_and_sources_only() {
    let dir = Dir::new();
    let mut config = dir.config();
    config.limits.max_running_runs = 1;
    let supervisor = Supervisor::new(config);
    supervisor
        .start_with_origin(
            dir.request(&format!("gate:{}", dir.gate().display())),
            origin(),
        )
        .await
        .expect("start");
    dir.gate_started().await;
    let queued = supervisor
        .start_with_origin(dir.request("echo:queued"), origin())
        .await
        .expect("start");
    let stopped = supervisor
        .interrupt(Target::Run(queued.run.clone()))
        .await
        .expect("interrupt");
    assert!(stopped.settled, "{stopped:#?}");
    let result = finished_result(&supervisor, &queued.run).await;
    assert_eq!(result.outcome, Outcome::Interrupted);
    assert_origin_only(&result);
    std::fs::write(dir.gate(), b"").expect("open the gate");
    supervisor.shutdown().await;
    dir.assert_nothing_left().await;
}

#[tokio::test]
async fn a_run_started_without_an_origin_has_no_sources_even_when_refused() {
    let dir = Dir::new();
    let mut config = dir.config();
    config.limits.max_live_workers = 1;
    let supervisor = Supervisor::new(config);
    supervisor
        .start(dir.request(&format!("gate:{}", dir.gate().display())))
        .await
        .expect("start");
    dir.gate_started().await;
    let refused = supervisor
        .start(dir.request("echo:two"))
        .await
        .expect("start");
    let result = finished_result(&supervisor, &refused.run).await;
    assert_eq!(result.outcome, Outcome::Refused);
    assert!(result.applied.is_none(), "{result:#?}");
    std::fs::write(dir.gate(), b"").expect("open the gate");
    supervisor.shutdown().await;
    dir.assert_nothing_left().await;
}

#[tokio::test]
async fn a_follow_up_reports_its_prompt_as_an_argument_whatever_layer_set_the_starts() {
    use crate::contract::applied::Layer;
    let dir = Dir::new();
    let supervisor = Supervisor::new(dir.config());
    // The start's prompt came from a profile, so only the follow-up's own
    // `prompt` entry can make it `argument`.
    let mut start_origin = origin();
    start_origin
        .sources
        .insert("prompt".to_string(), Layer::Profile);
    let started = supervisor
        .start_with_origin(dir.request("echo:one"), start_origin.clone())
        .await
        .expect("start");
    let first = finished_result(&supervisor, &started.run).await;
    assert_eq!(first.outcome, Outcome::Completed, "{first:#?}");
    let worker = started.worker.expect("worker");

    let followed = supervisor
        .follow_up(&worker, "echo:two", None)
        .await
        .expect("follow up");
    let result = finished_result(&supervisor, &followed.run).await;
    let applied = result.applied.as_ref().expect("applied");
    assert_eq!(
        applied.sources.get("prompt"),
        Some(&Layer::Argument),
        "{result:#?}"
    );
    assert_eq!(
        applied.sources.get("timeout_secs"),
        Some(&Layer::Config),
        "{result:#?}"
    );
    assert_eq!(applied.config, start_origin.config, "{result:#?}");
    supervisor.shutdown().await;
    dir.assert_nothing_left().await;
}

#[tokio::test]
async fn a_follow_up_that_continued_another_session_does_not_replace_the_workers_session() {
    let dir = Dir::new();
    let mut config = dir.config();
    // Every resumed run reports this session instead of the one it asked for.
    config
        .provider
        .env_set
        .push(("FAKE_RESUMED_SESSION_ID".into(), "stray-session".into()));
    let supervisor = Supervisor::new(config);
    let started = supervisor
        .start(dir.request("echo:one"))
        .await
        .expect("start");
    let first = finished_result(&supervisor, &started.run).await;
    assert_eq!(first.outcome, Outcome::Completed, "{first:#?}");
    assert_eq!(first.session_id.as_deref(), Some("fake-claude-session-1"));
    let worker = started.worker.expect("worker");

    for prompt in ["echo:two", "echo:three"] {
        let followed = supervisor
            .follow_up(&worker, prompt, None)
            .await
            .expect("follow up");
        let result = finished_result(&supervisor, &followed.run).await;
        let error = result.error.as_ref().expect("error");
        assert_eq!(error.kind, "session_mismatch", "{prompt}: {result:#?}");
        // Each follow-up asked for the session the worker started with, not the
        // one the previous follow-up reported.
        assert!(
            error
                .message
                .contains("resume session fake-claude-session-1 "),
            "{prompt}: {}",
            error.message
        );
    }
    supervisor.shutdown().await;
    dir.assert_nothing_left().await;
}

#[tokio::test]
async fn a_timed_out_follow_up_that_reported_another_session_does_not_replace_the_workers_session()
{
    let dir = Dir::new();
    let record = dir.root().join("record");
    std::fs::create_dir(&record).expect("create directory");
    let mut config = dir.config();
    config
        .provider
        .env_set
        .push(("FAKE_RESUMED_SESSION_ID".into(), "stray-session".into()));
    config.provider.env_set.push((
        "FAKE_RECORD_DIR".into(),
        record.to_string_lossy().into_owned(),
    ));
    let supervisor = Supervisor::new(config);
    let started = supervisor
        .start(dir.request("echo:one"))
        .await
        .expect("start");
    let first = finished_result(&supervisor, &started.run).await;
    assert_eq!(first.outcome, Outcome::Completed, "{first:#?}");
    let worker = started.worker.expect("worker");

    // This follow-up reports the stray session, then runs out of time. The
    // limit is far above the time the fake takes to start on a loaded machine,
    // and its `started` record, written after the event that names the
    // session, shows the event was written before the fake was stopped.
    let hung = supervisor
        .follow_up(&worker, "hang", Some(5))
        .await
        .expect("follow up");
    let hung = finished_result(&supervisor, &hung.run).await;
    assert_eq!(hung.outcome, Outcome::TimedOut, "{hung:#?}");
    assert!(
        record.join("started").exists(),
        "the fake did not report its session before it was stopped: {hung:#?}"
    );
    assert_eq!(hung.session_id.as_deref(), Some("stray-session"));

    let followed = supervisor
        .follow_up(&worker, "echo:two", None)
        .await
        .expect("follow up");
    let result = finished_result(&supervisor, &followed.run).await;
    let error = result.error.as_ref().expect("error");
    assert!(
        error
            .message
            .contains("resume session fake-claude-session-1 "),
        "{result:#?}"
    );
    let list = supervisor.list().await.expect("list");
    assert_eq!(
        list.workers[0].session_id.as_deref(),
        Some("fake-claude-session-1")
    );
    supervisor.shutdown().await;
    dir.assert_nothing_left().await;
}

fn provider_event(kind: &str, summary: &str) -> ProgressEvent {
    ProgressEvent::Provider {
        kind: kind.to_string(),
        summary: summary.to_string(),
        system: kind == "system",
    }
}

fn kinds_and_summaries(activity: &Activity) -> Vec<(String, String)> {
    activity
        .snapshot()
        .entries
        .into_iter()
        .map(|e| (e.kind, e.summary))
        .collect()
}

#[test]
fn a_run_of_thinking_tokens_between_two_tool_uses_is_one_entry_stating_the_count() {
    let activity = Activity::default();
    let cursor = AtomicU64::new(0);
    activity.record(provider_event("assistant", "tool_use: Bash"), &cursor);
    for _ in 0..100 {
        activity.record(provider_event("system", "thinking_tokens"), &cursor);
    }
    activity.record(provider_event("assistant", "tool_use: Read"), &cursor);
    let pair = |k: &str, s: &str| (k.to_string(), s.to_string());
    assert_eq!(
        kinds_and_summaries(&activity),
        vec![
            pair("assistant", "tool_use: Bash"),
            pair("system", "thinking_tokens (100 in a row)"),
            pair("assistant", "tool_use: Read"),
        ]
    );
    let snapshot = activity.snapshot();
    assert_eq!(snapshot.omitted, 0, "nothing was dropped from the front");
    let cursors: Vec<u64> = snapshot.entries.iter().map(|e| e.cursor).collect();
    assert!(cursors.windows(2).all(|w| w[0] < w[1]), "cursors increase");
}

#[test]
fn a_reader_with_a_cursor_from_before_or_during_a_run_of_system_events_gets_the_coalesced_entry() {
    let activity = Activity::default();
    let cursor = AtomicU64::new(0);
    activity.record(provider_event("assistant", "tool_use: Bash"), &cursor);
    let before_run = cursor.load(Ordering::SeqCst);
    activity.record(provider_event("system", "thinking_tokens"), &cursor);
    // A reader that has already seen the run's first entry.
    let during_run = cursor.load(Ordering::SeqCst);
    for _ in 0..4 {
        activity.record(provider_event("system", "thinking_tokens"), &cursor);
    }
    let entries = activity.snapshot().entries;
    for after in [before_run, during_run] {
        let later: Vec<&str> = entries
            .iter()
            .filter(|e| e.cursor > after)
            .map(|e| e.summary.as_str())
            .collect();
        assert_eq!(
            later,
            vec!["thinking_tokens (5 in a row)"],
            "after cursor {after}"
        );
    }
    assert_eq!(
        entries.last().map(|e| e.cursor),
        Some(cursor.load(Ordering::SeqCst)),
        "the coalesced entry carries the newest cursor"
    );
}

#[test]
fn an_event_shown_as_system_whose_type_was_not_exactly_system_is_not_merged() {
    let activity = Activity::default();
    let cursor = AtomicU64::new(0);
    activity.record(provider_event("system", ""), &cursor);
    // A provider type of " system " is shown as `system` but is another type.
    activity.record(
        ProgressEvent::Provider {
            kind: "system".to_string(),
            summary: String::new(),
            system: false,
        },
        &cursor,
    );
    activity.record(provider_event("system", ""), &cursor);
    let pair = |k: &str, s: &str| (k.to_string(), s.to_string());
    assert_eq!(
        kinds_and_summaries(&activity),
        vec![pair("system", ""), pair("system", ""), pair("system", "")]
    );
}

#[test]
fn two_different_system_subtypes_in_a_row_are_not_merged() {
    let activity = Activity::default();
    let cursor = AtomicU64::new(0);
    activity.record(provider_event("system", "thinking_tokens"), &cursor);
    activity.record(provider_event("system", "init"), &cursor);
    activity.record(provider_event("system", "thinking_tokens"), &cursor);
    let summaries: Vec<String> = kinds_and_summaries(&activity)
        .into_iter()
        .map(|(_, s)| s)
        .collect();
    assert_eq!(
        summaries,
        vec!["thinking_tokens", "init", "thinking_tokens"]
    );
}

#[test]
fn identical_non_system_provider_events_and_delegas_own_entries_are_not_merged() {
    let activity = Activity::default();
    let cursor = AtomicU64::new(0);
    activity.record(provider_event("assistant", "tool_use: Bash"), &cursor);
    activity.record(provider_event("assistant", "tool_use: Bash"), &cursor);
    for _ in 0..2 {
        activity.record(
            ProgressEvent::Denied {
                request: "Bash(rm)".to_string(),
            },
            &cursor,
        );
    }
    let pair = |k: &str, s: &str| (k.to_string(), s.to_string());
    assert_eq!(
        kinds_and_summaries(&activity),
        vec![
            pair("assistant", "tool_use: Bash"),
            pair("assistant", "tool_use: Bash"),
            pair("denied", "Bash(rm)"),
            pair("denied", "Bash(rm)"),
        ]
    );
}

#[test]
fn the_activity_total_counts_every_event_of_a_coalesced_run() {
    let activity = Activity::default();
    let cursor = AtomicU64::new(0);
    activity.record(provider_event("assistant", "tool_use: Bash"), &cursor);
    for _ in 0..37 {
        activity.record(provider_event("system", "thinking_tokens"), &cursor);
    }
    let snapshot = activity.snapshot();
    assert_eq!(snapshot.entries.len(), 2);
    assert_eq!(snapshot.total, 38);
    assert_eq!(snapshot.omitted, 0);
}

#[test]
fn a_repeated_system_event_in_a_full_buffer_replaces_the_tail_and_evicts_nothing() {
    let activity = Activity::default();
    let cursor = AtomicU64::new(0);
    for n in 0..MAX_ACTIVITY - 1 {
        activity.record(provider_event("assistant", &format!("n={n}")), &cursor);
    }
    activity.record(provider_event("system", "thinking_tokens"), &cursor);
    let full = activity.snapshot();
    assert_eq!(full.entries.len(), MAX_ACTIVITY);
    assert_eq!(full.omitted, 0);

    activity.record(provider_event("system", "thinking_tokens"), &cursor);
    let after = activity.snapshot();
    assert_eq!(after.entries.len(), MAX_ACTIVITY, "the length is unchanged");
    assert_eq!(after.omitted, 0, "nothing was evicted from the front");
    assert_eq!(after.entries[0].summary, "n=0", "the oldest entry is kept");
    let tail = after.entries.last().expect("a tail entry");
    assert_eq!(tail.summary, "thinking_tokens (2 in a row)");
    assert_eq!(tail.cursor, cursor.load(Ordering::SeqCst));
    assert_eq!(after.total, MAX_ACTIVITY as u64 + 1);
}

/// A wall clock the test moves by hand. It counts its readings, so a test can
/// tell that a call has taken its starting time before moving the clock.
struct HandClock {
    now: std::sync::Mutex<std::time::SystemTime>,
    readings: AtomicUsize,
}

impl HandClock {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            now: std::sync::Mutex::new(
                std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1 << 30),
            ),
            readings: AtomicUsize::new(0),
        })
    }

    fn clock(self: &Arc<Self>) -> super::bound::WallClock {
        let clock = self.clone();
        Arc::new(move || {
            clock.readings.fetch_add(1, Ordering::SeqCst);
            *clock.now.lock().expect("clock")
        })
    }

    /// Wait until a bounded call has read its starting time. The deadline only
    /// ends a test that would otherwise hang.
    async fn bound_began(&self) {
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        while self.readings.load(Ordering::SeqCst) == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "no bounded call read the wall clock"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    fn forward(&self, by: Duration) {
        let mut now = self.now.lock().expect("clock");
        *now += by;
    }

    fn back(&self, by: Duration) {
        let mut now = self.now.lock().expect("clock");
        *now -= by;
    }
}

/// Far more awake time than the one check interval a bound needs to notice
/// that wall time has passed, on a loaded machine, and far less than any limit
/// the tests below would otherwise run to.
const NOTICED_WITHIN: Duration = Duration::from_secs(20);

/// Start a gated run on a supervisor that reads `clock`, and a `wait` for it
/// with `timeout_secs`. Returns once the wait has read its starting time, with
/// the awake time at which the wait began.
async fn parked_wait(
    dir: &Dir,
    clock: &Arc<HandClock>,
    timeout_secs: u64,
) -> (
    Supervisor,
    tokio::task::JoinHandle<Result<super::WaitResponse, Error>>,
    tokio::time::Instant,
) {
    let supervisor = Supervisor::with_wall_clock(dir.config(), clock.clock());
    let started = supervisor
        .start(dir.request(&format!("gate:{}", dir.gate().display())))
        .await
        .expect("start");
    dir.gate_started().await;
    let begun = tokio::time::Instant::now();
    let waiting = {
        let supervisor = supervisor.clone();
        let targets = WaitTargets {
            runs: vec![started.run],
            workers: Vec::new(),
        };
        tokio::spawn(async move { supervisor.wait(targets, Some(timeout_secs), None).await })
    };
    clock.bound_began().await;
    (supervisor, waiting, begun)
}

#[tokio::test]
async fn a_wait_ends_timed_out_once_wall_time_passed_its_limit_though_awake_time_did_not() {
    let dir = Dir::new();
    let clock = HandClock::new();
    let (supervisor, waiting, begun) = parked_wait(&dir, &clock, 300).await;
    // What a sleep of the machine does: wall time moves, Tokio's does not.
    clock.forward(Duration::from_secs(300));
    let response = waiting.await.expect("task").expect("wait");
    let took = begun.elapsed();
    assert!(response.timed_out, "{response:#?}");
    assert_eq!(response.timeout_secs, 300, "{response:#?}");
    assert!(!response.released_by_shutdown, "{response:#?}");
    assert!(
        took < NOTICED_WITHIN,
        "the wait took {took:?} of awake time after 300 s of wall time had passed"
    );
    supervisor.shutdown().await;
    dir.assert_nothing_left().await;
}

#[tokio::test]
async fn a_wall_clock_set_back_does_not_end_a_wait_early() {
    let dir = Dir::new();
    let clock = HandClock::new();
    let (supervisor, waiting, begun) = parked_wait(&dir, &clock, 2).await;
    clock.back(Duration::from_secs(3600));
    let response = waiting.await.expect("task").expect("wait");
    let took = begun.elapsed();
    assert!(response.timed_out, "{response:#?}");
    // The wait began after `begun` was taken, so it cannot be shorter.
    assert!(
        took >= Duration::from_secs(2) && took < NOTICED_WITHIN,
        "the wait took {took:?}, not its 2 s of awake time"
    );
    supervisor.shutdown().await;
    dir.assert_nothing_left().await;
}

#[tokio::test]
async fn a_wait_still_ends_at_its_awake_time_limit_when_the_wall_clock_does_not_move() {
    let dir = Dir::new();
    let clock = HandClock::new();
    let (supervisor, waiting, begun) = parked_wait(&dir, &clock, 1).await;
    let response = waiting.await.expect("task").expect("wait");
    let took = begun.elapsed();
    assert!(response.timed_out, "{response:#?}");
    assert!(
        took >= Duration::from_secs(1) && took < NOTICED_WITHIN,
        "the wait took {took:?}, not its 1 s of awake time"
    );
    supervisor.shutdown().await;
    dir.assert_nothing_left().await;
}

/// Far above the time the tests below need, so neither the stop's escalation
/// nor the awake-time bound can end them: only the wall clock does.
const FAR_OFF: Duration = Duration::from_secs(60 * 60);

/// A supervisor that reads `clock`, with a running run that ignores the polite
/// stop and ends only when the test creates the gate file. The stop's
/// escalation and the interrupt bound are both [`FAR_OFF`].
async fn unstoppable(dir: &Dir, clock: &Arc<HandClock>) -> (Supervisor, super::StartResponse) {
    let mut config = dir.config();
    config.provider.stop_grace = Some(FAR_OFF);
    config.interrupt_bound = FAR_OFF;
    let supervisor = Supervisor::with_wall_clock(config, clock.clock());
    let started = supervisor
        .start(dir.request(&format!("hang-until:{}", dir.gate().display())))
        .await
        .expect("start");
    // The fake writes its marker after it has begun ignoring the polite stop.
    dir.gate_started().await;
    (supervisor, started)
}

/// Await a stop that the moved wall clock should end at its next check, about a
/// second away. The limit only ends a test that would otherwise wait for
/// [`FAR_OFF`]; it is far above the time a loaded machine needs.
async fn ended_by_wall_time<T>(stopping: tokio::task::JoinHandle<T>) -> T {
    tokio::time::timeout(Duration::from_secs(120), stopping)
        .await
        .expect("the stop was still waiting 120 s after wall time passed its bound")
        .expect("task")
}

/// Let the run of [`unstoppable`] end, and check it ended as stopped and left
/// nothing behind.
async fn release_and_settle(dir: &Dir, supervisor: &Supervisor, run: &str) {
    std::fs::write(dir.gate(), b"").expect("release the run");
    let result = finished_result(supervisor, run).await;
    assert_eq!(result.outcome, Outcome::Interrupted, "{result:#?}");
    supervisor.shutdown().await;
    dir.assert_nothing_left().await;
}

#[tokio::test]
async fn an_interrupt_says_not_settled_once_wall_time_passed_its_bound_though_awake_time_did_not() {
    let dir = Dir::new();
    let clock = HandClock::new();
    let (supervisor, started) = unstoppable(&dir, &clock).await;
    let interrupting = {
        let (supervisor, run) = (supervisor.clone(), started.run.clone());
        tokio::spawn(async move { supervisor.interrupt(Target::Run(run)).await })
    };
    clock.bound_began().await;
    // The run cannot stop until the gate is released, so nothing but wall time
    // can end the interrupt before then.
    clock.forward(FAR_OFF);
    let response = ended_by_wall_time(interrupting).await.expect("interrupt");
    assert!(!dir.gate().exists(), "the run was released too early");
    assert!(!response.settled, "{response:#?}");
    assert!(response.result.is_none(), "{response:#?}");
    release_and_settle(&dir, &supervisor, &started.run).await;
}

#[tokio::test]
async fn a_close_says_not_settled_once_wall_time_passed_its_bound_though_awake_time_did_not() {
    let dir = Dir::new();
    let clock = HandClock::new();
    let (supervisor, started) = unstoppable(&dir, &clock).await;
    let worker = started.worker.clone().expect("worker");
    let closing = {
        let supervisor = supervisor.clone();
        tokio::spawn(async move { supervisor.close(&worker).await })
    };
    clock.bound_began().await;
    clock.forward(FAR_OFF);
    let response = ended_by_wall_time(closing).await.expect("close");
    assert!(!dir.gate().exists(), "the run was released too early");
    assert!(!response.settled, "{response:#?}");
    assert!(response.result.is_none(), "{response:#?}");
    assert_eq!(
        response.stopped_run.as_deref(),
        Some(started.run.as_str()),
        "{response:#?}"
    );
    release_and_settle(&dir, &supervisor, &started.run).await;
}

#[tokio::test]
async fn a_parked_wait_whose_run_aged_out_does_not_hold_its_slot_against_the_next_wait() {
    let dir = Dir::new();
    let mut config = dir.config();
    config.limits.max_waits_in_flight = 1;
    let supervisor = Supervisor::new(config);
    let held = supervisor
        .start(dir.request(&format!("gate:{}", dir.gate().display())))
        .await
        .expect("start");
    dir.gate_started().await;
    let old = supervisor
        .start(dir.request("success"))
        .await
        .expect("start");
    let targets = |run: &str| WaitTargets {
        runs: vec![run.to_string()],
        workers: Vec::new(),
    };
    let done = supervisor
        .wait(targets(&old.run), Some(60), None)
        .await
        .expect("wait");
    assert!(!done.timed_out, "{done:#?}");

    // Parked on the finished run with a cursor past its finish: nothing wakes
    // it until its result ages out. Retention is an hour of the supervisor's
    // clock, which only this test moves, so the result cannot go early.
    let mut waits = supervisor.observe_waits();
    let stale = {
        let supervisor = supervisor.clone();
        let targets = targets(&old.run);
        tokio::spawn(async move { supervisor.wait(targets, Some(120), Some(done.cursor)).await })
    };
    let registration = waits.registered().await;
    assert!(!stale.is_finished(), "the wait was answered, not parked");
    let listed = supervisor.list().await.expect("list");
    assert_eq!(listed.limits.waits_in_flight, 1, "{listed:#?}");

    // The result ages out between two commands. The wait below is the command
    // that prunes it, and it must find the one slot free.
    supervisor.advance_retention_clock(Duration::from_secs(2 * 60 * 60));
    let next = supervisor
        .wait(targets(&held.run), Some(1), None)
        .await
        .expect("wait");
    assert_eq!(next.limit_reached, None, "{next:#?}");
    assert_eq!(next.timeout_secs, 1);
    assert!(next.timed_out, "{next:#?}");

    registration.removed().await;
    let gone = tokio::time::timeout(Duration::from_secs(60), stale)
        .await
        .expect("pruning woke the parked wait")
        .expect("task")
        .expect("wait");
    assert_eq!(gone.unknown, [old.run.as_str()], "{gone:#?}");
    assert_eq!(gone.limit_reached, None);
    let listed = supervisor.list().await.expect("list");
    assert_eq!(listed.limits.waits_in_flight, 0, "{listed:#?}");
    supervisor.shutdown().await;
    dir.assert_nothing_left().await;
}

/// Reports, when a held run's start closure is dropped, its prompt and whether
/// it was called: a preparation that ends after its run was released must be
/// dropped without starting anything.
struct Tracker {
    prompt: String,
    launched: bool,
    tx: tokio::sync::mpsc::UnboundedSender<(String, bool)>,
}

impl Drop for Tracker {
    fn drop(&mut self) {
        let _ = self
            .tx
            .send((std::mem::take(&mut self.prompt), self.launched));
    }
}

/// The test hooks of [`holding_launcher`], chosen by the prompt:
///
/// - `hold...`: the preparation reports that it has entered and blocks on a
///   release message; its start closure is tracked.
/// - `panic-prep...`: the preparation itself panics, on its thread.
/// - `stop-at-launch...`: the start closure cancels `stop_at_launch` (the
///   supervisor's root token, so the run's stop) before it launches, and
///   reports whether the token it was given saw that stop.
/// - anything else: prepared and started at once.
///
/// Every run that goes ahead is the provider layer's own, prepared on the
/// preparation's thread.
struct HeldPreparations {
    entered: tokio::sync::mpsc::UnboundedReceiver<String>,
    /// One message lets one held preparation go on. Dropping it, as a failing
    /// test does, lets them all go on.
    release: std::sync::mpsc::Sender<()>,
    /// What each held run's start closure did, when it is dropped.
    ended: tokio::sync::mpsc::UnboundedReceiver<(String, bool)>,
    /// The token a `stop-at-launch` run cancels as it launches.
    stop_at_launch: Arc<std::sync::OnceLock<tokio_util::sync::CancellationToken>>,
    /// Whether the token given to a `stop-at-launch` start closure was
    /// cancelled by that stop.
    saw_stop: tokio::sync::mpsc::UnboundedReceiver<bool>,
}

/// Ends the test process if the test has not finished within its limit. It
/// runs on a thread of its own, not on the test's runtime, so it still fires
/// when that runtime's only thread is blocked, where a Tokio timeout would
/// never be polled. Dropping it, when the test ends either way, stops it.
struct Watchdog {
    _done: std::sync::mpsc::Sender<()>,
}

fn watchdog(test: &'static str) -> Watchdog {
    // Only a blocked runtime reaches this; the test itself takes seconds.
    const LIMIT: Duration = Duration::from_secs(300);
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        if let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(LIMIT) {
            // Written to the handle, not with `eprintln!`: the test harness
            // captures that, and exiting would discard what it captured.
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr().lock(),
                "{test}: not finished within {LIMIT:?}; the runtime it runs on is blocked, \
                 as it is when a run is prepared on the runtime's own thread"
            );
            std::process::exit(101);
        }
    });
    Watchdog { _done: tx }
}

impl HeldPreparations {
    async fn entered(&mut self) -> String {
        tokio::time::timeout(Duration::from_secs(60), self.entered.recv())
            .await
            .expect("the preparation entered within a minute")
            .expect("the launcher is alive")
    }

    /// Let one held preparation go on and return what its start closure did.
    async fn release_one(&mut self) -> (String, bool) {
        self.release.send(()).expect("a preparation is held");
        tokio::time::timeout(Duration::from_secs(60), self.ended.recv())
            .await
            .expect("the released preparation ended within a minute")
            .expect("the launcher is alive")
    }
}

fn holding_launcher() -> (super::runner::Launch, HeldPreparations) {
    let (entered_tx, entered) = tokio::sync::mpsc::unbounded_channel();
    let (ended_tx, ended) = tokio::sync::mpsc::unbounded_channel();
    let (release, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = Arc::new(std::sync::Mutex::new(release_rx));
    let stop_at_launch =
        Arc::new(std::sync::OnceLock::<tokio_util::sync::CancellationToken>::new());
    let (saw_stop_tx, saw_stop) = tokio::sync::mpsc::unbounded_channel();
    let stop_token = stop_at_launch.clone();
    let launch: super::runner::Launch = Arc::new(move |request, options| {
        assert!(
            !request.prompt.starts_with("panic-prep"),
            "the preparation was told to panic"
        );
        let prepared = provider::prepare(&request, &options);
        if request.prompt.starts_with("stop-at-launch") {
            let (stop, saw_stop) = (stop_token.clone(), saw_stop_tx.clone());
            return Box::new(move |cancel: tokio_util::sync::CancellationToken| {
                stop.get().expect("the test set the stop").cancel();
                let _ = saw_stop.send(cancel.is_cancelled());
                prepared.launch(cancel)
            });
        }
        if !request.prompt.starts_with("hold") {
            return Box::new(move |cancel| prepared.launch(cancel));
        }
        let _ = entered_tx.send(request.prompt.clone());
        // An error means the test dropped the sender: go on.
        let _ = release_rx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .recv();
        let mut tracker = Tracker {
            prompt: request.prompt.clone(),
            launched: false,
            tx: ended_tx.clone(),
        };
        Box::new(move |cancel| {
            tracker.launched = true;
            let handle = prepared.launch(cancel);
            drop(tracker);
            handle
        })
    });
    (
        launch,
        HeldPreparations {
            entered,
            release,
            ended,
            stop_at_launch,
            saw_stop,
        },
    )
}

/// A supervisor with `launch`, request checks that refuse nothing, and `bound`
/// for both the checks and the preparations.
fn preparing_supervisor(dir: &Dir, launch: super::runner::Launch, bound: Duration) -> Supervisor {
    use super::preflight::Preflight;
    let check = Arc::new(|_: &Request, _: &ProviderOptions| None);
    Supervisor::with_hooks(dir.config(), launch, Preflight::with(check, bound))
}

/// The MCP server runs the supervisor on a current-thread runtime, as this
/// test does. A run held while it is being prepared, as on a filesystem that
/// stopped answering after admission, holds only its own thread: another run
/// finishes, a third is interrupted, its worker closed, and `list` answers,
/// all while the first is held. Before the fix the preparation ran on the
/// runtime's only thread and every one of these hung; the watchdog, on a
/// thread of its own, turns that hang into a failure.
#[tokio::test]
async fn a_run_held_in_preparation_does_not_stop_other_runs_results_interrupt_close_or_list() {
    let _watchdog = watchdog(
        "a_run_held_in_preparation_does_not_stop_other_runs_results_interrupt_close_or_list",
    );
    let dir = Dir::new();
    let (launch, mut held) = holding_launcher();
    let supervisor = preparing_supervisor(&dir, launch, Duration::from_secs(3600));

    let slow = supervisor
        .start(dir.request("hold slow"))
        .await
        .expect("start");
    assert_eq!(held.entered().await, "hold slow");

    let other = supervisor
        .start(dir.request("echo:other"))
        .await
        .expect("start");
    let result = finished_result(&supervisor, &other.run).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");

    let gated = supervisor
        .start(dir.request(&format!("gate:{}", dir.gate().display())))
        .await
        .expect("start");
    dir.gate_started().await;
    let interrupted = supervisor
        .interrupt(Target::Run(gated.run.clone()))
        .await
        .expect("interrupt");
    assert!(interrupted.settled, "{interrupted:#?}");
    let result = finished_result(&supervisor, &gated.run).await;
    assert_eq!(result.outcome, Outcome::Interrupted, "{result:#?}");
    let closed = supervisor
        .close(&gated.worker.expect("worker"))
        .await
        .expect("close");
    assert_eq!(closed.state, WorkerState::Closed, "{closed:#?}");

    let list = supervisor.list().await.expect("list");
    assert_eq!(list.limits.running_runs, 1, "{list:#?}");
    let read = supervisor
        .read(Target::Run(slow.run.clone()), Detail::Compact, None, None)
        .await
        .expect("read");
    assert_eq!(read.state, Some(RunState::Running), "{read:#?}");

    // Released, the held run starts and completes as usual.
    let (prompt, launched) = held.release_one().await;
    assert_eq!(prompt, "hold slow");
    assert!(launched, "the released preparation did not start its run");
    let result = finished_result(&supervisor, &slow.run).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    supervisor.shutdown().await;
    dir.assert_nothing_left().await;
}

/// A preparation still held when the bound passes ends its run refused with
/// `preflight_timeout`, carrying what any refusal before start carries: no
/// start time, no duration, a confirmed cleanup. When the preparation returns
/// later, nothing is started. The bound is short, and the preparation is held
/// on a message until after the run has finished, so it passes the bound
/// however loaded the machine is. The same bound covers the admission check;
/// on a loaded machine that can time out first, and the start is repeated
/// until it is admitted, which the check's instant answer decides, not speed.
#[tokio::test]
async fn a_preparation_past_the_bound_refuses_the_run_preflight_timeout_and_starts_nothing_later() {
    let dir = Dir::new();
    let (launch, mut held) = holding_launcher();
    let supervisor = preparing_supervisor(&dir, launch, Duration::from_millis(300));

    let mut admitted = None;
    for _ in 0..3000 {
        let started = supervisor
            .start(dir.request("hold late"))
            .await
            .expect("start");
        match started.error.as_ref().map(|e| e.kind.as_str()) {
            None => {
                admitted = Some(started);
                break;
            }
            // Refused at admission: the run made no worker and was never
            // dispatched, so nothing was prepared or held.
            Some("preflight_timeout") if started.worker.is_none() => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Some(other) => panic!("unexpected refusal {other}: {started:#?}"),
        }
    }
    let started = admitted.expect("the start is admitted");
    assert_eq!(held.entered().await, "hold late");
    let result = finished_result(&supervisor, &started.run).await;
    assert_eq!(result.outcome, Outcome::Refused, "{result:#?}");
    let error = result.error.as_ref().expect("error");
    assert_eq!(error.kind, "preflight_timeout", "{error:?}");
    assert!(
        error
            .message
            .starts_with("preparing the run did not finish"),
        "{error:?}"
    );
    assert_eq!(result.timing.started_at, None, "{result:#?}");
    assert_eq!(result.timing.duration_ms, 0, "{result:#?}");
    assert_eq!(result.cleanup, Cleanup::Confirmed, "{result:#?}");
    assert_eq!(
        result.worker.as_ref().map(|w| &w.id),
        started.worker.as_ref()
    );
    let list = supervisor.list().await.expect("list");
    assert_eq!(list.limits.running_runs, 0, "the refused run kept its slot");

    let (prompt, launched) = held.release_one().await;
    assert_eq!(prompt, "hold late");
    assert!(
        !launched,
        "a preparation that returned after the bound started its run"
    );
    supervisor.shutdown().await;
    dir.assert_nothing_left().await;
}

/// An interrupt, a close and a shutdown each release a run that is still being
/// prepared: it ends `interrupted` with no start time, and its preparation,
/// when it returns, starts nothing.
#[tokio::test]
async fn interrupt_close_and_shutdown_during_preparation_end_the_run_interrupted_and_start_nothing()
{
    let dir = Dir::new();
    let (launch, mut held) = holding_launcher();
    let supervisor = preparing_supervisor(&dir, launch, Duration::from_secs(3600));

    let assert_interrupted = |result: &crate::contract::result::RunResult| {
        assert_eq!(result.outcome, Outcome::Interrupted, "{result:#?}");
        assert_eq!(result.timing.started_at, None, "{result:#?}");
        assert_eq!(result.cleanup, Cleanup::Confirmed, "{result:#?}");
    };

    let first = supervisor
        .start(dir.request("hold interrupt"))
        .await
        .expect("start");
    assert_eq!(held.entered().await, "hold interrupt");
    let interrupted = supervisor
        .interrupt(Target::Run(first.run.clone()))
        .await
        .expect("interrupt");
    assert!(interrupted.settled, "{interrupted:#?}");
    assert!(!interrupted.was_queued, "{interrupted:#?}");
    assert_interrupted(&finished_result(&supervisor, &first.run).await);
    assert_eq!(
        held.release_one().await,
        ("hold interrupt".to_string(), false)
    );

    let second = supervisor
        .start(dir.request("hold close"))
        .await
        .expect("start");
    assert_eq!(held.entered().await, "hold close");
    let closed = supervisor
        .close(second.worker.as_deref().expect("worker"))
        .await
        .expect("close");
    assert!(closed.settled, "{closed:#?}");
    assert_eq!(closed.stopped_run.as_deref(), Some(second.run.as_str()));
    assert_interrupted(&finished_result(&supervisor, &second.run).await);
    assert_eq!(held.release_one().await, ("hold close".to_string(), false));

    let third = supervisor
        .start(dir.request("hold shutdown"))
        .await
        .expect("start");
    assert_eq!(held.entered().await, "hold shutdown");
    let report = supervisor.shutdown().await;
    assert_eq!(report.runs_stopped, 1, "{report:#?}");
    assert!(report.unsettled.is_empty(), "{report:#?}");
    let read = supervisor
        .read(Target::Run(third.run.clone()), Detail::Full, None, None)
        .await
        .expect("read");
    let Some(ResultView::Full(result)) = read.result else {
        panic!("{read:#?}");
    };
    assert_eq!(result.validate(), Ok(()), "{result:#?}");
    assert_interrupted(&result);
    assert_eq!(
        held.release_one().await,
        ("hold shutdown".to_string(), false)
    );
    dir.assert_nothing_left().await;
}

/// The error message of a result, which every non-completed result has.
fn error_message(result: &crate::contract::result::RunResult) -> &str {
    &result.error.as_ref().expect("error").message
}

/// A preparation whose answer is waiting when the runtime next polls it, but
/// only at its deadline, is refused `preflight_timeout`, and the answer is
/// dropped unused. Tokio's `timeout_at` polls the answer before its timer, so
/// without a check of the deadline itself the answer won and the run launched
/// after its bound. Time is paused and moves only when the test advances it;
/// the answer is known to be waiting from the thread's own signal.
#[tokio::test(start_paused = true)]
async fn a_preparation_whose_answer_is_ready_only_at_its_deadline_is_refused_and_starts_nothing() {
    use super::preflight::{Preflight, Unprepared};
    use std::future::Future;
    use std::task::Poll;

    const BOUND: Duration = Duration::from_secs(10);
    let (answered_tx, answered) = std::sync::mpsc::channel();
    let preflight = Preflight::with(Arc::new(|_: &Request, _: &ProviderOptions| None), BOUND)
        .signal_answers(answered_tx);
    let (release, release_rx) = std::sync::mpsc::channel::<()>();
    let (ended_tx, mut ended) = tokio::sync::mpsc::unbounded_channel();
    let work = move || {
        let _ = release_rx.recv();
        Tracker {
            prompt: "late".to_string(),
            launched: false,
            tx: ended_tx,
        }
    };
    let stop = tokio_util::sync::CancellationToken::new();
    let deadline = preflight.deadline();
    let prepare = preflight.prepare(work, &stop, deadline);
    tokio::pin!(prepare);
    // One poll takes a slot, starts the thread and waits for its answer.
    let pending =
        std::future::poll_fn(|cx| Poll::Ready(prepare.as_mut().poll(cx).is_pending())).await;
    assert!(pending, "the preparation answered while it was held");

    // The deadline arrives while the preparation is not polled; then the
    // answer is sent, and only then is the preparation polled again.
    tokio::time::advance(BOUND).await;
    release.send(()).expect("the preparation is held");
    answered
        .recv_timeout(Duration::from_secs(60))
        .expect("the preparation's thread sent its answer");
    match prepare.await {
        Err(Unprepared::Refused((kind, message))) => {
            assert_eq!(kind, "preflight_timeout", "{message}");
            assert!(
                message.starts_with("preparing the run did not finish"),
                "{message}"
            );
        }
        Err(Unprepared::Stopped) => panic!("the preparation was released, not timed out"),
        Ok(_) => panic!("an answer at the deadline was taken"),
    }
    assert_eq!(
        ended.try_recv(),
        Ok(("late".to_string(), false)),
        "the late answer was not dropped unused"
    );
}

/// A preparation whose deadline, taken at dispatch, has passed before its task
/// first runs is refused without starting its thread, though a slot is free.
#[tokio::test(start_paused = true)]
async fn a_preparation_whose_deadline_passed_before_it_ran_never_starts_its_thread() {
    use super::limits::MAX_PENDING_PREFLIGHTS;
    use super::preflight::{Preflight, Unprepared};

    const BOUND: Duration = Duration::from_secs(10);
    let preflight = Preflight::with(Arc::new(|_: &Request, _: &ProviderOptions| None), BOUND);
    let deadline = preflight.deadline();
    tokio::time::advance(BOUND).await;
    let (ran_tx, ran) = std::sync::mpsc::channel::<()>();
    let work = move || {
        let _ = ran_tx.send(());
    };
    let stop = tokio_util::sync::CancellationToken::new();
    match preflight.prepare(work, &stop, deadline).await {
        Err(Unprepared::Refused((kind, message))) => {
            assert_eq!(kind, "preflight_timeout", "{message}");
        }
        other => panic!("a preparation past its deadline went ahead: {other:?}"),
    }
    // The work was dropped without running: its sender is gone, unsent.
    assert_eq!(ran.recv(), Err(std::sync::mpsc::RecvError));
    assert_eq!(
        preflight.slots().available_permits(),
        MAX_PENDING_PREFLIGHTS
    );
}

/// The token a prepared run's launch receives is a child of the run's stop, so
/// a stop that lands while the run is being launched is already seen by that
/// token, and reaches the provider at once; the run ends `interrupted`. Before
/// the fix the launch made a fresh token, and the stop reached the provider
/// only when the run's task forwarded it.
#[tokio::test]
async fn a_stop_that_lands_as_a_prepared_run_launches_is_seen_by_the_launch_token() {
    let dir = Dir::new();
    let (launch, mut held) = holding_launcher();
    let supervisor = preparing_supervisor(&dir, launch, Duration::from_secs(3600));
    held.stop_at_launch
        .set(supervisor.shared.root.clone())
        .expect("set once");

    let started = supervisor
        .start(dir.request("stop-at-launch"))
        .await
        .expect("start");
    let saw = tokio::time::timeout(Duration::from_secs(60), held.saw_stop.recv())
        .await
        .expect("the run launched within a minute")
        .expect("the launcher is alive");
    assert!(saw, "the launch's token did not see the run's stop");
    let result = finished_result(&supervisor, &started.run).await;
    assert_eq!(result.outcome, Outcome::Interrupted, "{result:#?}");
    supervisor.shutdown().await;
    dir.assert_nothing_left().await;
}

/// A panic in the preparation itself, on its thread, ends the run refused with
/// kind `internal_error` and gives its slot back; the supervisor goes on.
#[tokio::test]
async fn a_panic_in_preparation_refuses_the_run_internal_error_and_frees_its_slot() {
    use super::limits::MAX_PENDING_PREFLIGHTS;

    let dir = Dir::new();
    let (launch, _held) = holding_launcher();
    let supervisor = preparing_supervisor(&dir, launch, Duration::from_secs(3600));

    let started = supervisor
        .start(dir.request("panic-prep"))
        .await
        .expect("start");
    let result = finished_result(&supervisor, &started.run).await;
    assert_eq!(result.outcome, Outcome::Refused, "{result:#?}");
    let error = result.error.as_ref().expect("error");
    assert_eq!(error.kind, "internal_error", "{error:?}");
    assert_eq!(error.message, "preparing the run failed unexpectedly");
    assert_eq!(result.timing.started_at, None, "{result:#?}");
    assert_eq!(result.cleanup, Cleanup::Confirmed, "{result:#?}");

    // Every slot can be taken: the panicked thread gave its slot back. The
    // timeout only ends a test that would otherwise hang.
    let all = tokio::time::timeout(
        Duration::from_secs(60),
        supervisor
            .shared
            .preflight
            .slots()
            .acquire_many_owned(u32::try_from(MAX_PENDING_PREFLIGHTS).expect("slot count")),
    )
    .await
    .expect("every slot is free")
    .expect("the slots are open");
    drop(all);

    let after = supervisor
        .start(dir.request("echo:after"))
        .await
        .expect("start");
    let result = finished_result(&supervisor, &after.run).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    supervisor.shutdown().await;
    dir.assert_nothing_left().await;
}

/// A follow-up queued behind a run that is being prepared waits its turn. An
/// interrupt of the preparing run lets the queue go on: the follow-up is
/// dispatched, and refused `no_session` because the interrupted run reported
/// none. A close discards the follow-up with the preparing run.
#[tokio::test]
async fn interrupting_a_preparing_run_lets_its_queued_follow_up_go_and_closing_discards_it() {
    let dir = Dir::new();
    let (launch, mut held) = holding_launcher();
    let supervisor = preparing_supervisor(&dir, launch, Duration::from_secs(3600));

    let first = supervisor
        .start(dir.request("hold first"))
        .await
        .expect("start");
    assert_eq!(held.entered().await, "hold first");
    let worker = first.worker.clone().expect("worker");
    let queued = supervisor
        .follow_up(&worker, "echo:queued", None)
        .await
        .expect("follow up");
    assert_eq!(queued.state, RunState::Queued, "{queued:#?}");
    let interrupted = supervisor
        .interrupt(Target::Run(first.run.clone()))
        .await
        .expect("interrupt");
    assert!(interrupted.settled, "{interrupted:#?}");
    let result = finished_result(&supervisor, &queued.run).await;
    assert_eq!(result.outcome, Outcome::Refused, "{result:#?}");
    assert_eq!(result.error.as_ref().expect("error").kind, "no_session");
    assert_eq!(held.release_one().await, ("hold first".to_string(), false));

    let second = supervisor
        .start(dir.request("hold second"))
        .await
        .expect("start");
    assert_eq!(held.entered().await, "hold second");
    let worker = second.worker.clone().expect("worker");
    let discarded = supervisor
        .follow_up(&worker, "echo:discarded", None)
        .await
        .expect("follow up");
    assert_eq!(discarded.state, RunState::Queued, "{discarded:#?}");
    let closed = supervisor.close(&worker).await.expect("close");
    assert!(closed.settled, "{closed:#?}");
    assert_eq!(
        closed.discarded,
        std::slice::from_ref(&discarded.run),
        "{closed:#?}"
    );
    assert_eq!(closed.stopped_run.as_deref(), Some(second.run.as_str()));
    let result = finished_result(&supervisor, &discarded.run).await;
    assert_eq!(result.outcome, Outcome::Interrupted, "{result:#?}");
    assert!(
        error_message(&result).contains("discarded by close"),
        "{result:#?}"
    );
    let result = finished_result(&supervisor, &second.run).await;
    assert_eq!(result.outcome, Outcome::Interrupted, "{result:#?}");
    assert_eq!(held.release_one().await, ("hold second".to_string(), false));
    supervisor.shutdown().await;
    dir.assert_nothing_left().await;
}

/// `shutdown` closes the request checks before its `Shutdown` reaches the
/// supervisor's task. That close does not release a preparation, so it cannot
/// end a run, and dispatch the next, ahead of the shutdown: here the held run
/// is released by the test after the close and completes. Before the fix the
/// close ended it `interrupted`.
#[tokio::test]
async fn closing_the_request_checks_does_not_release_a_preparation() {
    let dir = Dir::new();
    let (launch, mut held) = holding_launcher();
    let supervisor = preparing_supervisor(&dir, launch, Duration::from_secs(3600));

    let started = supervisor
        .start(dir.request("hold through close"))
        .await
        .expect("start");
    assert_eq!(held.entered().await, "hold through close");
    supervisor.shared.preflight.close();
    assert_eq!(
        held.release_one().await,
        ("hold through close".to_string(), true)
    );
    let result = finished_result(&supervisor, &started.run).await;
    assert_eq!(result.outcome, Outcome::Completed, "{result:#?}");
    supervisor.shutdown().await;
    dir.assert_nothing_left().await;
}

/// On a multi-threaded runtime, a shutdown during a preparation closes the
/// worker and discards its queued follow-up before the preparing run is
/// released, and counts both. The preparation is released only by the run's
/// stop, which the supervisor's task cancels in the same command that discards
/// the queue, so no ordering of the runtime's threads can dispatch the
/// follow-up first.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shutdown_during_preparation_discards_the_queued_follow_up_and_counts_both() {
    let dir = Dir::new();
    let (launch, mut held) = holding_launcher();
    let supervisor = preparing_supervisor(&dir, launch, Duration::from_secs(3600));

    let started = supervisor
        .start(dir.request("hold until shutdown"))
        .await
        .expect("start");
    assert_eq!(held.entered().await, "hold until shutdown");
    let worker = started.worker.clone().expect("worker");
    let queued = supervisor
        .follow_up(&worker, "echo:queued", None)
        .await
        .expect("follow up");
    assert_eq!(queued.state, RunState::Queued, "{queued:#?}");

    let report = supervisor.shutdown().await;
    assert_eq!(report.runs_stopped, 1, "{report:#?}");
    assert_eq!(report.runs_discarded, 1, "{report:#?}");
    assert!(report.unsettled.is_empty(), "{report:#?}");
    for (run, says) in [
        (&queued.run, "discarded by shutdown"),
        (&started.run, "interrupted while the run was being prepared"),
    ] {
        let read = supervisor
            .read(Target::Run(run.clone()), Detail::Full, None, None)
            .await
            .expect("read");
        let Some(ResultView::Full(result)) = read.result else {
            panic!("{read:#?}");
        };
        assert_eq!(result.outcome, Outcome::Interrupted, "{result:#?}");
        assert!(error_message(&result).contains(says), "{result:#?}");
    }
    assert_eq!(
        held.release_one().await,
        ("hold until shutdown".to_string(), false)
    );
    dir.assert_nothing_left().await;
}
