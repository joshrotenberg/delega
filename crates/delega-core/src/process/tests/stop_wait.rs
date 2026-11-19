//! How long a stop waits for the process listing that must precede its first
//! signal. A requested stop waits at most `STOP_LISTING_BUDGET`, a second
//! interrupt ends the wait, and a drop never waits. A listing that did not
//! complete makes the verdict `Unconfirmed`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use super::fixtures::{Reaper, Scratch, is_running, sh};
use crate::contract::result::Cleanup;
use crate::process::limits::STOP_LISTING_BUDGET;
use crate::process::tree::{Lister, list_processes};
use crate::process::{Ending, ProcessHandle, ProcessReport};

const LONG: Duration = Duration::from_secs(60);
/// Longer than the old wait of 3.1 s would leave unnoticed, and far past the
/// short budget; the listing that is delayed this long is a hung one.
const HUNG: Duration = Duration::from_secs(3);
/// What the stop itself, a process listing of the test and scheduling may add.
const SLACK: Duration = Duration::from_millis(700);

/// A lister whose second call (the listing a stop asks for, since no periodic
/// scan follows the first) waits `delay` before it reads the table.
fn slow_second_listing(delay: Duration) -> Lister {
    let calls = Arc::new(AtomicU32::new(0));
    Arc::new(move || {
        let call = calls.fetch_add(1, Ordering::SeqCst) + 1;
        Box::pin(async move {
            if call == 2 {
                tokio::time::sleep(delay).await;
            }
            list_processes().await
        })
    })
}

#[derive(Clone, Copy, Debug)]
pub(super) enum How {
    Interrupt,
    SecondInterrupt,
    Drop,
    TimeLimit,
}

/// Run a child that sleeps until stopped, stop it as `how` says while its
/// stop-time listing takes `delay`, and return how long the child took to be
/// gone, measured from the stopping request (the limit's moment for a time
/// limit, the second interrupt for that one), and the report unless the handle
/// was dropped.
pub(super) async fn stop_latency(how: How, delay: Duration) -> (Duration, Option<ProcessReport>) {
    let s = Scratch::new();
    let limit = match how {
        How::TimeLimit => Duration::from_secs(1),
        _ => LONG,
    };
    let mut spec =
        sh(&s, "echo $$ > leader; sleep 60", limit).with_lister(slow_second_listing(delay));
    spec.capture_interval = Some(Duration::from_secs(3600));
    let started = Instant::now();
    let handle = ProcessHandle::start(spec);
    let leader = s.pid("leader").await;
    let _reaper = Reaper(vec![leader]);
    // The first scan is done by now.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (requested, handle) = match how {
        How::Interrupt => {
            let at = Instant::now();
            handle.interrupt();
            (at, Some(handle))
        }
        How::SecondInterrupt => {
            handle.interrupt();
            tokio::time::sleep(Duration::from_millis(100)).await;
            let at = Instant::now();
            handle.interrupt();
            (at, Some(handle))
        }
        How::Drop => {
            let at = Instant::now();
            drop(handle);
            (at, None)
        }
        How::TimeLimit => (started + limit, Some(handle)),
    };
    while is_running(leader) {
        assert!(
            requested.elapsed() < Duration::from_secs(10),
            "{how:?}: the child was never stopped"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let latency = requested.elapsed();
    let report = match handle {
        Some(handle) => Some(handle.wait().await),
        None => None,
    };
    (latency, report)
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
    let (latency, report) = stop_latency(How::Interrupt, HUNG).await;
    assert!(
        latency < STOP_LISTING_BUDGET + SLACK,
        "the stop took {latency:?}"
    );
    let report = report.expect("a report");
    assert_eq!(report.ending, Ending::Interrupted, "{report:#?}");
    assert_missed_listing(&report);
}

#[tokio::test]
async fn a_time_limit_signals_after_the_short_budget_when_the_listing_is_late_and_is_unconfirmed() {
    let (latency, report) = stop_latency(How::TimeLimit, HUNG).await;
    assert!(
        latency < STOP_LISTING_BUDGET + SLACK,
        "the stop took {latency:?}"
    );
    let report = report.expect("a report");
    assert!(
        matches!(report.ending, Ending::TimedOut { .. }),
        "{report:#?}"
    );
    assert_missed_listing(&report);
}

#[tokio::test]
async fn a_second_interrupt_ends_the_wait_for_the_listing_at_once() {
    let (latency, report) = stop_latency(How::SecondInterrupt, HUNG).await;
    // Well under the budget the first interrupt alone would have waited.
    assert!(
        latency < STOP_LISTING_BUDGET / 2,
        "the stop took {latency:?} after the second interrupt"
    );
    let report = report.expect("a report");
    assert_eq!(report.ending, Ending::Interrupted, "{report:#?}");
    assert_missed_listing(&report);
}

#[tokio::test]
async fn dropping_the_handle_does_not_wait_for_a_listing() {
    let (latency, _) = stop_latency(How::Drop, HUNG).await;
    assert!(
        latency < STOP_LISTING_BUDGET / 2,
        "the child took {latency:?} to stop after the drop"
    );
}

#[tokio::test]
async fn a_normal_interrupt_is_one_listing_and_confirmed() {
    let (latency, report) = stop_latency(How::Interrupt, Duration::ZERO).await;
    assert!(latency < STOP_LISTING_BUDGET, "the stop took {latency:?}");
    let report = report.expect("a report");
    assert_eq!(report.cleanup, Cleanup::Confirmed, "{report:#?}");
}
