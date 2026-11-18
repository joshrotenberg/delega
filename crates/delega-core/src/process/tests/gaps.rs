//! Known gaps, written as the behaviour that is wanted. Each is ignored
//! because it fails today; run them with `cargo test -- --ignored`.
//!
//! Both leave one process running when they fail, so each stops it itself.

use std::sync::Arc;
use std::time::Duration;

use super::fixtures::{Reaper, Scratch, is_running, sh};
use crate::contract::result::Cleanup;
use crate::process::run;
use crate::process::tree::{Lister, list_processes};

const LONG: Duration = Duration::from_secs(60);

/// A lister that reads the real table and then drops a marker file, so a
/// script can start its descendant only after the first scan has finished.
/// That puts the descendant after the scan and before the next one.
fn marking_lister(marker: std::path::PathBuf) -> Lister {
    Arc::new(move || {
        let marker = marker.clone();
        Box::pin(async move {
            let rows = list_processes().await;
            let _ = std::fs::write(&marker, "scanned");
            rows
        })
    })
}

const WAIT_FOR_SCAN: &str = "while [ ! -e scanned ]; do sleep 0.02; done;";

/// The wanted behaviour: when a descendant is left running, the result must not
/// claim cleanup is confirmed.
fn assert_no_false_confirmation(descendant: i32, cleanup: &Cleanup) {
    let alive = is_running(descendant);
    assert!(
        !(alive && *cleanup == Cleanup::Confirmed),
        "descendant {descendant} is still running but cleanup says Confirmed"
    );
}

#[tokio::test]
#[ignore = "known gap, delega issue 1"]
async fn a_detached_grandchild_whose_parent_exits_at_once_is_not_reported_as_cleaned_up() {
    let s = Scratch::new();
    let script = format!(
        "{WAIT_FOR_SCAN} set -m; echo $$ > leader; \
         sleep 300 >/dev/null 2>&1 & echo $! > gc; exit 0"
    );
    let spec = sh(&s, &script, LONG).with_lister(marking_lister(s.path("scanned")));
    let report = run(spec).await;
    let gc = s.pid_now("gc");
    let _reaper = Reaper(vec![gc]);
    assert_no_false_confirmation(gc, &report.cleanup);
}

#[tokio::test]
#[ignore = "known gap, delega issue 1"]
async fn a_descendant_started_between_two_scans_before_a_crash_is_not_reported_as_cleaned_up() {
    let s = Scratch::new();
    // The crash lands 0.2 s after the descendant starts, before the next
    // periodic scan (0.5 s after the first), so only a scan at the moment of the
    // crash could have seen it.
    let script = format!(
        "{WAIT_FOR_SCAN} set -m; echo $$ > leader; \
         sleep 300 >/dev/null 2>&1 & echo $! > gc; sleep 0.2; kill -KILL $$; sleep 5"
    );
    let spec = sh(&s, &script, LONG).with_lister(marking_lister(s.path("scanned")));
    let report = run(spec).await;
    let gc = s.pid_now("gc");
    let _reaper = Reaper(vec![gc]);
    assert_no_false_confirmation(gc, &report.cleanup);
}
