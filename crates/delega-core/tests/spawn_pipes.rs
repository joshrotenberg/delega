//! A child the process layer starts never inherits a descriptor that another
//! holder of the spawn lock has open without close-on-exec (#104).
//!
//! On macOS std creates a child's pipes with `pipe()` and marks them
//! close-on-exec afterwards. A child forked by another thread in between
//! inherits those pipe ends and keeps them through `exec`: a provider that
//! holds another run's standard input keeps that run from ever reading end of
//! input. The process layer starts every child under one lock, which a program
//! that spawns on its own threads can take with `process::spawn_guard`.
//!
//! Here a plain thread plays the other spawn: it takes the guard, opens a pipe
//! without close-on-exec, keeps it open for a while and closes it before it
//! lets go. It holds such a pipe about two thirds of the time. A process-layer
//! spawn that skipped the lock would inherit it with about that chance, and
//! the child, which lists its own descriptors, would show it. With the lock no
//! process-layer child can be forked while the pipe is open.
//!
//! This is a test binary of its own so that no unguarded spawn of another test
//! runs beside it and inherits the pipe.

#![cfg(unix)]

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use delega_core::process::{EnvPolicy, ProcessReport, ProcessSpec, run, spawn_guard};

/// Rounds of process-layer children started together.
const ROUNDS: usize = 6;
const AT_ONCE: usize = 8;
/// How long the other thread keeps its pipe open under the guard, and how long
/// it waits before it takes the guard again, so the runs are not starved.
const HELD: Duration = Duration::from_millis(2);
const RELEASED: Duration = Duration::from_millis(1);
/// A bound on the whole check, far above the second or so it takes. The check
/// runs on a thread and runtime of its own and the test thread waits for it
/// with this deadline, so a spawn stuck on the lock (which blocks a runtime
/// worker, where no async timeout can reach it) fails the test instead of
/// hanging it. The stuck thread is left behind; this binary holds no other
/// test.
const BOUND: Duration = Duration::from_secs(120);

/// A process-layer child that lists its own open descriptors. Its standard
/// input is a pipe, as a provider's is.
fn lister(dir: &std::path::Path) -> ProcessSpec {
    ProcessSpec::new("/bin/sh", Duration::from_secs(60))
        .args(["-c", "cat >/dev/null; ls /dev/fd"])
        .cwd(dir)
        .env(EnvPolicy::inherit_only(["PATH"]))
        .stdin("prompt\n")
}

fn descriptors(report: &ProcessReport) -> BTreeSet<String> {
    report
        .stdout
        .text()
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// The check itself: `Err` with what went wrong, or `Ok`.
async fn check() -> Result<(), String> {
    let dir = tempfile::tempdir().map_err(|e| format!("scratch directory: {e}"))?;
    // The child alone: its standard streams plus the descriptor `ls` opens to
    // read the directory.
    let alone = run(lister(dir.path())).await;
    let expected = descriptors(&alone);
    if !expected.contains("0") {
        return Err(format!("the child alone listed nothing usable: {alone:#?}"));
    }

    let stop = Arc::new(AtomicBool::new(false));
    let opened = Arc::new(AtomicUsize::new(0));
    let other = std::thread::spawn({
        let stop = stop.clone();
        let opened = opened.clone();
        move || {
            while !stop.load(Ordering::Relaxed) {
                {
                    let _guard = spawn_guard();
                    // Plain pipe(2): no close-on-exec, as std's pipes are on
                    // macOS before it marks them.
                    let pipe = rustix::pipe::pipe().expect("open a pipe");
                    opened.fetch_add(1, Ordering::Relaxed);
                    std::thread::sleep(HELD);
                    drop(pipe);
                }
                std::thread::sleep(RELEASED);
            }
        }
    });

    let before = opened.load(Ordering::Relaxed);
    let mut wrong = Vec::new();
    for round in 0..ROUNDS {
        let barrier = Arc::new(tokio::sync::Barrier::new(AT_ONCE));
        let children: Vec<_> = (0..AT_ONCE)
            .map(|_| {
                let spec = lister(dir.path());
                let barrier = barrier.clone();
                tokio::spawn(async move {
                    barrier.wait().await;
                    run(spec).await
                })
            })
            .collect();
        for child in children {
            let report = child
                .await
                .map_err(|e| format!("the task that ran a child: {e}"))?;
            let listed = descriptors(&report);
            if listed != expected {
                wrong.push((round, listed));
            }
        }
    }
    let during = opened.load(Ordering::Relaxed) - before;
    stop.store(true, Ordering::Relaxed);
    other
        .join()
        .map_err(|_| "the thread that opened pipes panicked".to_string())?;

    // A sanity check that the pipe holder ran while the children started, not
    // proof that every spawn overlapped a held pipe. With the lock removed
    // from the process layer, 30 to 34 of the 48 children inherited the pipe
    // in the runs this was written with.
    if during < ROUNDS {
        return Err(format!(
            "the other thread opened a pipe only {during} times during the rounds"
        ));
    }
    if !wrong.is_empty() {
        return Err(format!(
            "{} of {} children listed descriptors beyond {expected:?}: {wrong:?}",
            wrong.len(),
            ROUNDS * AT_ONCE
        ));
    }
    Ok(())
}

#[test]
fn a_child_never_inherits_a_pipe_opened_under_the_spawn_guard() {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(8)
            .enable_all()
            .build()
            .expect("build a runtime");
        let _ = tx.send(runtime.block_on(check()));
    });
    match rx.recv_timeout(BOUND) {
        Ok(Ok(())) => {}
        Ok(Err(problem)) => panic!("{problem}"),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("the check did not finish within {BOUND:?}: a spawn may be stuck on the lock")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("the check's thread panicked; its message is above")
        }
    }
}
