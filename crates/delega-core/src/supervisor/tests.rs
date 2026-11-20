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
        .read(Target::Run(run.to_string()), Detail::Full, None, None)
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

/// Start a gated run and a `wait` for it that times out at once and is held
/// before it asks for its view. Returns the held wait and the gate that lets it
/// go on.
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
    let (reached, release) = supervisor.pause_next_wait_before_view();
    let waiting = {
        let supervisor = supervisor.clone();
        let targets = WaitTargets {
            runs: vec![started.run],
            workers: Vec::new(),
        };
        tokio::spawn(async move { supervisor.wait(targets, Some(0), None).await })
    };
    reached.await.expect("the wait reached its view");
    (waiting, release)
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
    let waiting = {
        let supervisor = supervisor.clone();
        let targets = WaitTargets {
            runs: vec![queued.run.clone()],
            workers: Vec::new(),
        };
        tokio::spawn(async move { supervisor.wait(targets, Some(300), None).await })
    };
    // Round trips through the supervisor's one task let the wait register.
    for _ in 0..20 {
        supervisor.list().await.expect("list");
        assert!(!waiting.is_finished(), "the wait returned before shutdown");
    }
    supervisor.shutdown().await;
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

    // Once the held checks end, a later start is admitted. Until their threads
    // have ended a start may still wait for a slot, or time out on a loaded
    // machine, and is repeated.
    released.store(true, Ordering::SeqCst);
    let mut admitted = None;
    for _ in 0..3000 {
        let later = supervisor
            .start(dir.request("echo:later"))
            .await
            .expect("start");
        match later.error.as_ref().map(|e| e.kind.as_str()) {
            None => {
                admitted = Some(later);
                break;
            }
            Some("preflight_timeout") => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Some(other) => panic!("unexpected refusal {other}: {later:#?}"),
        }
    }
    let admitted = admitted.expect("a start is admitted once the checks have ended");
    assert_eq!(
        finished_result(&supervisor, &admitted.run).await.outcome,
        Outcome::Completed
    );
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
