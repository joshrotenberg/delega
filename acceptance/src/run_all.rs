//! Running many cases with bounded parallelism.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::matrix::Row;
use crate::runner::{Context, Measured, run_case};

/// Run every row `repeat` times on `jobs` threads. The result is in row order,
/// then repeat order.
pub fn run_all(
    ctx: &Context,
    rows: &[Row],
    jobs: usize,
    repeat: u32,
    quiet: bool,
) -> Vec<Measured> {
    // Repeat-major: every row once, then every row again, so a repeat is a
    // full run of the matrix under the same load.
    let work: Vec<(usize, u32)> = (1..=repeat)
        .flat_map(|r| (0..rows.len()).map(move |i| (i, r)))
        .collect();
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let results: Mutex<Vec<(usize, u32, Measured)>> = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..jobs.max(1) {
            scope.spawn(|| {
                loop {
                    let n = next.fetch_add(1, Ordering::SeqCst);
                    let Some(&(i, r)) = work.get(n) else { break };
                    let measured = run_until_timely(ctx, &rows[i], r);
                    let finished = done.fetch_add(1, Ordering::SeqCst) + 1;
                    if !quiet {
                        eprintln!(
                            "[{finished}/{}] {} #{r}: {}",
                            work.len(),
                            measured.id,
                            measured.verdict
                        );
                    }
                    if let Ok(mut list) = results.lock() {
                        list.push((i, r, measured));
                    }
                }
            });
        }
    });
    let mut list = results.into_inner().unwrap_or_default();
    list.sort_by_key(|(i, r, _)| (*i, *r));
    list.into_iter().map(|(_, _, m)| m).collect()
}

/// Runs of one row, at most, when the only fault is a late process listing.
const LISTING_ATTEMPTS: u32 = 5;

/// What `judge` says when the only difference from the expectation is that
/// cleanup was `Unconfirmed` where `Confirmed` was expected.
const CONFIRMED_MISMATCH: &str = "cleanup.confirmed is Some(false) where Some(true) was expected";

/// Run one case. When it fails because `delega` could not take the process
/// listing that precedes its first signal in time (its budget is 500 ms for a
/// requested stop and 3.1 s after a natural exit, and `ps` takes longer on a
/// machine under heavy load), run it again, up to [`LISTING_ATTEMPTS`] times in
/// all. A run is discarded only when [`only_a_late_listing`] holds: the one
/// problem is the cleanup mismatch, the verdict is exactly the late-listing
/// verdict, and the exit status, outcome and survivors were as expected. Any
/// other failure, or a pass, is reported as measured. The result carries the
/// number of runs and the reasons of the discarded ones.
fn run_until_timely(ctx: &Context, row: &Row, repeat: u32) -> Measured {
    let mut discarded = Vec::new();
    let mut measured = run_case(ctx, row, repeat);
    while measured.attempts < LISTING_ATTEMPTS && only_a_late_listing(&measured) {
        discarded.push(measured.cleanup_reason.clone().unwrap_or_default());
        let attempts = measured.attempts + 1;
        measured = run_case(ctx, row, repeat);
        measured.attempts = attempts;
    }
    measured.discarded = discarded;
    measured
}

/// Whether a failed measurement failed on the late listing and on nothing
/// else. `judge` adds a survivors problem whenever the survivors differ from
/// the expectation, and a problem for any exit status or outcome that differs,
/// so the problem list being exactly the cleanup mismatch covers all three.
pub fn only_a_late_listing(m: &Measured) -> bool {
    m.verdict == "fail"
        && m.cleanup_late_listing_only
        && m.expected_confirmed == Some(true)
        && m.problems == [CONFIRMED_MISMATCH]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failing() -> Measured {
        serde_json::from_str(
            r#"{"id":"x","repeat":1,"provider":"claude","case":"d","variant":"plain",
            "behaviour":"busy","action":"","timeout_secs":null,"exit_code":130,
            "exit_signal":null,"hang":false,"stdout_docs":1,"result_valid":true,
            "outcome":"interrupted","error_kind":null,"cleanup_confirmed":false,
            "cleanup_reason":"late","cleanup_pids":[],"cleanup_groups":[],
            "cleanup_late_listing_only":true,"provider_pid":null,
            "survivors_at_exit":[],"survivors":[],"honest":true,"latency_ms":null,
            "latency_kind":"","elapsed_ms":null,"expected_outcome":"interrupted",
            "expected_exit":{"Code":130},"expected_confirmed":true,
            "expected_survivors":[],"expected_leak":"none","expected_reason":null,
            "verdict":"fail",
            "problems":["cleanup.confirmed is Some(false) where Some(true) was expected"],
            "stderr_first":null}"#,
        )
        .expect("a measurement")
    }

    #[test]
    fn a_failure_that_is_only_the_late_listing_is_retried() {
        assert!(only_a_late_listing(&failing()));
    }

    #[test]
    fn a_failure_with_any_other_problem_is_not_retried() {
        let mut m = failing();
        m.problems
            .push("exit status 1 where 130 was expected".into());
        assert!(!only_a_late_listing(&m));
        let mut m = failing();
        m.problems
            .push("survivors provider where none was expected".into());
        assert!(!only_a_late_listing(&m));
        let mut m = failing();
        m.problems = vec!["outcome failed where interrupted was expected".into()];
        assert!(!only_a_late_listing(&m));
    }

    #[test]
    fn an_unconfirmed_verdict_for_another_reason_is_not_retried() {
        let mut m = failing();
        m.cleanup_late_listing_only = false;
        assert!(!only_a_late_listing(&m));
    }

    #[test]
    fn a_pass_or_an_expected_failure_is_not_retried() {
        let mut m = failing();
        m.verdict = "pass".into();
        assert!(!only_a_late_listing(&m));
    }
}
