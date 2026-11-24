//! Runs through the keeper. The keeper is this test binary itself, started
//! again with a filter that runs only [`keeper_entry`] and a variable that
//! makes it act as the keeper; its standard output, where the test harness
//! writes, is the null device.

use std::time::Duration;

use super::fixtures::{
    OWN_GROUP, ProcessIdentity, Reaper, Scratch, assert_confirmed, assert_gone, is_running,
    own_group_recorded, own_session_recorded, perl_record_self, run_guarded, sh,
};
use crate::contract::result::Cleanup;
use crate::process::sys::Signal;
use crate::process::{Ending, KeeperCommand, ProcessHandle, ProcessSpec};

/// Set in the keeper's environment to make [`keeper_entry`] run the keeper.
const ENTRY_ENV: &str = "DELEGA_TEST_KEEPER_ENTRY";

/// The keeper for tests: this test binary, running [`keeper_entry`] only.
pub(super) fn test_keeper() -> KeeperCommand {
    let exe = std::env::current_exe().expect("the test binary's path");
    KeeperCommand::new(
        exe,
        [
            "--exact",
            "process::tests::keeper::keeper_entry",
            "--test-threads=1",
            "-q",
        ],
    )
    .env(ENTRY_ENV, "1")
}

/// An instance identifiable by its private directory on the command line.
fn named_keeper(dir: &std::path::Path) -> KeeperCommand {
    let mut keeper = test_keeper();
    let path = dir.to_str().expect("a scratch directory named in UTF-8");
    keeper.args.extend(["--skip".to_owned(), path.to_owned()]);
    keeper
}

fn keeper_identity(pid: i32, dir: &std::path::Path) -> Option<ProcessIdentity> {
    let identity = ProcessIdentity::capture(pid)?;
    super::fixtures::process_names_path(pid, dir.as_os_str().as_encoded_bytes()).then_some(identity)
}

/// A spec run through the test keeper.
pub(super) fn kept(spec: ProcessSpec) -> ProcessSpec {
    let mut spec = spec;
    spec.keeper = Some(test_keeper());
    spec
}

/// Not a test: the keeper's entry point when this binary is started as one.
#[test]
fn keeper_entry() {
    if std::env::var_os(ENTRY_ENV).is_none() {
        return;
    }
    std::process::exit(crate::process::keeper_main());
}

/// Set to a number of seconds to make [`sleeper_entry`] sleep that long.
pub(super) const SLEEP_ENV: &str = "DELEGA_TEST_SLEEP_SECS";

/// Not a test: a process that sleeps, when this binary is started as one. It
/// stands in for `sleep` where a test needs a program whose environment `ps -E`
/// shows on macOS, which it does not for Apple's own programs.
#[test]
fn sleeper_entry() {
    let Some(secs) = std::env::var(SLEEP_ENV).ok().and_then(|v| v.parse().ok()) else {
        return;
    };
    std::thread::sleep(Duration::from_secs(secs));
    std::process::exit(0);
}

/// A shell command that starts a process in a session of its own, like
/// `own_session_recorded`, running this test binary as a 60 s sleeper instead
/// of `sleep`, so the run marker in its environment is visible on macOS too.
/// It publishes its own pid at `record` once it leads its own session.
pub(super) fn own_session_sleeper(record: &str) -> String {
    let exe = std::env::current_exe().expect("the test binary's path");
    format!(
        "perl -e 'use POSIX; {} {} $ENV{{{SLEEP_ENV}}} = 60; \
         exec q({}), q(--exact), q(process::tests::keeper::sleeper_entry), q(--test-threads=1), q(-q)'",
        super::fixtures::PERL_LEAVE_SESSION,
        perl_record_self(record),
        exe.display()
    )
}

const LONG: Duration = Duration::from_secs(60);

#[tokio::test]
async fn a_kept_run_reports_the_programs_exit_code_and_output() {
    let s = Scratch::new();
    let spec = kept(sh(&s, "echo out; echo err >&2; exit 3", LONG));
    let report = ProcessHandle::start(spec).wait().await;
    assert_eq!(report.ending, Ending::Exited { code: 3 }, "{report:#?}");
    assert_eq!(report.stdout.text(), "out\n");
    assert_eq!(report.stderr.text(), "err\n");
    assert_confirmed(&report);
    let keeper = report
        .diagnostics
        .keeper
        .expect("a kept run reports its keeper");
    assert!(keeper.pid.is_some() && !keeper.lost, "{keeper:?}");
    #[cfg(target_os = "linux")]
    assert!(keeper.subreaper, "{keeper:?}");
}

#[tokio::test]
async fn a_kept_run_passes_standard_input_to_the_program() {
    let s = Scratch::new();
    let spec = kept(sh(&s, "cat", LONG)).stdin("from the owner");
    let report = ProcessHandle::start(spec).wait().await;
    assert_eq!(report.ending, Ending::Exited { code: 0 }, "{report:#?}");
    assert_eq!(report.stdout.text(), "from the owner");
}

