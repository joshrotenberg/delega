//! The stdout tap: it sees output, and it cannot delay or damage the run.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::fixtures::{Scratch, assert_confirmed, await_event, sh};
use crate::process::{Ending, OutputTap, ProcessHandle, run};

#[tokio::test]
async fn a_tap_sees_the_output_in_order() {
    let s = Scratch::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let mut spec = sh(&s, "printf one; printf two", Duration::from_secs(10));
    let tap: OutputTap = Arc::new(move |chunk| sink.lock().expect("lock").extend_from_slice(chunk));
    spec.stdout_tap = Some(tap);
    let report = run(spec).await;
    assert_eq!(report.stdout.text(), "onetwo");
    assert_eq!(seen.lock().expect("lock").as_slice(), b"onetwo");
    assert_confirmed(&report);
}

/// A tap that does not return until `release` is set, standing for one that is
/// stuck. It polls, because it runs on a thread of its own.
fn stuck_tap(release: Arc<AtomicBool>) -> OutputTap {
    Arc::new(move |_| {
        while !release.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
    })
}

#[tokio::test]
async fn a_slow_tap_does_not_stretch_the_time_limit_or_lose_capture() {
    // Proves: a tap that never returns does not hold the run. The report must
    // arrive while the tap is still stuck (it is released only afterwards), so
    // no duration is compared: a run that waited for the tap would not return
    // until the test's own deadline.
    let s = Scratch::new();
    let release = Arc::new(AtomicBool::new(false));
    // Output arrives at once, then the child runs past its time limit. The
    // limit is long against the two writes it follows and short for a test.
    let mut spec = sh(
        &s,
        "printf first; printf second; sleep 30",
        Duration::from_secs(2),
    );
    spec.stdout_tap = Some(stuck_tap(release.clone()));
    let report = tokio::time::timeout(Duration::from_secs(60), run(spec))
        .await
        .expect("the run was held by a stuck tap");
    assert!(
        !release.load(Ordering::SeqCst),
        "the report came back only after the tap was released"
    );
    release.store(true, Ordering::SeqCst);
    assert!(
        matches!(report.ending, Ending::TimedOut { .. }),
        "{report:#?}"
    );
    assert_eq!(report.stdout.text(), "firstsecond");
    assert!(
        report
            .diagnostics
            .warnings
            .iter()
            .any(|w| w.contains("left behind")),
        "{:?}",
        report.diagnostics.warnings
    );
    assert_confirmed(&report);
}

#[tokio::test]
async fn a_tap_that_cannot_keep_up_drops_chunks_and_says_so() {
    // Proves: when the tap's queue is full the chunk is dropped, counted and
    // reported, and the capture still holds every byte. The drops are decided
    // by the queue, not by speed: the tap is stuck, the queue holds two chunks,
    // and the child keeps writing until the test has seen the first drop.
    let s = Scratch::new();
    let release = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicU64::new(0));
    let mut spec = sh(
        &s,
        "i=0; while [ ! -e stop ]; do printf x; i=$((i+1)); done; printf '\\n%s' $i",
        Duration::from_secs(120),
    );
    spec.timings.tap_queue_chunks = 2;
    spec.stdout_tap = Some(stuck_tap(release.clone()));
    spec.stdout_tap_dropped = Some(dropped.clone());
    let handle = ProcessHandle::start(spec);
    await_event("the first dropped chunk", || {
        dropped.load(Ordering::SeqCst) >= 1
    })
    .await;
    std::fs::write(s.path("stop"), "").expect("write the stop file");
    release.store(true, Ordering::SeqCst);
    let report = handle.wait().await;
    assert_eq!(report.ending, Ending::Exited { code: 0 }, "{report:#?}");
    // The capture lost nothing: n bytes of `x`, a newline and n.
    let text = report.stdout.text();
    let (xs, count) = text.split_once('\n').expect("the child printed its count");
    assert_eq!(xs.len().to_string(), count, "capture lost bytes");
    assert!(xs.bytes().all(|b| b == b'x'));
    let dropped = dropped.load(Ordering::SeqCst);
    assert!(dropped >= 1);
    let expected = format!("{dropped} chunks of standard output were not delivered");
    assert!(
        report
            .diagnostics
            .warnings
            .iter()
            .any(|w| w.contains(&expected)),
        "{:?}",
        report.diagnostics.warnings
    );
}

#[tokio::test]
async fn a_panicking_tap_loses_neither_the_capture_nor_the_ending() {
    let s = Scratch::new();
    let mut spec = sh(
        &s,
        "printf hello; sleep 0.1; printf world",
        Duration::from_secs(10),
    );
    let tap: OutputTap = Arc::new(|_| panic!("the tap failed"));
    spec.stdout_tap = Some(tap);
    let report = run(spec).await;
    assert_eq!(report.ending, Ending::Exited { code: 0 });
    assert_eq!(report.stdout.text(), "helloworld");
    assert!(
        report
            .diagnostics
            .warnings
            .iter()
            .any(|w| w.contains("panicked")),
        "{:?}",
        report.diagnostics.warnings
    );
    assert_confirmed(&report);
}
