//! Running many cases with bounded parallelism.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::matrix::Row;
use crate::runner::{Context, GAP_CLOSED, GAP_NOT_SHOWN, Measured, cleanup_mismatch, run_case};

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

/// The smallest `--repeat` at which a gap that never showed counts as closed.
/// With fewer repeats one quiet run says too little.
pub const CLOSE_AFTER: u32 = 3;

/// The fewest timing-dependent known-gap runs in one run of the harness for
/// "none of them showed its gap" to mean anything. The matrix has 30 gap runs,
/// 22 of them timing dependent; a filtered run of one or two rows is not
/// evidence.
pub const CLOSE_ALL_AFTER: usize = 3;

/// Mark known gaps as closed, and return the ids of the rows marked, in row
/// order. Two rules, both on timing-dependent known-gap rows, and both turn
/// `gap_not_shown` into `gap_closed`, which fails the run:
///
/// - A row that showed no gap in any of `repeat` runs, with `repeat` at least
///   [`CLOSE_AFTER`]. A row that showed its gap in even one repeat keeps its
///   verdicts.
/// - Every timing-dependent known-gap run of this harness run, when there are
///   at least [`CLOSE_ALL_AFTER`] of them. One row failing to show its gap is
///   load; all of them failing at once is a fix, or a harness that stopped
///   measuring.
pub fn close_gaps(measured: &mut [Measured], repeat: u32) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    let timing = |m: &Measured| m.timing_dependent && m.verdict != "skipped";
    let gap_runs: Vec<&Measured> = measured.iter().filter(|m| timing(m)).collect();
    if gap_runs.len() >= CLOSE_ALL_AFTER && gap_runs.iter().all(|m| m.verdict == GAP_NOT_SHOWN) {
        for m in measured.iter_mut().filter(|m| m.timing_dependent) {
            m.verdict = GAP_CLOSED.to_string();
            m.problems.push(
                "no timing-dependent known gap showed in this run; correct the table".to_string(),
            );
            if !ids.contains(&m.id) {
                ids.push(m.id.clone());
            }
        }
        return ids;
    }
    if repeat < CLOSE_AFTER {
        return ids;
    }
    let mut all_ids: Vec<String> = Vec::new();
    for m in measured.iter() {
        if !all_ids.contains(&m.id) {
            all_ids.push(m.id.clone());
        }
    }
    let wanted = usize::try_from(repeat).unwrap_or(usize::MAX);
    all_ids.retain(|id| {
        let runs: Vec<&Measured> = measured.iter().filter(|m| &m.id == id).collect();
        runs.len() == wanted && runs.iter().all(|m| m.verdict == GAP_NOT_SHOWN)
    });
    for m in measured.iter_mut().filter(|m| all_ids.contains(&m.id)) {
        m.verdict = GAP_CLOSED.to_string();
        m.problems.push(format!(
            "the gap did not show in any of {repeat} runs; correct the table"
        ));
    }
    all_ids
}

/// Runs of one row, at most, when the only fault is a late process listing.
const LISTING_ATTEMPTS: u32 = 5;

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
        && m.problems == [cleanup_mismatch(Some(false), Some(true))]
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
            "problems":[],
            "stderr_first":null}"#,
        )
        .map(|mut m: Measured| {
            m.problems = vec![cleanup_mismatch(Some(false), Some(true))];
            m
        })
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

#[cfg(test)]
mod gap_tests {
    use super::*;

