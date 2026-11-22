//! A handful of rows of the acceptance matrix, so `cargo test --workspace`
//! notices a regression without running the whole matrix. The full run is
//! `cargo run -p delega-acceptance -- --all`.

use delega_acceptance::delega_bin;
use delega_acceptance::matrix;
use delega_acceptance::run_all::run_all;
use delega_acceptance::runner::Context;

#[test]
fn the_smoke_rows_meet_their_expectations_and_leave_nothing_running() {
    let delega = delega_bin::build(false).expect("build delega");
    let base = std::fs::canonicalize(std::env::temp_dir()).expect("temporary directory");
    let ctx = Context::new(delega, base, true);
    let rows = matrix::smoke();
    assert_eq!(rows.len(), matrix::SMOKE.len(), "a smoke row id is wrong");

    let measured = run_all(&ctx, &rows, 4, 1, true);

    // Visible with `--nocapture`: how many rows a late listing made repeat.
    let retried = measured.iter().filter(|m| m.attempts > 1).count();
    let attempts: u32 = measured.iter().map(|m| m.attempts).sum();
    eprintln!(
        "{retried} of {} smoke rows retried after a late process listing; {attempts} runs in total",
        measured.len()
    );

    let bad: Vec<String> = measured
        .iter()
        .filter(|m| !matches!(m.verdict.as_str(), "pass" | "expected_failure"))
        .map(|m| format!("{}: {}: {}", m.id, m.verdict, m.problems.join("; ")))
        .collect();
    assert!(
        bad.is_empty(),
        "rows that did not meet their expectation:\n{}",
        bad.join("\n")
    );

    // Nothing the cases started may be left, whatever delega reported.
    let marker = ctx.run_marker();
    let mut left: Vec<_> = delega_acceptance::procs::ps()
        .into_iter()
        .filter(|p| !p.stat.starts_with('Z') && p.command.contains(&marker))
        .collect();
    let bare = ctx.bare.lock().map(|b| b.clone()).unwrap_or_default();
    left.extend(delega_acceptance::procs::live_bare(&bare));
    assert!(left.is_empty(), "processes left behind: {left:?}");
}

#[test]
fn every_row_has_a_unique_id_and_a_known_variant() {
    let rows = matrix::all();
    let mut ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    let total = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), total, "two rows share an id");
    for id in matrix::SMOKE {
        assert!(ids.contains(id), "smoke row {id} is not in the matrix");
    }
}
