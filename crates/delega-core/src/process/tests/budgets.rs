//! The production listing budgets, pinned. Test builds start every spec with
//! waits far longer than any load adds (`Timings::generous`), so the tests that
//! run under load do not depend on how fast `ps` is. These tests are the ones
//! that keep the real values honest: the constants themselves, the verdict that
//! names them, and two runs with `Timings::production()` in which the listing
//! is held back and the order and the least elapsed time are asserted. Neither
//! asserts an upper bound that load could break: the listing is held until the
//! test has seen the signal go out.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime};

use super::fixtures::{Held, Reaper, Scratch, is_running_async, sh};
use crate::contract::result::Cleanup;
use crate::process::limits::{
    DESCENDANT_SIGNAL_BUDGET, EXIT_LISTING_BUDGET, FINAL_CLEANUP_PASS, PS_TIMEOUT,
    STOP_LISTING_BUDGET, TAP_QUEUE_CHUNKS,
};
use crate::process::spec::Timings;
use crate::process::{
    Ending, LATE_LISTING_REASON, ProcessHandle, ProcessReport, ProcessSpec, is_late_listing_only,
};

const LONG: Duration = Duration::from_secs(60);
/// How long a test waits for a held stop to take effect before it gives up.
const DEADLINE: Duration = Duration::from_secs(60);

#[test]
fn the_production_budgets_are_the_documented_values() {
    assert_eq!(STOP_LISTING_BUDGET, Duration::from_millis(500));
    assert_eq!(PS_TIMEOUT, Duration::from_millis(1500));
    assert_eq!(EXIT_LISTING_BUDGET, Duration::from_millis(3100));
    assert_eq!(DESCENDANT_SIGNAL_BUDGET, Duration::from_millis(250));
    assert_eq!(FINAL_CLEANUP_PASS, Duration::from_secs(1));
    assert_eq!(TAP_QUEUE_CHUNKS, 256);
}

#[test]
fn the_budgets_keep_the_relations_the_design_depends_on() {
    // A caller waits for a requested stop, so it waits less than after a natural
    // exit, where nothing is delayed for anyone.
    assert!(STOP_LISTING_BUDGET < EXIT_LISTING_BUDGET);
    // The exit budget is two listing timeouts and a margin: the scan in progress
    // at the exit cannot stand in, so up to two runs of `ps` may follow.
    assert_eq!(
        EXIT_LISTING_BUDGET,
        2 * PS_TIMEOUT + Duration::from_millis(100)
    );
    // One listing fits inside a requested stop's wait only when it is quick; the
    // sweep after a signal never waits longer than that wait.
    assert!(DESCENDANT_SIGNAL_BUDGET < STOP_LISTING_BUDGET);
    // The hard stop after a panic spends half the final pass on its listing.
    assert!(STOP_LISTING_BUDGET <= FINAL_CLEANUP_PASS);
}

#[test]
fn a_production_spec_takes_the_constants_and_a_test_spec_does_not() {
    let production = Timings::production();
    assert_eq!(production.stop_listing, STOP_LISTING_BUDGET);
    assert_eq!(production.exit_listing, EXIT_LISTING_BUDGET);
    assert_eq!(production.final_cleanup_pass, FINAL_CLEANUP_PASS);
    assert_eq!(production.descendant_signal, DESCENDANT_SIGNAL_BUDGET);
    assert_eq!(production.tap_queue_chunks, TAP_QUEUE_CHUNKS);
    // The two budgets are not interchangeable.
    assert_ne!(production.stop_listing, production.exit_listing);
    // A test build's spec is generous, so a test that does not test a wait never
    // meets the late listing.
    let spec = ProcessSpec::new("/bin/true", LONG);
    assert_eq!(spec.timings, Timings::generous());
    assert!(spec.timings.stop_listing > production.stop_listing);
}

#[test]
fn the_late_listing_verdict_has_the_reason_the_library_produces() {
    // The text is the reason of the verdict that `delega run` prints and that the
    // tests of the binary and of the acceptance harness recognise.
    assert_eq!(
        LATE_LISTING_REASON,
        "the process listing taken before the first signal did not complete, so a \
         descendant that left the session when its parent was stopped could not be \
         ruled out; the group and session sweeps afterwards found nothing"
    );
    let unconfirmed = |pids: Vec<u32>, groups: Vec<u32>, reason: String| Cleanup::Unconfirmed {
        pids,
        process_groups: groups,
        reason,
    };
    let late = || LATE_LISTING_REASON.to_string();
    assert!(is_late_listing_only(&unconfirmed(vec![], vec![], late())));
    // The version probe and the run each missed their listing.
    assert!(is_late_listing_only(&unconfirmed(
        vec![],
        vec![],
        format!("{}; {}", late(), late())
    )));
    assert!(!is_late_listing_only(&Cleanup::Confirmed));
    assert!(!is_late_listing_only(&unconfirmed(vec![7], vec![], late())));
    assert!(!is_late_listing_only(&unconfirmed(vec![], vec![7], late())));
    assert!(!is_late_listing_only(&unconfirmed(
        vec![],
        vec![],
        format!("{}; stopping was not confirmed within 10 s: x", late())
    )));
    // Codex with MCP discovery merges three reports.
    assert!(is_late_listing_only(&unconfirmed(
        vec![],
        vec![],
        format!("{}; {}; {}", late(), late(), late())
    )));
    assert!(is_late_listing_only(&unconfirmed(
        vec![],
        vec![],
        vec![late(); 5].join("; ")
    )));
    // A repeat must be the whole reason, joined by the separator, with nothing
    // before, between or after.
    for reason in [
        String::new(),
        format!("{}; ", late()),
        format!("{};{}", late(), late()),
        format!("{} {}", late(), late()),
        format!("x; {}", late()),
        format!("{}; {}; other", late(), late()),
        format!("{}; {}x", late(), late()),
    ] {
        assert!(
            !is_late_listing_only(&unconfirmed(vec![], vec![], reason.clone())),
            "{reason:?}"
        );
    }
    assert!(!is_late_listing_only(&unconfirmed(
        vec![],
        vec![],
        "process inspection was unavailable".into()
    )));
}