    fn row(id: &str, repeat: u32, verdict: &str) -> Measured {
        let mut m: Measured = serde_json::from_str(
            r#"{"id":"x","repeat":1,"provider":"claude","case":"a","variant":"relay",
            "behaviour":"success","action":"","timeout_secs":null,"exit_code":0,
            "exit_signal":null,"hang":false,"stdout_docs":1,"result_valid":true,
            "outcome":"completed","error_kind":null,"cleanup_confirmed":true,
            "cleanup_reason":null,"cleanup_pids":[],"cleanup_groups":[],
            "provider_pid":null,"survivors_at_exit":[],"survivors":[],"honest":true,
            "latency_ms":null,"latency_kind":"","elapsed_ms":null,
            "expected_outcome":"completed","expected_exit":{"Code":0},
            "expected_confirmed":true,"expected_survivors":["relay-heartbeat"],
            "expected_leak":"documented_gap","expected_reason":null,
            "verdict":"","problems":[],"stderr_first":null}"#,
        )
        .expect("a measurement");
        m.id = id.to_string();
        m.timing_dependent = true;
        m.repeat = repeat;
        m.verdict = verdict.to_string();
        m
    }

    fn runs(id: &str, verdicts: &[&str]) -> Vec<Measured> {
        (1..)
            .zip(verdicts)
            .map(|(r, v)| row(id, r, v))
            .collect::<Vec<_>>()
    }

    fn verdicts(list: &[Measured]) -> Vec<&str> {
        list.iter().map(|m| m.verdict.as_str()).collect()
    }

    #[test]
    fn a_gap_absent_in_every_one_of_three_repeats_is_closed() {
        let mut list = runs("a-relay", &[GAP_NOT_SHOWN; 3]);
        assert_eq!(close_gaps(&mut list, 3), ["a-relay"]);
        assert_eq!(verdicts(&list), [GAP_CLOSED; 3]);
        assert!(!list[0].problems.is_empty());
    }

    #[test]
    fn a_gap_that_showed_once_stays_open() {
        let mut list = runs(
            "a-relay",
            &[GAP_NOT_SHOWN, "expected_failure", GAP_NOT_SHOWN],
        );
        assert!(close_gaps(&mut list, 3).is_empty());
        assert_eq!(
            verdicts(&list),
            [GAP_NOT_SHOWN, "expected_failure", GAP_NOT_SHOWN]
        );
    }

    #[test]
    fn fewer_than_three_repeats_never_close_a_gap() {
        let mut list = runs("a-relay", &[GAP_NOT_SHOWN; 2]);
        assert!(close_gaps(&mut list, 2).is_empty());
        let mut list = runs("a-relay", &[GAP_NOT_SHOWN]);
        assert!(close_gaps(&mut list, 1).is_empty());
        assert_eq!(verdicts(&list), [GAP_NOT_SHOWN]);
    }

    #[test]
    fn a_run_where_every_timing_gap_row_shows_nothing_is_closed_even_once() {
        let mut list: Vec<Measured> = (0..3)
            .map(|i| row(&format!("r{i}"), 1, GAP_NOT_SHOWN))
            .collect();
        assert_eq!(close_gaps(&mut list, 1).len(), 3);
        assert_eq!(verdicts(&list), [GAP_CLOSED; 3]);
    }

    #[test]
    fn one_timing_gap_row_that_shows_keeps_a_single_run_open() {
        let mut list: Vec<Measured> = (0..3)
            .map(|i| row(&format!("r{i}"), 1, GAP_NOT_SHOWN))
            .collect();
        list[1].verdict = "expected_failure".to_string();
        assert!(close_gaps(&mut list, 1).is_empty());
    }

    #[test]
    fn a_run_with_fewer_than_three_timing_gap_rows_is_not_closed() {
        let mut list: Vec<Measured> = (0..2)
            .map(|i| row(&format!("r{i}"), 1, GAP_NOT_SHOWN))
            .collect();
        assert!(close_gaps(&mut list, 1).is_empty());
    }

    #[test]
    fn rows_that_do_not_depend_on_timing_do_not_count_toward_closing_a_run() {
        let mut list: Vec<Measured> = (0..3)
            .map(|i| row(&format!("r{i}"), 1, GAP_NOT_SHOWN))
            .collect();
        for m in &mut list {
            m.timing_dependent = false;
        }
        assert!(close_gaps(&mut list, 1).is_empty());
    }

    #[test]
    fn only_the_row_with_no_gap_is_closed() {
        let mut list = runs("a-relay", &[GAP_NOT_SHOWN; 3]);
        list.extend(runs(
            "c-orphan",
            &["expected_failure", GAP_NOT_SHOWN, "expected_failure"],
        ));
        assert_eq!(close_gaps(&mut list, 3), ["a-relay"]);
        assert_eq!(list[3].verdict, "expected_failure");
    }
}
