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
                    let measured = run_case(ctx, &rows[i], r);
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
