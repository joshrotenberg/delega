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

use std::sync::atomic::Ordering;
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
