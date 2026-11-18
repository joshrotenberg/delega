//! The descendants that escape, and the ones that no longer do.
//!
//! A descendant in its own process group but the child's session is found by
//! the session sweep, so the first two tests are ordinary regression tests. A
//! descendant that calls `setsid` escapes the sweep and, if no scan saw it
//! while its parent lived, is not known at all. That is the gap that remains;
//! the last two tests state the wanted behaviour and are ignored because they
//! fail today. Run them with `cargo test -- --ignored`. They leave one process
//! running when they fail, so each stops it itself.

use std::sync::Arc;
use std::time::Duration;

use super::fixtures::{OWN_GROUP, OWN_SESSION, Scratch, WAIT_ESCAPED, is_running, run_guarded, sh};
use crate::contract::result::Cleanup;
use crate::process::tree::{Lister, list_processes};

const LONG: Duration = Duration::from_secs(60);

/// A lister that reads the real table and then drops a marker file, so a
/// script can start its descendant only after the first scan has finished.
/// That puts the descendant after the scan and before the next one.
pub(super) fn marking_lister(marker: std::path::PathBuf) -> Lister {
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
async fn an_own_group_grandchild_whose_parent_exits_at_once_is_cleaned_up() {
    require_perl!();
    let s = Scratch::new();
    let script = format!("{WAIT_FOR_SCAN} echo $$ > leader; {OWN_GROUP} & echo $! > gc; exit 0");
    let spec = sh(&s, &script, LONG).with_lister(marking_lister(s.path("scanned")));
    let (report, pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
    assert_eq!(report.cleanup, Cleanup::Confirmed, "{report:#?}");
    super::fixtures::assert_gone("grandchild", &pids).await;
}

#[tokio::test]
async fn an_own_group_descendant_started_between_two_scans_before_a_crash_is_cleaned_up() {
    require_perl!();
    let s = Scratch::new();
    // The crash lands 0.2 s after the descendant starts, before the next
    // periodic scan (0.5 s after the first).
    let script = format!(
        "{WAIT_FOR_SCAN} echo $$ > leader; {OWN_GROUP} & echo $! > gc; \
         sleep 0.2; kill -KILL $$; sleep 5"
    );
    let spec = sh(&s, &script, LONG).with_lister(marking_lister(s.path("scanned")));
    let (report, pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
    assert_eq!(report.cleanup, Cleanup::Confirmed, "{report:#?}");
    super::fixtures::assert_gone("descendant", &pids).await;
}

#[tokio::test]
#[ignore = "known gap, delega issue 1"]
async fn a_setsid_grandchild_whose_parent_exits_at_once_is_not_reported_as_cleaned_up() {
    require_perl!();
    let s = Scratch::new();
    let script = format!(
        "{WAIT_FOR_SCAN} echo $$ > leader; {OWN_SESSION} >/dev/null 2>&1 & echo $! > gc; {WAIT_ESCAPED} exit 0"
    );
    let spec = sh(&s, &script, LONG).with_lister(marking_lister(s.path("scanned")));
    let (report, pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
    assert_no_false_confirmation(pids[0], &report.cleanup);
}

#[tokio::test]
#[ignore = "known gap, delega issue 1"]
async fn a_setsid_descendant_started_between_two_scans_before_a_crash_is_not_reported_as_cleaned_up()
 {
    require_perl!();
    let s = Scratch::new();
    let script = format!(
        "{WAIT_FOR_SCAN} echo $$ > leader; {OWN_SESSION} >/dev/null 2>&1 & echo $! > gc; {WAIT_ESCAPED} \
         sleep 0.2; kill -KILL $$; sleep 5"
    );
    let spec = sh(&s, &script, LONG).with_lister(marking_lister(s.path("scanned")));
    let (report, pids, _reaper) = run_guarded(&s, spec, &["gc"]).await;
    assert_no_false_confirmation(pids[0], &report.cleanup);
}
