//! The committed acceptance results and the documents generated from them stay
//! in step with the matrix and the report generator. A change to a row, a
//! reason text or the report format fails here until the results are
//! regenerated as `acceptance/README.md` describes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use delega_acceptance::matrix;
use delega_acceptance::report;
use delega_acceptance::runner::Measured;

const COMMITTED: &[&str] = &[
    "results.jsonl",
    "results.meta.json",
    "repeats.jsonl",
    "spike-numbers.jsonl",
];

/// Inputs of `--report` that exist only after a recorded live run.
const OPTIONAL: &[&str] = &["live.jsonl"];

fn acceptance_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rows(path: &Path) -> Vec<Measured> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(i, line)| {
            serde_json::from_str(line)
                .unwrap_or_else(|err| panic!("{}:{}: {err}", path.display(), i + 1))
        })
        .collect()
}

const REGENERATE: &str = "regenerate them as acceptance/README.md describes (a full run, the \
                          repeats, then --report)";

#[test]
fn the_committed_reports_are_what_report_generates_from_the_committed_results() {
    let dir = acceptance_dir();
    let scratch = tempfile::tempdir().expect("temporary directory");
    for name in COMMITTED {
        std::fs::copy(dir.join(name), scratch.path().join(name))
            .unwrap_or_else(|err| panic!("copy {name}: {err}"));
    }
    for name in OPTIONAL {
        if dir.join(name).exists() {
            std::fs::copy(dir.join(name), scratch.path().join(name))
                .unwrap_or_else(|err| panic!("copy {name}: {err}"));
        }
    }
    report::regenerate(scratch.path(), &scratch.path().join("results.jsonl"))
        .expect("regenerate the reports");
    for name in ["RESULTS.md", "COMPARISON.md"] {
        let committed = std::fs::read_to_string(dir.join(name)).expect("committed report");
        let generated = std::fs::read_to_string(scratch.path().join(name)).expect("generated");
        assert!(
            committed == generated,
            "acceptance/{name} is not what --report generates from the committed results; {REGENERATE}"
        );
    }
}

#[test]
fn the_committed_results_cover_exactly_the_matrix_rows_with_their_current_expectations() {
    let matrix: BTreeMap<String, matrix::Row> = matrix::all()
        .into_iter()
        .map(|r| (r.id.clone(), r))
        .collect();
    for (name, runs) in [("results.jsonl", 1u32), ("repeats.jsonl", 3)] {
        let committed = rows(&acceptance_dir().join(name));
        let ids: BTreeSet<&str> = committed.iter().map(|m| m.id.as_str()).collect();
        let expected: BTreeSet<&str> = matrix.keys().map(String::as_str).collect();
        assert_eq!(
            ids, expected,
            "acceptance/{name} does not hold the matrix's rows; {REGENERATE}"
        );
        // Each row exactly `runs` times, as repeats 1 to `runs`.
        let mut repeats: BTreeMap<&str, Vec<u32>> = BTreeMap::new();
        for m in &committed {
            repeats.entry(m.id.as_str()).or_default().push(m.repeat);
        }
        let wanted: Vec<u32> = (1..=runs).collect();
        for (id, mut seen) in repeats {
            seen.sort_unstable();
            assert_eq!(
                seen, wanted,
                "acceptance/{name}: {id} should be run as repeats {wanted:?}; {REGENERATE}"
            );
        }
        // Every expectation a committed row records is the matrix's current one.
        for m in &committed {
            let expect = &matrix[&m.id].expect;
            let survivors: Vec<&str> = m.expected_survivors.iter().map(String::as_str).collect();
            let fields = [
                ("outcome", m.expected_outcome == expect.outcome),
                ("exit", m.expected_exit == expect.exit),
                ("cleanup verdict", m.expected_confirmed == expect.confirmed),
                ("survivors", survivors == expect.survivors),
                ("leak class", m.expected_leak == expect.leak),
                (
                    "timing dependence",
                    m.timing_dependent == expect.timing_dependent,
                ),
                ("reason text", m.expected_reason.as_deref() == expect.reason),
            ];
            for (field, same) in fields {
                assert!(
                    same,
                    "acceptance/{name}: {} expected {field} differs from src/matrix.rs; {REGENERATE}",
                    m.id
                );
            }
        }
    }
}