#[tokio::test]
async fn a_run_without_a_keeper_says_so() {
    let s = Scratch::new();
    let report = ProcessHandle::start(sh(&s, "exit 0", LONG)).wait().await;
    assert_eq!(report.diagnostics.keeper, None);
    assert_confirmed(&report);
}

#[tokio::test]
async fn a_kept_program_that_cannot_start_fails_to_start_as_it_would_directly() {
    let s = Scratch::new();
    let mut spec = kept(sh(&s, "exit 0", LONG));
    spec.program = "/nonexistent/delega-test-program".into();
    let report = ProcessHandle::start(spec).wait().await;
    match &report.ending {
        Ending::FailedToStart { message } => assert!(
            message.contains("/nonexistent/delega-test-program"),
            "{message}"
        ),
        other => panic!("{other:?}"),
    }
    assert_eq!(report.cleanup, Cleanup::Confirmed);
}

#[tokio::test]
async fn a_keeper_that_cannot_start_refuses_the_run_and_starts_nothing() {
    let s = Scratch::new();
    let mut spec = sh(&s, "echo started > started; sleep 30", LONG);
    spec.keeper = Some(KeeperCommand::new("/nonexistent/delega-keeper", ["x"]));
    let report = ProcessHandle::start(spec).wait().await;
    match &report.ending {
        Ending::FailedToStart { message } => {
            assert!(message.contains("keeper"), "{message}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(report.cleanup, Cleanup::Confirmed);
    assert!(!s.path("started").exists());
}

#[tokio::test]
async fn a_kept_run_reports_a_crash_by_its_signal() {
    let s = Scratch::new();
    let spec = kept(sh(&s, "kill -SEGV $$", LONG));
    let report = ProcessHandle::start(spec).wait().await;
    assert!(
        matches!(report.ending, Ending::Signaled { signal: 11, .. }),
        "{report:#?}"
    );
}

#[tokio::test]
async fn a_kept_run_that_times_out_is_stopped_with_its_group() {
    let s = Scratch::new();
    let script = format!("{OWN_GROUP} & record gc $!; record leader $$; sleep 60");
    let spec = kept(sh(&s, &script, Duration::from_millis(800)));
    let (report, pids, _reaper) = run_guarded(&s, spec, &["gc", "leader"]).await;
    assert!(
        matches!(report.ending, Ending::TimedOut { .. }),
        "{report:#?}"
    );
    assert_confirmed(&report);
    assert_gone("descendants", &pids).await;
}

#[tokio::test]
async fn an_interrupted_kept_run_stops_a_setsid_descendant() {
    require_perl!();
    let s = Scratch::new();
    // The descendant publishes `gc` once it has left the session, so the
    // interrupt comes after that.
    let script = format!("{} >/dev/null 2>&1 & sleep 60", own_session_recorded("gc"));
    let spec = kept(sh(&s, &script, LONG));
    let handle = ProcessHandle::start(spec);
    let mut reaper = Reaper::default();
    let gc = reaper.watch(&s, "gc").await;
    handle.interrupt();
    let report = handle.wait().await;
    assert_eq!(report.ending, Ending::Interrupted, "{report:#?}");
    assert_confirmed(&report);
    assert_gone("setsid descendant", &[gc]).await;
}

/// A process the run did not start, carrying another run's marker value, is
/// never signalled, not even stopped: the marker matches one run's value
/// exactly. The process is this test binary sleeping, whose environment `ps
/// -E` shows on every machine (Apple's own programs may hide it), and it is
/// watched through a plain run and through an interrupted one whose hard
/// stop is a freeze.
#[tokio::test]
async fn a_process_with_another_runs_marker_is_never_signalled() {
    use crate::process::MARKER_ENV;
    use crate::test_spawn::Guarded;
    let exe = std::env::current_exe().expect("the test binary's path");
    let mut other = std::process::Command::new(exe)
        .args([
            "--exact",
            "process::tests::keeper::sleeper_entry",
            "--test-threads=1",
            "-q",
        ])
        .env(SLEEP_ENV, "60")
        .env(MARKER_ENV, "0123456789abcdef0123456789abcdef")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .guarded_spawn()
        .expect("start a foreign process");
    let pid = i32::try_from(other.id()).expect("a pid");
    let _reaper = Reaper::new([pid]);
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watching = done.clone();
    // Stops the watcher however the test ends.
    let stop_watching = super::fixtures::SetOnDrop(done.clone());
    let (first_sample, sampled) = tokio::sync::oneshot::channel();
    let watcher = tokio::task::spawn_blocking(move || {
        let mut states = Vec::new();
        let mut first_sample = Some(first_sample);
        while !watching.load(std::sync::atomic::Ordering::SeqCst) {
            states.push(super::fixtures::process_state(pid));
            if let Some(first) = first_sample.take() {
                let _ = first.send(());
            }
            // How often it samples, not a wait for anything.
            std::thread::sleep(Duration::from_millis(5));
        }
        states
    });
    // The runs start only once the watcher is watching.
    sampled.await.expect("the watcher took its first sample");
    let s = Scratch::new();
    let report = ProcessHandle::start(kept(sh(&s, "exit 0", LONG)))
        .wait()
        .await;
    assert_confirmed(&report);
    let mut spec = kept(sh(&s, "record leader $$; sleep 60", LONG));
    spec.stop_grace = Duration::from_millis(100);
    let handle = ProcessHandle::start(spec);
    let _ = s.pid("leader").await;
    handle.interrupt();
    let report = handle.wait().await;
    assert_eq!(report.ending, Ending::Interrupted, "{report:#?}");
    drop(stop_watching);
    let states = watcher.await.expect("the watcher ended");
    assert!(is_running(pid), "a foreign marked process was signalled");
    assert!(
        states.iter().all(|state| !state.starts_with('T')),
        "a foreign marked process was stopped: {states:?}"
    );
    let _ = other.kill();
    let _ = other.wait();
}

/// Set to a scratch directory to make [`owner_entry`] act as an owner.
const OWNER_ENV: &str = "DELEGA_TEST_OWNER_DIR";
/// Set as well to make the owner's program the fork relay.
const OWNER_RELAY_ENV: &str = "DELEGA_TEST_OWNER_RELAY";

/// Not a test: an owning process, when this binary is started as one. It runs
/// one kept program in the directory named by [`OWNER_ENV`] that starts a
/// descendant in its own group, one in a session of its own, and one that
/// ignores TERM, and then waits; the test kills it with SIGKILL.
#[test]
fn owner_entry() {
    let Some(dir) = std::env::var_os(OWNER_ENV) else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let script = if std::env::var_os(OWNER_RELAY_ENV).is_some() {
        // The program is the fork relay of gaps.rs, run long enough that only
        // a stop explains its end.
        format!("record leader $$; {}", super::gaps::long_relay(&dir))
    } else {
        // The group and session descendants publish their own records once
        // they have left the group and the session.
        format!(
            "record leader $$; {} & \
             {} >/dev/null 2>&1 & \
             sh -c 'trap \"\" TERM; echo $$ > stubborn; while :; do sleep 1; done' & \
             sleep 60",
            own_group_recorded("group"),
            own_session_sleeper("session")
        )
    };
    let script = format!("{}{script}", super::fixtures::RECORD);
    let mut spec = ProcessSpec::new("/bin/sh", LONG)
        .args(["-c", &script])
        .cwd(&dir)
        .env(crate::process::EnvPolicy::inherit_only(["PATH"]));
    spec.keeper = Some(named_keeper(&dir));
    spec.stop_grace = Duration::from_millis(500);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let _ = runtime.block_on(crate::process::run(spec));
    std::process::exit(0);
}

/// The parent of `pid`, from `ps`.
fn parent_of(pid: i32) -> Option<i32> {
    use crate::test_spawn::Guarded;
    let out = std::process::Command::new("/bin/ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .guarded_output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// Gap 3: the owner is killed with SIGKILL, so no code of the owner runs. The
/// keeper sees the owner's end of its socket close and stops the whole tree,
/// TERM, the grace, then KILL, and exits.
#[tokio::test]
async fn sigkill_of_the_owner_leaves_nothing_running() {
    require_perl!();
    use crate::test_spawn::Guarded;
    let s = Scratch::new();
    let exe = std::env::current_exe().expect("the test binary's path");
    let mut owner = std::process::Command::new(exe)
        .args([
            "--exact",
            "process::tests::keeper::owner_entry",
            "--test-threads=1",
            "-q",
        ])
        .env(OWNER_ENV, s.dir())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .guarded_spawn()
        .expect("start the owner");
    let owner_pid = i32::try_from(owner.id()).expect("a pid");
    let mut reaper = Reaper::default();
    let leader = reaper.watch(&s, "leader").await;
    let keeper = parent_of(leader).expect("the program's parent");
    assert_ne!(keeper, owner_pid, "the program's parent must be the keeper");
    reaper.add(keeper_identity(keeper, &s.dir()));
    let mut pids = vec![leader];
    for file in ["group", "session", "stubborn"] {
        pids.push(reaper.watch(&s, file).await);
    }
    pids.push(keeper);
    let _ = crate::process::sys::signal_process(owner_pid, Signal::KILL);
    let _ = owner.wait();
    assert_gone("the program, its descendants and the keeper", &pids).await;
}

/// The accepted residual on every platform: SIGKILL aimed at the keeper. The
/// program's pid stops pinning its group once the keeper is gone, so the owner
/// sends no more group signals, and the verdict says why it cannot confirm.
#[tokio::test]
async fn a_keeper_killed_during_the_run_makes_the_verdict_unconfirmed() {
    let s = Scratch::new();
    let mut spec = sh(&s, "record leader $$; sleep 60", LONG);
    spec.keeper = Some(named_keeper(&s.dir()));
    let handle = ProcessHandle::start(spec);
    let mut reaper = Reaper::default();
    let leader = reaper.watch(&s, "leader").await;
    let keeper = parent_of(leader).expect("the program's parent");
    let keeper = keeper_identity(keeper, &s.dir()).expect("identify the keeper");
    assert!(keeper.signal(Signal::KILL), "signal the keeper");
    let report = handle.wait().await;
    super::fixtures::assert_documented(&report.cleanup);
    match &report.cleanup {
        Cleanup::Unconfirmed { reason, .. } => {
            assert!(reason.contains("keeper"), "{reason}");
        }
        other => panic!("a lost keeper must not confirm cleanup: {other:?}"),
    }
    assert!(
        report.diagnostics.keeper.as_ref().is_some_and(|k| k.lost),
        "{report:#?}"
    );
}

/// The cost of a keeper per run: 40 runs of `/usr/bin/true` owned directly and
/// 40 through the keeper named by `DELEGA_KEEPER_BIN` (a `delega` binary, run
/// as `delega process-keeper`), the median and the 90th percentile of each,
/// from start to report. Run with `DELEGA_KEEPER_BIN=target/release/delega
/// cargo test --release -- --ignored measure_the_keepers_cost --nocapture`.
#[tokio::test]
#[ignore = "a measurement, not a check"]
async fn measure_the_keepers_cost() {
    let Some(bin) = std::env::var_os("DELEGA_KEEPER_BIN") else {
        eprintln!("DELEGA_KEEPER_BIN is not set; nothing measured");
        return;
    };
    let bin = std::fs::canonicalize(bin).expect("the keeper binary");
    let mut results = Vec::new();
    for kept_run in [false, true] {
        let mut took = Vec::new();
        for _ in 0..40 {
            let mut spec = ProcessSpec::new("/usr/bin/true", LONG);
            if kept_run {
                spec.keeper = Some(KeeperCommand::new(&bin, ["process-keeper"]));
            }
            let began = std::time::Instant::now();
            let report = ProcessHandle::start(spec).wait().await;
            took.push(began.elapsed());
            assert_eq!(report.ending, Ending::Exited { code: 0 }, "{report:#?}");
        }
        took.sort();
        results.push((kept_run, took[20], took[36]));
    }
    for (kept_run, median, p90) in results {
        eprintln!(
            "{}: median {median:?}, p90 {p90:?}",
            if kept_run { "kept" } else { "direct" }
        );
    }
}

/// A keeper that ends without saying whether it started the program may have
/// started it: the run fails to start, and its cleanup is not confirmed.
#[tokio::test]
async fn a_keeper_that_ends_before_answering_leaves_cleanup_unconfirmed() {
    let s = Scratch::new();
    let mut spec = sh(&s, "exit 0", LONG);
    // It reads the start of the hello, so the hello was handed over in full,
    // and exits without a word. (A keeper that exits before the hello is
    // written cannot have started anything, and that run is `Confirmed`.)
    spec.keeper = Some(KeeperCommand::new(
        "/bin/sh",
        ["-c", "/usr/bin/head -c 1 >/dev/null; exit 0"],
    ));
    let report = ProcessHandle::start(spec).wait().await;
    assert!(
        matches!(report.ending, Ending::FailedToStart { .. }),
        "{report:#?}"
    );
    match &report.cleanup {
        Cleanup::Unconfirmed { reason, .. } => {
            assert_eq!(reason, crate::process::KEEPER_UNANSWERED_REASON);
        }
        other => panic!("{other:?}"),
    }
}

/// SIGTERM to the keeper (`pkill delega` matches it) has it stop the tree by
/// itself. Before it reaps the program it says `releasing` and waits for the
/// owner's answer, so no group signal of the owner can follow the reap; the
/// verdict says the keeper ended before the owner had the program reaped.
#[tokio::test]
async fn a_keeper_stopped_by_a_signal_releases_the_program_before_reaping_it() {
    let s = Scratch::new();
    let mut spec = sh(&s, "record leader $$; sleep 60", LONG);
    spec.keeper = Some(named_keeper(&s.dir()));
    let handle = ProcessHandle::start(spec);
    let mut reaper = Reaper::default();
    let leader = reaper.watch(&s, "leader").await;
    let keeper = parent_of(leader).expect("the program's parent");
    let keeper = keeper_identity(keeper, &s.dir()).expect("identify the keeper");
    assert!(keeper.signal(Signal::TERM), "signal the keeper");
    let report = handle.wait().await;
    assert_gone("the program", &[leader]).await;
    match &report.cleanup {
        Cleanup::Unconfirmed { reason, .. } => {
            assert_eq!(reason, crate::process::KEEPER_LOST_REASON, "{report:#?}");
        }
        other => panic!("a released program must not confirm cleanup: {other:?}"),
    }
    assert!(
        report
            .diagnostics
            .warnings
            .iter()
            .any(|w| w.contains("stopped the program on its own")),
        "{:?}",
        report.diagnostics.warnings
    );
}

/// Gap 3 with a fork relay, on Linux, where the keeper's own cleanup must
/// leave nothing after a SIGKILL of the owner: its census and freeze walk the
/// tree again from the keeper's own children until a pass finds nothing new
/// with every process stopped, so a generation reparented to the keeper
/// between two reads is not missed. The race is a matter of timing, so the
/// case is repeated 20 times; any surviving generation fails it. The owner is
/// killed once the relay has made one handoff. The relay's lifeline says when
/// its last process is gone and whether it ran its course or broke by itself
/// first (see `gaps::RELAY`); the relay outlives any stop by many seconds.
/// The keeper holds no lifeline and goes on after the relay is gone (it
/// reaps and exits), so its exit is awaited on its own pidfd; the census of
/// processes naming the case directory, which also finds the keeper (the
/// directory is on its command line), is taken after that. One deadline
/// bounds each attempt.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn sigkill_of_the_owner_stops_a_fork_relay_every_time_on_linux() {
    require_perl!();
    use super::fixtures::AWAIT;
    use super::gaps::{RelayFate, RelayLife};
    use crate::test_spawn::Guarded;
    for attempt in 1..=20 {
        let s = Scratch::new();
        let mut relay = RelayLife::new(&s);
        let exe = std::env::current_exe().expect("the test binary's path");
        let mut owner = std::process::Command::new(exe)
            .args([
                "--exact",
                "process::tests::keeper::owner_entry",
                "--test-threads=1",
                "-q",
            ])
            .env(OWNER_ENV, s.dir())
            .env(OWNER_RELAY_ENV, "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .guarded_spawn()
            .expect("start the owner");
        let owner_pid = i32::try_from(owner.id()).expect("a pid");
        let mut reaper = Reaper::default();
        let leader = s.pid("leader").await;
        let keeper_pid = parent_of(leader).expect("the program's parent");
        assert_ne!(
            keeper_pid, owner_pid,
            "the program's parent must be the keeper"
        );
        let keeper = keeper_identity(keeper_pid, &s.dir()).expect("identify the keeper");
        reaper.add(keeper_identity(keeper_pid, &s.dir()));
        relay.await_handoffs(1).await;
        let _ = crate::process::sys::signal_process(owner_pid, Signal::KILL);
        let _ = owner.wait();
        let deadline = std::time::Instant::now() + AWAIT;
        // Generations go on running until the keeper stops them; only the
        // end of the lifeline, or the relay's own end, decides.
        let fate = loop {
            match relay.fate_by(deadline).await {
                RelayFate::Running => {}
                fate => break fate,
            }
        };
        let keeper_exited = loop {
            if keeper.has_exited() {
                break true;
            }
            if std::time::Instant::now() >= deadline {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        // A census that does not depend on the lifeline: no process may name
        // the case directory on its command line either. The keeper has
        // exited by now, and an exited process is not counted.
        let survivors = super::fixtures::case_processes(&s.dir())
            .unwrap_or_else(|e| panic!("attempt {attempt}: survivors unknown: {e}"));
        let stopped = fate == RelayFate::Stopped && keeper_exited && survivors.is_empty();
        if !stopped {
            super::fixtures::end_relay_leftovers(&s.dir(), &mut relay.lifeline).await;
        }
        assert!(
            stopped,
            "attempt {attempt}: the relay or the keeper outlived the keeper's cleanup: relay \
             {fate:?}, keeper {keeper_pid} exited {keeper_exited}, survivors {survivors:?}"
        );
    }
}

/// A descendant that leaves the leader's process group, says so in
/// `gc-ready` only after `setpgid` succeeded, and then waits, holding none of
/// the run's pipes, until the owner has recorded a refused signal (the test
/// seam creates `refused` then; it refuses every signal to a single process).
/// It ends only then, by itself. The leader goes on only once `gc-ready` is
/// written, so no group signal can reach the descendant before it left the
/// group, and nothing but the refusal decides when it ends.
const WAITS_FOR_A_REFUSAL: &str = "perl -e 'use POSIX; setpgid(0, 0); getpgrp() == $$ or die; \
     open F, q(>), q(gc-ready); print F $$; close F; \
     until (-e q(refused)) { select(undef, undef, undef, 0.02) }' \
     </dev/null >/dev/null 2>&1 & record gc $!; \
     until [ -s gc-ready ]; do sleep 0.01; done;";

/// The spec with every signal to a single process refused, and the refusal
/// recorded in the scratch directory's `refused`. A kept spec's keeper holds
/// every freeze of its tree until then, so the keeper cannot end the
/// descendant before the owner has had a signal refused, whichever process
/// acts first.
fn refusing(s: &Scratch, spec: ProcessSpec) -> ProcessSpec {
    let mut spec = spec;
    spec.refuse_signals = true;
    spec.refusal_file = Some(s.path("refused"));
    if let Some(keeper) = spec.keeper.take() {
        spec.keeper = Some(keeper.env(crate::process::keeper::FREEZE_GATE_ENV, s.path("refused")));
    }
    spec
}

/// Assert that the keeper's freeze gate, if a freeze waited at it, was opened
/// by the owner's refusal and did not expire: an expired gate runs no freeze
/// and leaves its marker, so a run whose ordering was not the one the gate
/// enforces fails here rather than passing on a stale refusal.
fn assert_gate_opened_by_the_refusal(s: &Scratch) {
    assert!(s.path("refused").exists(), "the owner recorded no refusal");
    let marker = crate::process::keeper::gate_expired_marker(&s.path("refused"));
    assert!(
        !marker.exists(),
        "the keeper's freeze gate expired before the owner's refusal"
    );
}

/// The keeper's last report, its answer to the reap.
fn reap_report(report: &crate::process::ProcessReport) -> String {
    report
        .diagnostics
        .keeper
        .as_ref()
        .and_then(|k| k.tree_reports.last().cloned())
        .unwrap_or_default()
}

/// Assert the verdict is Unconfirmed for an unverified signal.
fn assert_refusal_stands(report: &crate::process::ProcessReport) {
    match &report.cleanup {
        Cleanup::Unconfirmed { reason, .. } => {
            assert_eq!(
                reason,
                crate::process::UNVERIFIED_SIGNAL_REASON,
                "{report:#?}"
            );
        }
        other => panic!("a refused signal must not confirm cleanup: {other:?}: {report:#?}"),
    }
}

/// A process the cleanup had to signal and could not verify is not signalled,
/// and the verdict is not Confirmed even when it then ends by itself and every
/// sweep afterwards finds nothing. The run has no keeper, so nothing but the
/// owner signals the descendant and no final census can supersede the
/// refusal. The stop comes once the descendant has left the group and the
/// owner tracks it, and the descendant ends only after the refusal.
#[tokio::test]
async fn a_signal_refused_for_lack_of_verification_makes_the_verdict_unconfirmed() {
    require_perl!();
    let s = Scratch::new();
    let script = format!("{WAITS_FOR_A_REFUSAL} record leader $$; sleep 60");
    let mut spec = refusing(&s, sh(&s, &script, LONG));
    spec.keeper = None;
    let handle = ProcessHandle::start(spec);
    let mut reaper = Reaper::default();
    let gc = reaper.watch(&s, "gc").await;
    let _ = s.pid("leader").await;
    super::fixtures::wait_tracked(&handle, gc).await;
    handle.interrupt();
    let report = handle.wait().await;
    assert_gone("the descendant", &[gc]).await;
    assert_eq!(report.diagnostics.keeper, None, "{report:#?}");
    assert!(s.path("refused").exists(), "{report:#?}");
    assert!(
        report.diagnostics.inspection.unverified_signals > 0,
        "{report:#?}"
    );
    assert_refusal_stands(&report);
}

/// The narrower Rule A of `process/mod.rs`. A kept run whose owner had a
/// signal refused: on Linux the keeper is a child subreaper, so its final
/// census at the reap, which proves its tree empty once the descendant has
/// ended, supersedes the refusal and the verdict is Confirmed. On macOS there
/// is no subreaper and no such proof, so the refusal stands. The leader exits
/// once the descendant has left its group; the owner's sweeps after the exit
/// meet the descendant (the keeper is asked for its tree only once a sweep
/// finds nothing), and the descendant ends only after one was refused.
#[tokio::test]
async fn a_refused_signal_is_superseded_only_by_a_final_census_that_proves_the_tree_empty() {
    require_perl!();
    let s = Scratch::new();
    let script = format!("{WAITS_FOR_A_REFUSAL} exit 0");
    let spec = refusing(&s, kept(sh(&s, &script, LONG)));
    let (report, pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
    assert_gone("the descendant", &pids).await;
    assert_gate_opened_by_the_refusal(&s);
    assert_eq!(report.ending, Ending::Exited { code: 0 }, "{report:#?}");
    assert!(
        report.diagnostics.inspection.unverified_signals > 0,
        "the owner never had a signal refused: {report:#?}"
    );
    let keeper = report.diagnostics.keeper.as_ref().expect("a kept run");
    if cfg!(target_os = "linux") {
        assert!(keeper.subreaper, "{report:#?}");
        assert_eq!(
            reap_report(&report),
            "reaped: live=0 proven=true",
            "{report:#?}"
        );
        assert_confirmed(&report);
    } else {
        assert_eq!(
            reap_report(&report),
            "reaped: live=0 proven=false",
            "{report:#?}"
        );
        assert_refusal_stands(&report);
    }
}

/// The same kept run, with a keeper whose first read of `/proc` after the
/// owner says `reap` fails (a test seam, Linux). No answer of the keeper can
/// report it, so only the proof shows it: the final census proves nothing,
/// and the owner's refusal stands on every platform. A verdict that waived
/// Linux owner refusals without the proof would be Confirmed here.
#[tokio::test]
async fn an_owner_refusal_stands_when_the_final_census_proves_nothing() {
    require_perl!();
    let s = Scratch::new();
    let script = format!("{WAITS_FOR_A_REFUSAL} exit 0");
    let mut spec = sh(&s, &script, LONG);
    spec.keeper = Some(test_keeper().env(crate::process::keeper::INJECT_READ_FAILURES_ENV, "1"));
    let spec = refusing(&s, spec);
    let (report, pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
    assert_gone("the descendant", &pids).await;
    assert_gate_opened_by_the_refusal(&s);
    assert_eq!(report.ending, Ending::Exited { code: 0 }, "{report:#?}");
    assert!(
        report.diagnostics.inspection.unverified_signals > 0,
        "the owner never had a signal refused: {report:#?}"
    );
    assert_eq!(
        reap_report(&report),
        "reaped: live=0 proven=false",
        "{report:#?}"
    );
    assert_refusal_stands(&report);
}

/// The verdict's reason, or a panic naming the verdict.
fn unconfirmed_reason(report: &crate::process::ProcessReport) -> &str {
    super::fixtures::assert_documented(&report.cleanup);
    match &report.cleanup {
        Cleanup::Unconfirmed { reason, .. } => reason,
        other => panic!("expected an Unconfirmed verdict, got {other:?}: {report:#?}"),
    }
}

/// A refused hold the keeper reports in answer to a census the owner had
/// already given up on (the request timed out) still decides the verdict:
/// the owner's reader records it as it reads the answer, and the keeper,
/// which recorded it before it answered, carries it in its answer to the
/// reap. The program exits at once and every later answer is clean, so a
/// run that lost the late answer would end Confirmed. Linux: only a
/// subreaper keeper is asked for its census.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_keeper_refusal_answered_after_its_request_timed_out_makes_the_verdict_unconfirmed() {
    let s = Scratch::new();
    let mut spec = sh(&s, "exit 0", LONG);
    spec.keeper = Some(test_keeper().env(crate::process::keeper::LATE_REFUSAL_ENV, "1"));
    let report = ProcessHandle::start(spec).wait().await;
    assert_eq!(report.ending, Ending::Exited { code: 0 }, "{report:#?}");
    let keeper = report.diagnostics.keeper.as_ref().expect("a kept run");
    assert!(keeper.subreaper, "{report:#?}");
    assert!(
        keeper.tree_reports.iter().any(|r| r == "census: no answer"),
        "the census did not time out: {report:#?}"
    );
    assert!(
        keeper
            .tree_reports
            .iter()
            .any(|r| r.starts_with("census: late live=1 ") && r.contains("refused=1")),
        "the late refusal was not read: {report:#?}"
    );
    assert_eq!(
        reap_report(&report),
        "reaped: live=0 proven=true unproven=true",
        "{report:#?}"
    );
    assert_eq!(
        unconfirmed_reason(&report),
        crate::process::KEEPER_UNPROVEN_REASON,
        "{report:#?}"
    );
}

/// An interrupted kept run of a program that ignores TERM, so the owner's
/// hard stop asks the keeper for a freeze, with a keeper whose first freeze
/// is answered by the test seam with the details `answer` names (see
/// `FIRST_FREEZE_ENV`), without a walk. The owner's group KILL ends the
/// program, and every later answer is a real walk. Linux: only a subreaper
/// keeper is asked for a freeze.
#[cfg(target_os = "linux")]
async fn interrupted_with_first_freeze(answer: &str) -> crate::process::ProcessReport {
    let s = Scratch::new();
    let mut spec = sh(&s, "trap '' TERM; record leader $$; sleep 60", LONG);
    spec.stop_grace = Duration::from_millis(100);
    spec.keeper = Some(test_keeper().env(crate::process::keeper::FIRST_FREEZE_ENV, answer));
    let handle = ProcessHandle::start(spec);
    let _ = s.pid("leader").await;
    handle.interrupt();
    let report = handle.wait().await;
    assert_eq!(report.ending, Ending::Interrupted, "{report:#?}");
    let keeper = report.diagnostics.keeper.as_ref().expect("a kept run");
    assert!(keeper.subreaper, "{report:#?}");
    report
}

/// Whether the keeper's answers include the seam's freeze, `late` or not.
#[cfg(target_os = "linux")]
fn has_freeze_answer(report: &crate::process::ProcessReport, late: bool, detail: &str) -> bool {
    let prefix = if late {
        "freeze: late live="
    } else {
        "freeze: live="
    };
    report.diagnostics.keeper.as_ref().is_some_and(|k| {
        k.tree_reports
            .iter()
            .any(|r| r.starts_with(prefix) && r.ends_with(detail))
    })
}

/// The narrower Rule A on Linux: a freeze that only did not complete (no
/// refused hold, no failed read) says the keeper had not stopped everything
/// yet, not that a process escaped, and the final census that proves the
/// tree empty supersedes it. A verdict that kept every incomplete freeze
/// sticky would be Unconfirmed here.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn an_incomplete_freeze_is_superseded_by_a_final_census_that_proves_the_tree_empty() {
    let report = interrupted_with_first_freeze("incomplete").await;
    assert!(
        has_freeze_answer(
            &report,
            false,
            "passes=64 complete=false refused=0 read_failures=0"
        ),
        "the seam's freeze was not answered: {report:#?}"
    );
    assert_eq!(
        reap_report(&report),
        "reaped: live=0 proven=true incomplete=true",
        "{report:#?}"
    );
    assert_confirmed(&report);
}

/// The same with the incomplete freeze answered after the owner's request
/// timed out: the owner's reader records it as incomplete, and the proof
/// still supersedes it. This is the race of #208 made certain.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_late_incomplete_freeze_is_superseded_by_a_final_census_that_proves_the_tree_empty() {
    let report = interrupted_with_first_freeze("late-incomplete").await;
    let keeper = report.diagnostics.keeper.as_ref().expect("a kept run");
    assert!(
        keeper.tree_reports.iter().any(|r| r == "freeze: no answer"),
        "the freeze did not time out: {report:#?}"
    );
    assert!(
        has_freeze_answer(
            &report,
            true,
            "passes=64 complete=false refused=0 read_failures=0"
        ),
        "the late freeze was not read: {report:#?}"
    );
    assert_eq!(
        reap_report(&report),
        "reaped: live=0 proven=true incomplete=true",
        "{report:#?}"
    );
    assert_confirmed(&report);
}

/// Assert that a freeze answer carrying a hole stayed sticky through a
/// proven final census.
#[cfg(target_os = "linux")]
fn assert_freeze_hole_stands(report: &crate::process::ProcessReport, detail: &str) {
    assert!(
        has_freeze_answer(report, false, detail),
        "the seam's freeze was not answered: {report:#?}"
    );
    assert_eq!(
        reap_report(report),
        "reaped: live=0 proven=true unproven=true",
        "{report:#?}"
    );
    assert_eq!(
        unconfirmed_reason(report),
        crate::process::KEEPER_UNPROVEN_REASON,
        "{report:#?}"
    );
}

/// An incomplete freeze that refused a hold left a process unsignalled; the
/// proof does not supersede it, so the verdict stays Unconfirmed.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_freeze_that_refused_a_hold_is_not_superseded_by_a_proven_final_census() {
    let report = interrupted_with_first_freeze("refused").await;
    assert_freeze_hole_stands(&report, "passes=2 complete=false refused=1 read_failures=0");
}

/// The same for an incomplete freeze with a failed read of `/proc`.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_freeze_with_a_failed_read_is_not_superseded_by_a_proven_final_census() {
    let report = interrupted_with_first_freeze("read-failure").await;
    assert_freeze_hole_stands(&report, "passes=2 complete=false refused=0 read_failures=1");
}

/// The same for a freeze whose task failed: no walk ran, so it is not merely
/// incomplete.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_failed_freeze_task_is_not_superseded_by_a_proven_final_census() {
    let report = interrupted_with_first_freeze("task-failed").await;
    assert_freeze_hole_stands(
        &report,
        "passes=0 complete=false refused=0 read_failures=0 task=failed",
    );
}

/// A kept run that starts nothing but the program, with a keeper whose first
/// read of `/proc` after the owner says `reap` fails (a test seam, Linux).
/// Nothing is alive and the owner refused nothing, but the final census
/// proves nothing, so on Linux the verdict is Unconfirmed for that reason
/// alone. macOS has no subreaper and no proof, and keeps its rules.
#[tokio::test]
async fn a_reap_of_nothing_alive_without_the_final_proof_is_unconfirmed_on_linux() {
    let s = Scratch::new();
    let mut spec = sh(&s, "exit 0", LONG);
    spec.keeper = Some(test_keeper().env(crate::process::keeper::INJECT_READ_FAILURES_ENV, "1"));
    let report = ProcessHandle::start(spec).wait().await;
    assert_eq!(report.ending, Ending::Exited { code: 0 }, "{report:#?}");
    assert_eq!(
        report.diagnostics.inspection.unverified_signals, 0,
        "{report:#?}"
    );
    assert_eq!(
        reap_report(&report),
        "reaped: live=0 proven=false",
        "{report:#?}"
    );
    if cfg!(target_os = "linux") {
        assert_eq!(
            unconfirmed_reason(&report),
            crate::process::FINAL_CENSUS_UNPROVEN_REASON,
            "{report:#?}"
        );
    } else {
        assert_confirmed(&report);
    }
}
