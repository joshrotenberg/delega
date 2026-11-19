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

/// Attempts at one row whose only fault is a late process listing.
const LISTING_ATTEMPTS: u32 = 5;

/// Run one case. When it fails because `delega` could not take the process
/// listing that precedes its first signal in time (its budget is 500 ms for a
/// requested stop and 3.1 s after a natural exit, and `ps` takes longer on a
/// machine under heavy load), run it again, up to [`LISTING_ATTEMPTS`] times in
/// all. That verdict is the documented honest answer to a slow machine, not a
/// property of the row, so the row is judged on a run in which the listing was
/// on time. Nothing else is retried: a row that fails any other way, or that
/// meets its expectation, is reported as measured, and a row whose listing is
/// late every time still fails with that reason.
fn run_until_timely(ctx: &Context, row: &Row, repeat: u32) -> Measured {
    let mut measured = run_case(ctx, row, repeat);
    for _ in 1..LISTING_ATTEMPTS {
        if !failed_on_a_late_listing(&measured) {
            break;
        }
        measured = run_case(ctx, row, repeat);
    }
    measured
}

fn failed_on_a_late_listing(m: &Measured) -> bool {
    m.verdict == "fail"
        && m.cleanup_confirmed == Some(false)
        && m.expected_confirmed == Some(true)
        && m.cleanup_reason
            .as_deref()
            .is_some_and(|r| r.contains("before the first signal did not complete"))
}
