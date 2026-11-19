//! Output bounds, standard input, environment and working directory.

use std::time::Duration;

use super::fixtures::{Scratch, assert_confirmed, sh};
use crate::process::{CaptureSpec, Ending, EnvPolicy, Keep, ProcessSpec, run};

const LONG: Duration = Duration::from_secs(60);

#[tokio::test]
async fn output_larger_than_the_bound_keeps_the_head_and_counts_everything() {
    let s = Scratch::new();
    let mut spec = sh(
        &s,
        "printf START; head -c 3000000 /dev/zero | tr '\\0' a",
        LONG,
    );
    spec.stdout = CaptureSpec {
        limit: 8,
        keep: Keep::Head,
    };
    let report = run(spec).await;
    assert_eq!(report.ending, Ending::Exited { code: 0 });
    assert_eq!(report.stdout.text(), "STARTaaa");
    assert_eq!(report.stdout.total_bytes, 3_000_005);
    assert!(report.stdout.truncated);
    assert_confirmed(&report);
}

#[tokio::test]
async fn error_output_larger_than_the_bound_keeps_the_tail() {
    let s = Scratch::new();
    let mut spec = sh(
        &s,
        "head -c 100000 /dev/zero | tr '\\0' a >&2; printf END >&2",
        LONG,
    );
    spec.stderr = CaptureSpec {
        limit: 10,
        keep: Keep::Tail,
    };
    let report = run(spec).await;
    assert_eq!(report.stderr.text(), "aaaaaaaEND");
    assert_eq!(report.stderr.total_bytes, 100_003);
    assert!(report.stderr.truncated);
}

#[tokio::test]
async fn both_streams_flooding_at_once_do_not_block_the_child() {
    let s = Scratch::new();
    let mut spec = sh(
        &s,
        "head -c 2000000 /dev/zero | tr '\\0' a >&2 & head -c 2000000 /dev/zero | tr '\\0' b; wait",
        Duration::from_secs(30),
    );
    spec.stdout.limit = 100;
    spec.stderr.limit = 100;
    let report = run(spec).await;
    assert_eq!(report.ending, Ending::Exited { code: 0 }, "{report:#?}");
    assert_eq!(report.stdout.total_bytes, 2_000_000);
    assert_eq!(report.stderr.total_bytes, 2_000_000);
    assert_eq!(report.stdout.bytes.len(), 100);
    assert_confirmed(&report);
}

#[tokio::test]
async fn standard_input_content_reaches_the_child_and_is_then_closed() {
    let s = Scratch::new();
    let spec = ProcessSpec::new("/bin/cat", LONG)
        .cwd(s.dir())
        .env(EnvPolicy::empty())
        .stdin("hello from stdin");
    let report = run(spec).await;
    assert_eq!(report.ending, Ending::Exited { code: 0 });
    assert_eq!(report.stdout.text(), "hello from stdin");
}

#[tokio::test]
async fn large_standard_input_to_a_child_that_never_reads_does_not_hang_or_warn() {
    let s = Scratch::new();
    let spec = sh(&s, "exit 0", LONG).stdin(vec![b'x'; 4 * 1024 * 1024]);
    let report = run(spec).await;
    assert_eq!(report.ending, Ending::Exited { code: 0 });
    assert!(
        report
            .diagnostics
            .warnings
            .iter()
            .all(|w| !w.contains("Standard input")),
        "{:?}",
        report.diagnostics.warnings
    );
    assert_confirmed(&report);
}

#[tokio::test]
async fn the_env_policy_decides_what_the_child_sees() {
    let s = Scratch::new();
    let spec = ProcessSpec::new("/usr/bin/env", LONG)
        .cwd(s.dir())
        .env(EnvPolicy::empty().set("DELEGA_A", "1").set("DELEGA_B", "2"));
    let report = run(spec).await;
    let mut lines: Vec<_> = report.stdout.text().lines().map(str::to_owned).collect();
    lines.sort();
    assert_eq!(lines, ["DELEGA_A=1", "DELEGA_B=2"]);
}

#[tokio::test]
async fn the_working_directory_is_the_one_asked_for() {
    let s = Scratch::new();
    let spec = sh(&s, "pwd -P", LONG);
    let report = run(spec).await;
    let expected = std::fs::canonicalize(s.dir()).unwrap();
    assert_eq!(report.stdout.text().trim(), expected.to_str().unwrap());
}
