//! How long a stop waits for the process listing that must precede its first
//! signal. A requested stop waits at most its listing budget, a second
//! interrupt ends the wait, and a drop never waits. A listing that did not
//! complete makes the verdict `Unconfirmed`.
//!
//! None of these tests measures a duration. Each holds the listing back with a
//! gate the test controls and asserts an order: the child was gone while the
//! listing was still held, and only then is the gate opened. A stop that waited
//! for the listing would leave the child running until the test's own deadline
//! (seconds, far beyond what load adds), which fails the test; a stop that does
//! not wait ends the child at once however slow the machine is.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use super::fixtures::{Held, Reaper, Scratch, await_event, is_running_async, sh};
use crate::contract::result::Cleanup;
use crate::process::{Ending, ProcessHandle, ProcessReport};

const LONG: Duration = Duration::from_secs(60);
/// How long the child may stay alive after a stop that must not wait for the
/// listing. The listing is held for the whole of it, so only a stop that waits
/// for the listing, or for a budget longer than this, runs into it.
const DEADLINE: Duration = Duration::from_secs(30);
/// A short listing budget for the tests that let it run out. Its value is not
/// asserted: the listing is held until the child is gone, whatever the budget.
const SHORT_BUDGET: Duration = Duration::from_millis(300);
/// How long a test watches that a child is still alive while a stop is held
/// back. This can only miss a defect on a slow machine, never fail a correct
/// run.
const HELD_WINDOW: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug)]
enum How {
    Interrupt,
    SecondInterrupt,
    Drop,
    TimeLimit,
}

