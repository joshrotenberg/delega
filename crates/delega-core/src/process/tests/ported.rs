//! Tests carried over from the project this layer came from, adapted to this interface.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::fixtures::{RECORD, Record, Scratch, assert_confirmed, read_record};
use crate::process::sys::leader_has_exited;
use crate::process::tree::{Lister, list_processes};
use crate::process::{Ending, ProcessSpec, run};

/// A child that exits before the first process listing is still identified:
/// the lister waits for the exit and then reads a real table.
#[tokio::test]
async fn a_fast_child_is_identified_when_the_first_snapshot_follows_its_exit() {
    let s = Scratch::new();
    let pid_file = s.path("leader");
    let observed_after_exit = Arc::new(AtomicBool::new(false));
    let confirmed = observed_after_exit.clone();
    let lister: Lister = Arc::new(move || {
        let pid_file = pid_file.clone();
        let confirmed = confirmed.clone();
        Box::pin(async move {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
            loop {
                let pid = match read_record(&pid_file) {
                    Record::Pid(pid) => Some(pid),
                    Record::Absent | Record::Incomplete(_) => None,
                };
                if pid.is_some_and(leader_has_exited) {
                    confirmed.store(true, Ordering::SeqCst);
                    return list_processes().await;
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(crate::process::tree::InspectError::Failed(
                        "fixture exit was not observed".into(),
                    ));
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
    });
    let spec = ProcessSpec::new("/bin/sh", Duration::from_secs(60))
        .args([
            "-c".to_owned(),
            format!("{RECORD}record leader $$; printf '[]'; exit 0"),
        ])
        .cwd(s.dir())
        .env(crate::process::EnvPolicy::empty())
        .with_lister(lister);
    let report = run(spec).await;
    assert!(observed_after_exit.load(Ordering::SeqCst));
    assert_eq!(report.ending, Ending::Exited { code: 0 });
    assert_eq!(report.stdout.text(), "[]");
    assert!(
        report.diagnostics.warnings.is_empty(),
        "{:?}",
        report.diagnostics.warnings
    );
    assert!(report.diagnostics.inspection.root_identity_observed);
    assert!(!report.diagnostics.capture_gap_observed);
    assert_confirmed(&report);
}
