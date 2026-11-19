//! The stdout tap: it sees output, and it cannot delay or damage the run.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::fixtures::{Scratch, assert_confirmed, sh};
use crate::process::{Ending, OutputTap, run};

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

#[tokio::test]
async fn a_slow_tap_does_not_stretch_the_time_limit_or_lose_capture() {
    let s = Scratch::new();
    // Output arrives at once, then the child runs past its one second limit.
    let mut spec = sh(
        &s,
        "printf first; sleep 0.1; printf second; sleep 30",
        Duration::from_secs(1),
    );
    let tap: OutputTap = Arc::new(|_| std::thread::sleep(Duration::from_secs(3)));
    spec.stdout_tap = Some(tap);
    let began = Instant::now();
    let report = run(spec).await;
    let took = began.elapsed();
    assert!(
        matches!(report.ending, Ending::TimedOut { .. }),
        "{report:#?}"
    );
    assert!(took < Duration::from_millis(2500), "took {took:?}");
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
    let s = Scratch::new();
    // Many small writes, each its own chunk, against a tap that sleeps.
    let mut spec = sh(
        &s,
        "i=0; while [ $i -lt 600 ]; do printf x; sleep 0.002; i=$((i+1)); done",
        Duration::from_secs(20),
    );
    let tap: OutputTap = Arc::new(|_| std::thread::sleep(Duration::from_millis(100)));
    spec.stdout_tap = Some(tap);
    let report = run(spec).await;
    assert_eq!(report.stdout.text().len(), 600);
    assert!(
        report
            .diagnostics
            .warnings
            .iter()
            .any(|w| w.contains("not delivered")),
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