/// Wait for the child to be gone, with the listing still held, and fail with
/// the way it was stopped if it is not gone by [`DEADLINE`].
async fn assert_gone_while_held(how: How, leader: i32) {
    let began = Instant::now();
    while is_running_async(leader).await {
        assert!(
            began.elapsed() < DEADLINE,
            "{how:?}: the child was still running {DEADLINE:?} after the stop; the stop was held by the listing"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Run a child that sleeps until stopped, stop it as `how` says while every
/// process listing is held, check that it is gone before the listing is
/// released, release the listing, and return the report unless the handle was
/// dropped.
async fn stop_while_the_listing_is_held(how: How) -> Option<ProcessReport> {
    let s = Scratch::new();
    let limit = match how {
        How::TimeLimit => Duration::from_secs(1),
        _ => LONG,
    };
    let held = Held::new(true);
    let mut spec = sh(&s, "echo $$ > leader; sleep 60", limit).with_lister(held.lister());
    // No periodic scan after the first, so the stop's listing is the one held.
    spec.capture_interval = Some(Duration::from_secs(3600));
    // A budget that runs out for the stops that are meant to wait for it, and
    // one that cannot run out for the stops that must end the wait themselves.
    spec.timings.stop_listing = match how {
        How::Interrupt | How::TimeLimit => SHORT_BUDGET,
        How::SecondInterrupt | How::Drop => Duration::from_secs(120),
    };
    let handle = ProcessHandle::start(spec);
    await_event("the child starting", || handle.pid().is_some()).await;
    let leader = i32::try_from(handle.pid().expect("it started")).expect("a pid");
    let _reaper = Reaper(vec![leader]);
    let handle = match how {
        How::Interrupt => {
            handle.interrupt();
            Some(handle)
        }
        How::SecondInterrupt => {
            handle.interrupt();
            // The first interrupt waits for the listing, which is held: the
            // child must still be alive. Then the second ends the wait.
            tokio::time::sleep(HELD_WINDOW).await;
            assert!(
                is_running_async(leader).await,
                "the child was stopped by the first interrupt while its listing was held"
            );
            handle.interrupt();
            Some(handle)
        }
        How::Drop => {
            drop(handle);
            None
        }
        How::TimeLimit => Some(handle),
    };
    assert_gone_while_held(how, leader).await;
    // The listing has been held for the whole stop.
    assert!(held.waiting.load(Ordering::SeqCst) >= 1, "{how:?}");
    held.release();
    match handle {
        Some(handle) => Some(handle.wait().await),
        None => None,
    }
}

fn assert_missed_listing(report: &ProcessReport) {
    match &report.cleanup {
        Cleanup::Unconfirmed { reason, .. } => assert!(
            reason.contains("before the first signal did not complete"),
            "{reason}"
        ),
        other => panic!("a missed stop listing must not confirm cleanup: {other:?}"),
    }
}

#[tokio::test]
async fn an_interrupt_signals_after_the_short_budget_when_the_listing_is_late_and_is_unconfirmed() {
    // Proves: the stop does not wait for a listing that never arrives, only for
    // its budget. Not speed dependent: the child must be gone while the listing
    // is held, however long the budget takes to run out.
    let report = stop_while_the_listing_is_held(How::Interrupt)
        .await
        .expect("a report");
    assert_eq!(report.ending, Ending::Interrupted, "{report:#?}");
    assert_missed_listing(&report);
}

#[tokio::test]
async fn a_time_limit_signals_after_the_short_budget_when_the_listing_is_late_and_is_unconfirmed() {
    let report = stop_while_the_listing_is_held(How::TimeLimit)
        .await
        .expect("a report");
    assert!(
        matches!(report.ending, Ending::TimedOut { .. }),
        "{report:#?}"
    );
    assert_missed_listing(&report);
}

#[tokio::test]
async fn a_second_interrupt_ends_the_wait_for_the_listing_at_once() {
    // The budget is two minutes, so only the second interrupt can end the wait
    // before the test's deadline.
    let report = stop_while_the_listing_is_held(How::SecondInterrupt)
        .await
        .expect("a report");
    assert_eq!(report.ending, Ending::Interrupted, "{report:#?}");
    assert_missed_listing(&report);
}

#[tokio::test]
async fn dropping_the_handle_does_not_wait_for_a_listing() {
    // The budget is two minutes and nothing but the drop asks for a stop.
    assert!(stop_while_the_listing_is_held(How::Drop).await.is_none());
}

#[tokio::test]
async fn a_normal_interrupt_is_one_listing_and_confirmed() {
    // Proves: a requested stop takes one listing before it signals, waits for
    // it, and with that listing complete the cleanup is confirmed. The listing
    // is held until the test has watched that the child is not signalled before
    // it arrives; no duration is asserted.
    let s = Scratch::new();
    let held = Held::new(false);
    let mut spec = sh(&s, "echo $$ > leader; sleep 60", LONG).with_lister(held.lister());
    spec.capture_interval = Some(Duration::from_secs(3600));
    let handle = ProcessHandle::start(spec);
    let leader = s.pid("leader").await;
    let _reaper = Reaper(vec![leader]);
    held.armed.store(true, Ordering::SeqCst);
    handle.interrupt();
    await_event("the stop asking for its listing", || {
        held.waiting.load(Ordering::SeqCst) >= 1
    })
    .await;
    tokio::time::sleep(HELD_WINDOW).await;
    assert!(
        is_running_async(leader).await,
        "the child was signalled before its stop listing arrived"
    );
    held.release();
    let report = handle.wait().await;
    assert_eq!(report.ending, Ending::Interrupted, "{report:#?}");
    assert_eq!(report.cleanup, Cleanup::Confirmed, "{report:#?}");
}

/// A descendant in a session of its own that holds the output pipes and ends on
/// TERM, started by a leader that also ends on TERM.
const SESSION_DESCENDANT: &str = "echo $$ > leader; \
     perl -e 'use POSIX; POSIX::setsid(); exec q(sleep), q(60)' & echo $! > gc; wait";

fn term_to_descendants(report: &ProcessReport) -> usize {
    report
        .diagnostics
        .signals
        .iter()
        .filter(|record| {
            record.signal == "SIGTERM"
                && matches!(
                    record.target,
                    crate::process::SignalTarget::Descendants { .. }
                )
        })
        .count()
}

#[tokio::test]
async fn a_term_sweep_that_ran_out_of_budget_is_repeated_until_a_descendant_gets_term() {
    // Proves: when the listing is not done within the sweep budget at the
    // stop, the descendant that left the group still receives TERM from a later
    // sweep and the run ends long before the grace. The listing is held until
    // the leader is gone, so the budget has certainly run out; the descendant
    // holds the pipes, so before the repeat only the KILL after the grace (a
    // minute) could end the run. No duration is asserted beyond the bound that
    // only the grace reaches.
    if !super::fixtures::perl_available() {
        eprintln!("skipped: perl is not available");
        return;
    }
    let s = Scratch::new();
    let held = Held::new(false);
    let mut spec = sh(&s, SESSION_DESCENDANT, LONG).with_lister(held.lister());
    spec.capture_interval = Some(Duration::from_millis(100));
    spec.stop_grace = LONG;
    spec.drain_bound = LONG;
    spec.cleanup_deadline = Duration::from_secs(180);
    spec.timings.stop_listing = SHORT_BUDGET;
    spec.timings.descendant_signal = Duration::from_millis(100);
    let handle = ProcessHandle::start(spec);
    let leader = s.pid("leader").await;
    let gc = s.pid("gc").await;
    let _reaper = Reaper(vec![leader, gc]);
    super::fixtures::wait_own_group(gc).await;
    super::fixtures::wait_tracked(&handle, gc).await;
    held.armed.store(true, Ordering::SeqCst);
    handle.interrupt();
    await_event("the stop asking for its listing", || {
        held.waiting.load(Ordering::SeqCst) >= 1
    })
    .await;
    // The group TERM ended the leader while the listing is still held; the
    // descendant has not been signalled, since no listing has been seen. The
    // window lets the sweep of the stop itself run out of its budget before
    // the listing is released (it can only miss a defect on a slow machine,
    // never fail a correct run).
    assert_gone_while_held(How::Interrupt, leader).await;
    tokio::time::sleep(HELD_WINDOW).await;
    assert!(
        is_running_async(gc).await,
        "the descendant was signalled before any listing arrived"
    );
    held.release();
    let began = Instant::now();
    let report = tokio::time::timeout(DEADLINE, handle.wait())
        .await
        .expect("the run waited for the grace instead of repeating the TERM sweep");
    assert_eq!(report.ending, Ending::Interrupted, "{report:#?}");
    assert!(began.elapsed() < DEADLINE);
    assert_eq!(term_to_descendants(&report), 1, "{report:#?}");
    assert!(
        !report
            .diagnostics
            .signals
            .iter()
            .any(|record| record.signal == "SIGKILL"),
        "{report:#?}"
    );
    assert_missed_listing(&report);
    super::fixtures::assert_gone("descendant", &[gc]).await;
}

#[tokio::test]
async fn a_stop_whose_listing_never_completes_still_ends_at_the_grace_with_kill() {
    // Proves: repeating the TERM sweep adds no wait. With every listing held
    // for the whole run, the group is killed at the grace, the run returns at
    // the cleanup deadline with an unconfirmed verdict, and nothing waits for
    // the listing.
    let s = Scratch::new();
    let held = Held::new(true);
    let mut spec = sh(
        &s,
        "trap '' TERM; echo $$ > leader; while :; do sleep 1; done",
        LONG,
    )
    .with_lister(held.lister());
    spec.stop_grace = Duration::from_secs(1);
    spec.drain_bound = Duration::from_secs(1);
    spec.cleanup_deadline = Duration::from_secs(3);
    spec.timings.stop_listing = SHORT_BUDGET;
    spec.timings.descendant_signal = Duration::from_millis(250);
    let handle = ProcessHandle::start(spec);
    let leader = s.pid("leader").await;
    let _reaper = Reaper(vec![leader]);
    handle.interrupt();
    let report = tokio::time::timeout(DEADLINE, handle.wait())
        .await
        .expect("the stop did not end by its grace and cleanup deadline");
    held.release();
    assert_eq!(report.ending, Ending::Interrupted, "{report:#?}");
    assert!(
        report
            .diagnostics
            .signals
            .iter()
            .any(|record| record.signal == "SIGKILL"
                && matches!(record.target, crate::process::SignalTarget::Group)),
        "{report:#?}"
    );
    assert!(
        matches!(report.cleanup, Cleanup::Unconfirmed { .. }),
        "{report:#?}"
    );
    super::fixtures::assert_gone("leader", &[leader]).await;
}

/// The initial capture waits for the descendant to enter its own session.
/// Count only listings from the owner, starting with its bounded TERM
/// sweep. The capture task also takes the listing requested by the stop.
fn slow_stop_lister(s: &Scratch, calls: Arc<AtomicU32>) -> crate::process::tree::Lister {
    let ready = s.path("ready");
    let capture_task = OnceLock::new();
    Arc::new(move || {
        let ready = ready.clone();
        let task = tokio::task::id();
        // The test waits for the initial capture to track the descendant
        // before requesting the stop, so the first listing is from capture.
        let initial_capture = capture_task.set(task).is_ok();
        if capture_task.get() != Some(&task) {
            calls.fetch_add(1, Ordering::SeqCst);
        }
        Box::pin(async move {
            if initial_capture {
                await_event("the descendant entering its own session", || ready.exists()).await;
            } else {
                // Longer than the stop listing and bounded sweep budgets.
                tokio::time::sleep(Duration::from_millis(400)).await;
            }
            crate::process::tree::list_processes().await
        })
    })
}

/// Stop a leader whose tracked descendant began in its own session.
async fn stop_with_slow_listings(term_handler: &str, grace: Duration) -> (ProcessReport, u32) {
    let s = Scratch::new();
    let script = format!(
        "echo $$ > leader; perl -e 'use POSIX; POSIX::setsid(); \
         {term_handler} open my $ready, q(>), q(ready) or die $!; close $ready; \
         exec q(sleep), q(60)' & echo $! > gc; wait"
    );
    let calls = Arc::new(AtomicU32::new(0));
    let mut spec = sh(&s, &script, LONG).with_lister(slow_stop_lister(&s, calls.clone()));
    spec.capture_interval = Some(Duration::from_secs(3600));
    spec.stop_grace = grace;
    spec.drain_bound = LONG;
    spec.cleanup_deadline = Duration::from_secs(180);
    spec.timings.stop_listing = SHORT_BUDGET;
    spec.timings.descendant_signal = Duration::from_millis(250);
    let handle = ProcessHandle::start(spec);
    let leader = s.pid("leader").await;
    let gc = s.pid("gc").await;
    let _reaper = Reaper(vec![leader, gc]);
    super::fixtures::wait_tracked(&handle, gc).await;
    handle.interrupt();
    let report = tokio::time::timeout(DEADLINE, handle.wait())
        .await
        .expect("the stop waited for the grace or for the descendant to end by itself");
    super::fixtures::assert_gone("descendant", &[gc]).await;
    let after_first_sweep = calls
        .load(Ordering::SeqCst)
        .checked_sub(1)
        .expect("the first TERM sweep started");
    (report, after_first_sweep)
}

#[tokio::test]
async fn a_listing_always_slower_than_the_sweep_budget_still_gets_the_descendant_one_term() {
    crate::test_spawn::rooms::hold(crate::test_spawn::rooms::Room::Alone);
    // Proves: every listing after the first takes longer than the budget of a
    // bounded sweep, yet the repeat (one unbounded sweep at a time) completes
    // and the descendant in its own session, which holds the pipes, gets one
    // TERM and the run ends long before the 60 s grace, without piling up
    // listings.
    if !super::fixtures::perl_available() {
        eprintln!("skipped: perl is not available");
        return;
    }
    let (report, started) = stop_with_slow_listings("", LONG).await;
    assert_eq!(report.ending, Ending::Interrupted, "{report:#?}");
    assert_eq!(term_to_descendants(&report), 1, "{report:#?}");
    assert!(
        !report
            .diagnostics
            .signals
            .iter()
            .any(|record| record.signal == "SIGKILL"),
        "{report:#?}"
    );
    // One TERM retry and two clean sweeps, with room for one sweep that
    // still sees the descendant exiting.
    assert!(
        started <= 4,
        "{started} listings after the first TERM sweep"
    );
}

#[tokio::test]
async fn a_listing_always_slower_than_the_sweep_budget_still_kills_a_term_ignoring_descendant() {
    crate::test_spawn::rooms::hold(crate::test_spawn::rooms::Room::Alone);
    // Proves: the KILL sweep after the grace is repeated the same way, so a
    // descendant that ignores TERM and never exits is killed at the grace and
    // does not hold the run until the drain bound (a minute) closes its pipes.
    if !super::fixtures::perl_available() {
        eprintln!("skipped: perl is not available");
        return;
    }
    let (report, started) =
        stop_with_slow_listings("$SIG{TERM} = q(IGNORE);", Duration::from_secs(1)).await;
    assert_eq!(report.ending, Ending::Interrupted, "{report:#?}");
    assert!(
        report
            .diagnostics
            .signals
            .iter()
            .any(|record| record.signal == "SIGKILL"
                && matches!(
                    record.target,
                    crate::process::SignalTarget::Descendants { .. }
                )),
        "{report:#?}"
    );
    // The TERM retry, the bounded KILL sweep and its retry, the cleanup's
    // bounded KILL sweep, and two clean sweeps. Allow one more sweep if it
    // still sees the descendant exiting.
    assert!(
        started <= 7,
        "{started} listings after the first TERM sweep"
    );
}