async fn wait_gone_while_held(what: &str, pid: i32) {
    let began = Instant::now();
    while is_running_async(pid).await {
        assert!(
            began.elapsed() < DEADLINE,
            "{what}: still running after {DEADLINE:?}; the signal was held by the listing"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn first_signal_at(report: &ProcessReport) -> SystemTime {
    report
        .diagnostics
        .signals
        .first()
        .expect("a signal was recorded")
        .at
}

/// The owner's warning names the budget it waited, in milliseconds. This is
/// load-free: a use site that waited a different budget than the one its
/// field says, or a swapped pair of budgets, names the wrong number.
fn assert_warns_of_budget(report: &ProcessReport, budget: Duration) {
    let expected = format!(
        "The process listing before the first signal did not complete within {} ms",
        budget.as_millis()
    );
    let warnings = &report.diagnostics.warnings;
    assert!(
        warnings.iter().any(|w| w.contains(&expected)),
        "no warning contains {expected:?}: {warnings:#?}"
    );
    let others = warnings
        .iter()
        .filter(|w| w.contains("did not complete within"))
        .count();
    assert_eq!(others, 1, "{warnings:#?}");
}

#[tokio::test]
async fn a_requested_stop_signals_after_the_real_stop_budget_and_is_unconfirmed() {
    // The listing is held for the whole stop. The child must be gone while it is
    // still held (the stop did not wait for the listing), the first signal must
    // not precede the stop budget (the stop did wait that long), and the verdict
    // is the late listing's.
    let s = Scratch::new();
    let held = Held::new(true);
    let mut spec = sh(&s, "echo $$ > leader; sleep 60", LONG).with_lister(held.lister());
    spec.capture_interval = Some(Duration::from_secs(3600));
    spec.timings = Timings::production();
    let handle = ProcessHandle::start(spec);
    let mut reaper = Reaper::default();
    let leader = reaper.watch(&s, "leader").await;
    let asked_at = SystemTime::now();
    let asked = Instant::now();
    handle.interrupt();
    wait_gone_while_held("requested stop", leader).await;
    let waited = asked.elapsed();
    assert!(held.waiting.load(Ordering::SeqCst) >= 1);
    held.release();
    let report = handle.wait().await;
    assert!(
        waited >= STOP_LISTING_BUDGET,
        "the child was gone {waited:?} after the interrupt, before the {STOP_LISTING_BUDGET:?} budget"
    );
    assert!(first_signal_at(&report) >= asked_at + STOP_LISTING_BUDGET);
    assert_eq!(report.ending, Ending::Interrupted, "{report:#?}");
    assert!(is_late_listing_only(&report.cleanup), "{report:#?}");
    assert_warns_of_budget(&report, STOP_LISTING_BUDGET);
}

#[tokio::test]
async fn a_natural_exit_with_a_held_listing_signals_after_the_real_exit_budget_and_is_unconfirmed()
{
    // The child exits at once and leaves a descendant in its group. The listing
    // before the first signal is held, so the signal that stops the descendant
    // is the one that follows the exit budget: it cannot come sooner than the
    // budget after the start. This also fails if the stop and exit budgets are
    // swapped, since a stop budget would signal 2.6 s sooner.
    let s = Scratch::new();
    let held = Held::new(true);
    let mut spec = sh(
        &s,
        "echo $$ > leader; sleep 60 & echo $! > gc; exit 0",
        LONG,
    )
    .with_lister(held.lister());
    spec.capture_interval = Some(Duration::from_secs(3600));
    spec.timings = Timings::production();
    // The child exits right after it writes `gc`, and the test may read it
    // late, so the bounds are from before the child could have started.
    let before_start_at = SystemTime::now();
    let before_start = Instant::now();
    let handle = ProcessHandle::start(spec);
    let mut reaper = Reaper::default();
    let gc = reaper.watch(&s, "gc").await;
    wait_gone_while_held("descendant after a natural exit", gc).await;
    let waited = before_start.elapsed();
    held.release();
    let report = handle.wait().await;
    assert!(
        waited >= EXIT_LISTING_BUDGET,
        "the descendant was gone {waited:?} after the start, before the {EXIT_LISTING_BUDGET:?} exit budget"
    );
    assert!(first_signal_at(&report) >= before_start_at + EXIT_LISTING_BUDGET);
    assert_eq!(report.ending, Ending::Exited { code: 0 }, "{report:#?}");
    assert!(is_late_listing_only(&report.cleanup), "{report:#?}");
    assert_warns_of_budget(&report, EXIT_LISTING_BUDGET);
}
