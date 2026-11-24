//! RESULTS.md and COMPARISON.md, generated from `results.jsonl`.
//!
//! Nothing here measures anything. `--report` reads the committed results
//! (and, when present, `results.meta.json`, `repeats.jsonl` and
//! `spike-numbers.jsonl`) and writes both documents, so a fresh full run can
//! be turned into documents with one command. When `live.jsonl` is present
//! (a recorded `--live` run: its metadata, then its rows), RESULTS.md ends
//! with a "Live suite" section made from it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use serde::Deserialize;

use crate::live::LiveRow;
use crate::matrix::{self, LeakClass};
use crate::runner::Measured;

/// The spike commit the comparison numbers come from.
pub const SPIKE_COMMIT: &str = "7f4da23";

fn read_rows<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(i, l)| {
            serde_json::from_str(l).map_err(|e| format!("{}:{}: {e}", path.display(), i + 1))
        })
        .collect()
}

/// Rewrite `RESULTS.md` and `COMPARISON.md` in `dir` from `results`.
pub fn regenerate(dir: &Path, results: &Path) -> Result<(), String> {
    let rows: Vec<Measured> = read_rows(results)?;
    if rows.is_empty() {
        return Err(format!("{} has no rows", results.display()));
    }
    let meta: Option<serde_json::Value> = std::fs::read_to_string(dir.join("results.meta.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok());
    let repeats: Vec<Measured> = read_rows(&dir.join("repeats.jsonl")).unwrap_or_default();
    let spike: Vec<SpikeRow> = read_rows(&dir.join("spike-numbers.jsonl")).unwrap_or_default();
    let live_path = dir.join("live.jsonl");
    // Present in any form, a dangling link included: anything but a regular
    // file is refused by `read_regular_file`, not taken as absent.
    let live = if std::fs::symlink_metadata(&live_path).is_ok() {
        let text = crate::live::read_regular_file(&live_path, 16 * 1024 * 1024)?;
        Some(crate::live::read_live(&text).map_err(|e| format!("{}: {e}", live_path.display()))?)
    } else {
        None
    };
    let mut results = results_md(&rows, meta.as_ref(), &repeats);
    results.push_str(&match &live {
        Some((meta, rows)) => live_section(rows, Some(meta)),
        None => live_section(&[], None),
    });
    std::fs::write(dir.join("RESULTS.md"), results)
        .map_err(|e| format!("write RESULTS.md: {e}"))?;
    std::fs::write(dir.join("COMPARISON.md"), comparison_md(&rows, &spike))
        .map_err(|e| format!("write COMPARISON.md: {e}"))?;
    Ok(())
}

/// Nearest-rank percentile of a sorted list.
pub fn percentile(sorted: &[u64], p: u32) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let len = u64::try_from(sorted.len()).unwrap_or(u64::MAX);
    let rank = (u64::from(p) * len).div_ceil(100);
    let index = usize::try_from(rank.clamp(1, len) - 1).unwrap_or(0);
    sorted.get(index).copied()
}

fn label(v: &[String]) -> String {
    if v.is_empty() {
        "none".to_string()
    } else {
        v.join(",")
    }
}

fn yes(b: Option<bool>) -> &'static str {
    match b {
        Some(true) => "yes",
        Some(false) => "NO",
        None => "n/a",
    }
}

fn exit_label(m: &Measured) -> String {
    match (m.exit_code, m.exit_signal) {
        (Some(c), _) => c.to_string(),
        (None, Some(s)) => format!("signal {s}"),
        _ => "none".to_string(),
    }
}

/// Cases with no document to read: the harness closed stdout or killed delega.
fn unreadable(m: &Measured) -> bool {
    matches!(m.case.as_str(), "dc" | "dca" | "k" | "kk")
}

/// Whether delega's standard output held one valid result, for rows where one
/// is expected.
fn stated(m: &Measured) -> Option<bool> {
    (!unreadable(m)).then(|| m.stdout_docs == Some(1) && m.result_valid == Some(true) && !m.hang)
}

fn outcome_exit_ok(m: &Measured) -> bool {
    let exit = m
        .exit_code
        .map(matrix::Exit::Code)
        .or(m.exit_signal.map(matrix::Exit::Signal));
    let outcome_ok = unreadable(m) || m.outcome.as_deref() == Some(m.expected_outcome.as_str());
    outcome_ok && exit == Some(m.expected_exit) && !m.hang
}

#[derive(Default)]
struct Counts {
    rows: usize,
    outcome_exit: usize,
    cleanup_ok: usize,
    stated: (usize, usize),
    clean: usize,
    honest: (usize, usize),
    pass: usize,
    non_gap_pass: usize,
    gap_shown: usize,
    gap_not_shown: usize,
    skipped: usize,
    fail: usize,
}

fn counts<'a>(rows: impl Iterator<Item = &'a Measured>) -> Counts {
    let mut c = Counts::default();
    for m in rows {
        c.rows += 1;
        c.outcome_exit += usize::from(outcome_exit_ok(m));
        c.cleanup_ok += usize::from(m.cleanup_confirmed == m.expected_confirmed);
        if let Some(s) = stated(m) {
            c.stated.1 += 1;
            c.stated.0 += usize::from(s);
        }
        c.clean += usize::from(m.survivors.is_empty());
        if let Some(h) = m.honest {
            c.honest.1 += 1;
            c.honest.0 += usize::from(h);
        }
        c.gap_shown += usize::from(m.gap_shown == Some(true));
        c.gap_not_shown += usize::from(m.gap_shown == Some(false));
        match m.verdict.as_str() {
            "pass" => {
                c.pass += 1;
                c.non_gap_pass += usize::from(m.expected_leak != LeakClass::DocumentedGap);
            }
            "skipped" => c.skipped += 1,
            _ => c.fail += 1,
        }
    }
    c
}

fn counts_row(name: &str, c: &Counts) -> String {
    format!(
        "| {name} | {} | {} | {} | {} of {} | {} | {} of {} | {} | {} | {} | {} | {} | {} |\n",
        c.rows,
        c.outcome_exit,
        c.cleanup_ok,
        c.stated.0,
        c.stated.1,
        c.clean,
        c.honest.0,
        c.honest.1,
        c.pass,
        c.non_gap_pass,
        c.gap_shown,
        c.gap_not_shown,
        c.skipped,
        c.fail,
    )
}

const COUNT_HEAD: &str = "| Group | Rows | Expected outcome and exit | Cleanup verdict as expected | One stated outcome | Nothing left | Cleanup consistency | Pass | Non-gap pass | Gap shown | Gap not shown | Skipped | Fail |\n| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |\n";

fn ms_label(v: Option<u64>) -> String {
    v.map_or("-".to_string(), |x| format!("{x} ms"))
}

fn latency_table(rows: &[Measured]) -> String {
    let mut out = String::from(
        "| Measure | Rows | p50 | p90 | p99 | max |\n| --- | --- | --- | --- | --- | --- |\n",
    );
    let ignoring = |m: &Measured| matches!(m.variant.as_str(), "ignore" | "stubborn");
    let groups: [(&str, &str, bool); 6] = [
        ("signal to exit", "signal to exit", false),
        (
            "signal to exit",
            "signal to exit, a process ignores TERM",
            true,
        ),
        ("timeout overshoot", "timeout overshoot", false),
        (
            "timeout overshoot",
            "timeout overshoot, a process ignores TERM",
            true,
        ),
        ("run to exit", "run to exit", false),
        ("run to exit", "run to exit, a process ignores TERM", true),
    ];
    for (kind, name, ign) in groups {
        let mut v: Vec<u64> = rows
            .iter()
            .filter(|m| m.latency_kind == kind && ignoring(m) == ign && m.case != "k")
            .filter_map(|m| m.latency_ms)
            .collect();
        v.sort_unstable();
        let _ = writeln!(
            out,
            "| {name} | {} | {} | {} | {} | {} |",
            v.len(),
            ms_label(percentile(&v, 50)),
            ms_label(percentile(&v, 90)),
            ms_label(percentile(&v, 99)),
            ms_label(v.last().copied()),
        );
    }
    out
}

fn action_group(m: &Measured) -> &'static str {
    match m.case.as_str() {
        "a" | "b" | "e" | "ed" | "i" | "id" => "provider ends by itself",
        "c" | "fd" | "hd" | "fl" => "time limit (--timeout)",
        "d" | "fli" => "SIGINT",
        "dt" => "SIGTERM",
        "dh" => "SIGHUP",
        "dc" | "dca" => "stdout closed early",
        "k" => "SIGKILL of delega",
        "kk" => "SIGKILL of delega and its keeper",
        _ => "other",
    }
}

fn results_md(rows: &[Measured], meta: Option<&serde_json::Value>, repeats: &[Measured]) -> String {
    let mut out = String::new();
    out.push_str("# Results: does delega meet criteria 1 and 2?\n\n");
    out.push_str(
        "Generated from `results.jsonl` by `cargo run -p delega-acceptance -- --report`; do not edit by hand.\n\
         The harness is described in [README.md](README.md). Criteria, from the repository README:\n\
         (1) every run ends in exactly one stated outcome, never a hang and never silence; (2) after any\n\
         outcome the provider and its descendants are gone, or the result says cleanup could not be\n\
         confirmed and what remains.\n\n",
    );

    out.push_str("## Environment\n\n");
    if let Some(meta) = meta {
        let text = |v: &serde_json::Value| v.as_str().unwrap_or("?").to_string();
        let _ = writeln!(out, "- Machine: {}.", text(&meta["os"]));
        if let Some(hidden) = meta["system_programs_hide_environment"].as_bool() {
            let _ = writeln!(
                out,
                "- The system hides the environment of its own programs from `ps -E`: {}. On \
                 macOS the orphan rows are listed as gap cases when it does and ordinary cases when it \
                 does not (process module docs, \"What remains possible\").",
                if hidden { "yes" } else { "no" }
            );
        }
        let _ = writeln!(
            out,
            "- delega: {}.",
            meta["delega_version"]["version"].as_str().map_or_else(
                || "version not recorded".to_string(),
                |v| format!("version {v}")
            ),
        );
        let _ = writeln!(
            out,
            "- Harness commit: `{}`.",
            text(&meta["harness_commit"])
        );
        let _ = writeln!(
            out,
            "- {} rows, {} at a time, finished in {} s.",
            meta["rows"], meta["jobs"], meta["wall_secs"]
        );
    }
    out.push_str(
        "- Fake providers only, a temporary `HOME` and a cleared environment per case; no real `claude` or `codex`.\n\
         - The harness samples for survivors at exit and after settling, using the process table, PID ledger, and relay heartbeat. It then kills any remaining processes.\n\
         - Cases run in parallel, so differences under about 100 ms mean nothing.\n\n",
    );

    out.push_str("## Summary\n\n");
    out.push_str(
        "- **Expected outcome and exit**: the outcome and the exit status the expectations table gives.\n\
         - **Cleanup verdict as expected**: `cleanup.confirmed` matches the fixture's reference value. The reference outcome, exit and cleanup verdict are recorded for comparison.\n\
         - **One stated outcome**: standard output held exactly one JSON document that parses and validates\n\
           as a `RunResult`, and `delega` exited without the harness having to kill it. Rows where the harness\n\
           closed standard output or killed `delega` have no document to read and are not counted.\n\
         - **Nothing left**: the harness observed no survivors after settling.\n\
         - **Cleanup consistency**: a Confirmed result had no observed survivor at exit or after settling. Counted over rows with a result outside the documented residuals.\n\
         - **Non-gap pass**: passing rows outside the documented residuals. All rows are checked for the guarantees that apply to them.\n\
         - **Gap shown** and **Gap not shown**: observations of the listed survivor roles after settling, independent of the verdict.\n\n",
    );
    out.push_str(COUNT_HEAD);
    out.push_str(&counts_row("all", &counts(rows.iter())));
    for p in matrix::PROVIDERS {
        out.push_str(&counts_row(
            p,
            &counts(rows.iter().filter(|m| m.provider == p)),
        ));
    }
    out.push('\n');

    out.push_str("### By caller action\n\n");
    out.push_str(COUNT_HEAD);
    let mut actions: Vec<(&str, Vec<&Measured>)> = Vec::new();
    for m in rows {
        let name = action_group(m);
        match actions.iter_mut().find(|(n, _)| *n == name) {
            Some((_, v)) => v.push(m),
            None => actions.push((name, vec![m])),
        }
    }
    for (name, v) in &actions {
        out.push_str(&counts_row(name, &counts(v.iter().copied())));
    }
    out.push('\n');

    out.push_str("### By variant (what the provider leaves running)\n\n");
    out.push_str(COUNT_HEAD);
    let mut variants: BTreeMap<&str, Vec<&Measured>> = BTreeMap::new();
    for m in rows {
        variants.entry(m.variant.as_str()).or_default().push(m);
    }
    for v in [
        "plain",
        "child",
        "pipe",
        "ignore",
        "stubborn",
        "setsid",
        "orphan",
        "orphan-late",
        "relay",
    ] {
        if let Some(list) = variants.get(v) {
            out.push_str(&counts_row(v, &counts(list.iter().copied())));
        }
    }
    out.push('\n');

    out.push_str("## Latency\n\n");
    out.push_str(
        "Measured from the harness's action to `delega` exiting (a signal), past the `--timeout` (a time\n\
         limit that fired), or from start to exit (a run left to finish). A process that ignores TERM adds the\n\
         grace period before SIGKILL. SIGKILL rows are left out.\n\n",
    );
    out.push_str(&latency_table(rows));
    out.push('\n');

    let bad: Vec<&Measured> = rows
        .iter()
        .filter(|m| matches!(m.verdict.as_str(), "fail" | "skipped"))
        .collect();
    out.push_str("## Failures and skips\n\n");
    if bad.is_empty() {
        out.push_str("None.\n\n");
    } else {
        out.push_str("| Row | Verdict | Problems |\n| --- | --- | --- |\n");
        for m in bad {
            let _ = writeln!(
                out,
                "| {} | {} | {} |",
                m.id,
                m.verdict,
                m.problems.join("; ")
            );
        }
        out.push('\n');
    }

    out.push_str("## Gap observations\n\n");
    out.push_str(
        "The matrix's `GAPS` table classifies documented residuals for the recorded platform. A gap is shown when all its listed survivor roles are observed after settling. These rows retain the applicable result and termination checks. The presence or absence of survivors is informational.\n\n",
    );
    let mut groups: BTreeMap<&str, Vec<&Measured>> = BTreeMap::new();
    for m in rows
        .iter()
        .filter(|m| m.expected_leak == LeakClass::DocumentedGap)
    {
        groups
            .entry(m.expected_reason.as_deref().unwrap_or_default())
            .or_default()
            .push(m);
    }
    for (reason, list) in &groups {
        let seen = list.iter().filter(|m| m.gap_shown == Some(true)).count();
        let _ = writeln!(
            out,
            "### Documented gap: {} rows, {seen} shown in this run\n\n{reason}\n",
            list.len(),
        );
        out.push_str("| Row | Outcome | Exit | Cleanup confirmed | Survivors at exit | Survivors after settling | Gap observation | Verdict |\n| --- | --- | --- | --- | --- | --- | --- | --- |\n");
        for m in list {
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} | {} | {} |",
                m.id,
                m.outcome.as_deref().unwrap_or("none"),
                exit_label(m),
                yes(m.cleanup_confirmed),
                label(&m.survivors_at_exit),
                label(&m.survivors),
                match m.gap_shown {
                    Some(true) => "gap shown",
                    Some(false) => "gap not shown",
                    None => "not measured",
                },
                m.verdict,
            );
        }
        out.push('\n');
    }

    out.push_str("## Rows with process listing disabled\n\n");
    out.push_str(
        "These rows disable process listing through the test seam. The table records any cleanup reason and the processes the harness observed.\n\n",
    );
    out.push_str("| Row | Outcome | Confirmed | Process groups | Pids | Left running | Reason |\n| --- | --- | --- | --- | --- | --- | --- |\n");
    for m in rows.iter().filter(|m| m.expected_confirmed == Some(false)) {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {:?} | {:?} | {} | {} |",
            m.id,
            m.outcome.as_deref().unwrap_or("none"),
            yes(m.cleanup_confirmed),
            m.cleanup_groups,
            m.cleanup_pids,
            label(&m.survivors),
            m.cleanup_reason
                .as_deref()
                .unwrap_or("")
                .chars()
                .take(70)
                .collect::<String>(),
        );
    }
    out.push('\n');

    out.push_str(TIME_BREAKDOWN);

    if !repeats.is_empty() {
        out.push_str(&variation(repeats));
    }

    out.push_str("## All rows\n\n");
    out.push_str("| Row | Action | Outcome | Exit | One document | Cleanup confirmed | Left running | Cleanup consistency | Latency | Verdict |\n| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |\n");
    for m in rows {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            m.id,
            m.action,
            m.outcome.as_deref().unwrap_or("none"),
            exit_label(m),
            yes(stated(m)),
            yes(m.cleanup_confirmed),
            label(&m.survivors),
            yes(m.honest),
            ms_label(m.latency_ms),
            m.verdict,
        );
    }
    out
}

/// The "Live suite" section: the recorded `--live` run, or a line saying
/// there is none.
pub fn live_section(rows: &[LiveRow], meta: Option<&serde_json::Value>) -> String {
    let mut out = String::from("\n## Live suite\n\n");
    out.push_str(
        "Acceptance criterion 6 asks for a small live suite per provider. `--live` runs it against the\n\
         installed `claude` and `codex` (README.md, \"Live suite\"); this section is generated from the\n\
         recorded run in `live.jsonl`, whose first line is the run's metadata.\n\n",
    );
    if rows.is_empty() {
        out.push_str("No live run is recorded.\n");
        return out;
    }
    if let Some(meta) = meta {
        let text = |v: &serde_json::Value| v.as_str().unwrap_or("?").to_string();
        let _ = writeln!(out, "- Date: {}.", text(&meta["date"]));
        let _ = writeln!(out, "- Machine: {}.", text(&meta["os"]));
        let _ = writeln!(out, "- delega: version {}.", text(&meta["delega_version"]));
        let _ = writeln!(
            out,
            "- Harness commit: `{}`.",
            text(&meta["harness_commit"])
        );
        for provider in crate::live::PROVIDERS {
            if rows.iter().any(|r| r.provider == provider) {
                let _ = writeln!(
                    out,
                    "- {provider}: {}.",
                    crate::live::version_label(provider, rows)
                );
            }
        }
        let _ = writeln!(
            out,
            "- Claude cases used `--model {}`, Codex cases `--effort {}`.",
            text(&meta["claude_model"]),
            text(&meta["codex_effort"])
        );
        out.push('\n');
    }
    let count = |v: &str| rows.iter().filter(|r| r.verdict == v).count();
    let _ = writeln!(
        out,
        "{} cases: {} pass, {} fail, {} not shown.\n",
        rows.len(),
        count("pass"),
        count("fail"),
        count("not_shown")
    );
    out.push_str("| Provider | Case | Version | Verdict | Outcome | Cleanup confirmed | Denials | Runs | Reported cost | Problems | Not shown and notes |\n| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |\n");
    let cell = |items: Vec<String>| {
        if items.is_empty() {
            "none".to_string()
        } else {
            items.join("; ").replace('|', "/")
        }
    };
    for r in rows {
        let notes: Vec<String> = r
            .not_shown
            .iter()
            .map(|n| format!("not shown: {n}"))
            .chain(r.notes.iter().cloned())
            .collect();
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            r.provider,
            r.case,
            r.provider_version.as_deref().unwrap_or("-"),
            r.verdict,
            r.outcome.as_deref().unwrap_or("-"),
            yes(r.cleanup_confirmed),
            r.denied,
            r.runs,
            r.cost_usd.map_or("-".to_string(), |c| format!("${c:.4}")),
            cell(r.problems.clone()),
            cell(notes),
        );
    }
    out
}

/// Written by the reviewer of the harness, not measured by it.
const TIME_BREAKDOWN: &str = "## Where the time goes in a run\n\n\
    Measured by the reviewer on a release build, not by this harness. On an idle machine a plain Claude\n\
    run takes about 250 to 310 ms and a plain Codex run about 385 to 440 ms. Roughly 80 percent of that\n\
    is process listings: 8 for Claude and 12 for Codex, 24 to 33 ms each, four per child process run\n\
    (the version probe counts as one, and Codex runs a second probe). Under 8 parallel jobs the listings\n\
    slow down, which accounts for the 264 to 680 ms seen for a normal completion here.\n\n";

fn variation(repeats: &[Measured]) -> String {
    let mut out = String::from("## Variation across repeated runs\n\n");
    let runs: Vec<u32> = repeats
        .iter()
        .map(|m| m.repeat)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let _ = writeln!(
        out,
        "`repeats.jsonl` holds {} full runs of the whole matrix ({} rows each).\n",
        runs.len(),
        repeats.len() / runs.len().max(1)
    );
    out.push_str("| Run | Pass | Non-gap pass | Gap shown | Gap not shown | Skipped | Fail | p99 latency (not SIGKILL) | Max latency |\n| --- | --- | --- | --- | --- | --- | --- | --- | --- |\n");
    for r in &runs {
        let c = counts(repeats.iter().filter(|m| m.repeat == *r));
        let mut lat: Vec<u64> = repeats
            .iter()
            .filter(|m| m.repeat == *r && m.case != "k")
            .filter_map(|m| m.latency_ms)
            .collect();
        lat.sort_unstable();
        let _ = writeln!(
            out,
            "| {r} | {} | {} | {} | {} | {} | {} | {} | {} |",
            c.pass,
            c.non_gap_pass,
            c.gap_shown,
            c.gap_not_shown,
            c.skipped,
            c.fail,
            ms_label(percentile(&lat, 99)),
            ms_label(lat.last().copied()),
        );
    }
    out.push('\n');
    let mut by_id: BTreeMap<&str, Vec<&Measured>> = BTreeMap::new();
    for m in repeats {
        by_id.entry(m.id.as_str()).or_default().push(m);
    }
    let key = |m: &Measured| {
        (
            m.verdict.clone(),
            m.outcome.clone(),
            exit_label(m),
            m.survivors.clone(),
            m.cleanup_confirmed,
            m.gap_shown,
        )
    };
    let differing: Vec<(&str, &Vec<&Measured>)> = by_id
        .iter()
        .filter(|(_, v)| v.iter().any(|m| key(m) != key(v[0])))
        .map(|(k, v)| (*k, v))
        .collect();
    if differing.is_empty() {
        out.push_str("No row changed verdict, outcome, exit status, survivors or cleanup report between runs.\n\n");
    } else {
        out.push_str("Rows whose result changed between runs:\n\n| Row | Per run: verdict, outcome, survivors |\n| --- | --- |\n");
        for (id, list) in differing {
            let cells: Vec<String> = list
                .iter()
                .map(|m| {
                    format!(
                        "#{} {}, {}, {}",
                        m.repeat,
                        m.verdict,
                        m.outcome.as_deref().unwrap_or("none"),
                        label(&m.survivors)
                    )
                })
                .collect();
            let _ = writeln!(out, "| {id} | {} |", cells.join("; "));
        }
        out.push('\n');
    }
    out
}

/// One row of `spike-numbers.jsonl`.
#[derive(Debug, Clone, Deserialize)]
pub struct SpikeRow {
    pub id: String,
    pub side: String,
    pub status: Option<String>,
    pub ms: Option<u64>,
    pub timeout: bool,
    pub survivors: String,
    pub only_after_close: bool,
}

const STATED: [&str; 7] = [
    "Succeeded",
    "Failed",
    "TimedOut",
    "Cancelled",
    "completed",
    "failed",
    "interrupted",
];

impl SpikeRow {
    fn stated(&self) -> bool {
        self.status.as_deref().is_some_and(|s| STATED.contains(&s))
    }
    fn clean(&self) -> bool {
        self.survivors == "none"
    }
    fn cell(&self) -> String {
        let status = self.status.as_deref().unwrap_or("none");
        let status = if self.timeout {
            "failed (timeout)"
        } else {
            status
        };
        let star = if self.only_after_close { "*" } else { "" };
        let left = if self.clean() {
            format!("none{star}")
        } else {
            self.survivors.clone()
        };
        let state = if self.stated() { "" } else { "NO RESULT " };
        format!(
            "{state}{status} {} ms; left {left}",
            self.ms.map_or("-".to_string(), |m| m.to_string())
        )
    }
}

fn delega_cell(m: &Measured) -> String {
    format!(
        "{} {} ms; left {}",
        m.outcome.as_deref().unwrap_or("none"),
        m.elapsed_ms.unwrap_or(0),
        label(&m.survivors),
    )
}

const SIDES: [(&str, &str); 3] = [
    ("counterpart", "counterpart"),
    ("due-oneshot", "due one-shot"),
    ("due-resident", "due resident"),
];

fn by_side<'a>(spike: &'a [SpikeRow], side: &str) -> BTreeMap<&'a str, &'a SpikeRow> {
    spike
        .iter()
        .filter(|s| s.side == side)
        .map(|s| (s.id.as_str(), s))
        .collect()
}

fn comparison_md(rows: &[Measured], spike: &[SpikeRow]) -> String {
    let mut out = String::new();
    out.push_str("# Comparison: delega against the spike's counterpart and due numbers\n\n");
    let _ = writeln!(
        out,
        "Generated from `results.jsonl` and `spike-numbers.jsonl` by `cargo run -p delega-acceptance -- --report`;\n\
         do not edit by hand. The spike numbers are copied from the committed `results.jsonl` files of the outcome\n\
         spike (`delega-dot-spike-outcomes`, commit `{SPIKE_COMMIT}`): counterpart at its commit `8fa4645` and due at its\n\
         commit `8d56df2`. Only the status, the time to the result and the survivors are copied.\n"
    );
    out.push_str(
        "## What is and is not comparable\n\n\
         - The spike drove two libraries from a harness process (counterpart's supervisor, due's engine). This\n\
           harness drives the `delega` executable: a caller action is an operating system signal or a closed pipe,\n\
           and the result is the JSON on standard output and the exit status.\n\
         - Times are from start to exit in milliseconds, as the spike's `elapsed_ms` is from the start of the call.\n\
           A `delega` run also pays process start and one provider probe for Claude and two for Codex.\n\
         - Survivors: the spike measured 5 s after the result (after close and shutdown for resident workers and\n\
           counterpart; `*` marks a process that was alive 5 s after the result and removed only by close). This\n\
           harness measures 1 s after `delega` exits, because `delega` waits for cleanup before it exits.\n\
         - The cases are not the spike's one for one. Rows are compared where the provider behaviour, the variant and\n\
           the caller action carry over: cases `a`, `b`, `c`, `d`, `e`, `ed`, `fd`, `hd`, `i`, `id`. The spike's `d` is\n\
           a library `interrupt` at 1 s; here it is SIGINT to `delega` at 1 s.\n\
         - The fakes differ: these are the stream-accurate fakes `delega` already tests against, extended with the\n\
           spike's process behaviours. The descendant variants are ported; the provider scripts are not the spike's\n\
           byte for byte.\n\
         - The delega run is on a release build with 8 cases at a time; the spike ran 12 at a time.\n\n",
    );

    out.push_str("## Summary on the shared rows\n\n");
    out.push_str(
        "For each side, the rows that both this harness and that side ran. C1: a stated outcome came back (a\n\
         valid result on standard output for delega; completed, failed, timed out, cancelled or interrupted for the\n\
         spike). C2: nothing of the case alive at the measurement. A stated outcome says nothing about whether it\n\
         was the right one.\n\n",
    );
    out.push_str(
        "| Side | Shared rows | C1 met | C2 met | Both met |\n| --- | --- | --- | --- | --- |\n",
    );
    for (side, name) in SIDES {
        let map = by_side(spike, side);
        let shared: Vec<&Measured> = rows
            .iter()
            .filter(|m| map.contains_key(m.id.as_str()))
            .collect();
        let n = shared.len();
        let c1 = shared
            .iter()
            .filter(|m| map[m.id.as_str()].stated())
            .count();
        let c2 = shared.iter().filter(|m| map[m.id.as_str()].clean()).count();
        let both = shared
            .iter()
            .filter(|m| map[m.id.as_str()].stated() && map[m.id.as_str()].clean())
            .count();
        let _ = writeln!(out, "| {name} | {n} | {c1} | {c2} | {both} |");
        let d1 = shared.iter().filter(|m| stated(m) == Some(true)).count();
        let d2 = shared.iter().filter(|m| m.survivors.is_empty()).count();
        let db = shared
            .iter()
            .filter(|m| stated(m) == Some(true) && m.survivors.is_empty())
            .count();
        let _ = writeln!(out, "| delega, those {n} rows | {n} | {d1} | {d2} | {db} |");
    }
    out.push('\n');

    out.push_str("## Timing on the common rows\n\n");
    out.push_str("Time to the result in milliseconds, start to exit. `c` is a 2 s time limit, `d` an interrupt at 1 s, `ignore` a child that ignores TERM and INT, `stubborn` a provider that does.\n\n");
    out.push_str("| Row | delega | counterpart | due one-shot | due resident |\n| --- | --- | --- | --- | --- |\n");
    let maps: Vec<BTreeMap<&str, &SpikeRow>> =
        SIDES.iter().map(|(s, _)| by_side(spike, s)).collect();
    for prov in matrix::PROVIDERS {
        for key in [
            "a-plain",
            "c-plain",
            "c-ignore",
            "c-stubborn",
            "d-plain",
            "d-stubborn",
            "e-plain",
            "e-pipe",
        ] {
            let id = format!("{prov}-{key}");
            let Some(m) = rows.iter().find(|m| m.id == id) else {
                continue;
            };
            let cell = |i: usize| {
                maps[i]
                    .get(id.as_str())
                    .and_then(|s| s.ms)
                    .map_or("-".to_string(), |x| x.to_string())
            };
            let _ = writeln!(
                out,
                "| {id} | {} | {} | {} | {} |",
                m.elapsed_ms.unwrap_or(0),
                cell(0),
                cell(1),
                cell(2),
            );
        }
    }
    out.push('\n');

    out.push_str("## Row by row\n\n");
    out.push_str("Each cell is the outcome (or status), the milliseconds to the result, and what was left. `-` means the side did not run the row.\n\n");
    out.push_str("| Row | delega | counterpart | due one-shot | due resident |\n| --- | --- | --- | --- | --- |\n");
    let mut shown = 0;
    for m in rows {
        if !maps.iter().any(|map| map.contains_key(m.id.as_str())) {
            continue;
        }
        shown += 1;
        let cells: Vec<String> = maps
            .iter()
            .map(|map| map.get(m.id.as_str()).map_or("-".to_string(), |s| s.cell()))
            .collect();
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} |",
            m.id,
            delega_cell(m),
            cells[0],
            cells[1],
            cells[2]
        );
    }
    let _ = writeln!(out, "\n{shown} rows.\n");

    out.push_str("## Rows delega runs that the spike did not\n\n");
    let spike_ids: BTreeSet<&str> = spike.iter().map(|s| s.id.as_str()).collect();
    let mut by_case: BTreeMap<&str, usize> = BTreeMap::new();
    for m in rows.iter().filter(|m| !spike_ids.contains(m.id.as_str())) {
        *by_case.entry(m.case.as_str()).or_default() += 1;
    }
    let _ = writeln!(out, "{} rows. By case:\n", by_case.values().sum::<usize>());
    for (case, n) in &by_case {
        let desc = matrix::CASES
            .iter()
            .find(|c| c.id == *case)
            .map_or("", |c| c.desc);
        let _ = writeln!(out, "- `{case}` ({desc}): {n} rows.");
    }
    out.push_str("\nThe spike ran `a` only with the `plain` variant. The `orphan` and `orphan-late` variants were run on counterpart only, so due has no number for them. Cases `dt`, `dh`, `fl`, `fli`, `dc`, `dca` and `k` are new.\n\n");

    out.push_str("## Spike rows not carried over\n\n| Spike case | Why |\n| --- | --- |\n");
    for (case, why) in matrix::DROPPED {
        let _ = writeln!(out, "| {case} | {why} |");
    }
    out.push('\n');

    out.push_str("## Measured differences\n\n");
    out.push_str(&differences(rows, spike));
    out
}

fn differences(rows: &[Measured], spike: &[SpikeRow]) -> String {
    let mut out = String::new();
    let counterpart = by_side(spike, "counterpart");
    let shared: Vec<&Measured> = rows
        .iter()
        .filter(|m| counterpart.contains_key(m.id.as_str()))
        .collect();
    let both_left: Vec<&str> = shared
        .iter()
        .filter(|m| !m.survivors.is_empty() && !counterpart[m.id.as_str()].clean())
        .map(|m| m.id.as_str())
        .collect();
    let only_delega: Vec<&str> = shared
        .iter()
        .filter(|m| !m.survivors.is_empty() && counterpart[m.id.as_str()].clean())
        .map(|m| m.id.as_str())
        .collect();
    let only_cp: Vec<&str> = shared
        .iter()
        .filter(|m| m.survivors.is_empty() && !counterpart[m.id.as_str()].clean())
        .map(|m| m.id.as_str())
        .collect();
    let list = |v: &[&str]| {
        if v.is_empty() {
            "none".to_string()
        } else {
            v.iter()
                .map(|s| format!("`{s}`"))
                .collect::<Vec<_>>()
                .join(", ")
        }
    };
    let _ = writeln!(
        out,
        "- On the {} rows delega shares with counterpart: both left a survivor on {}; only delega on {}; only counterpart on {}.",
        shared.len(),
        list(&both_left),
        list(&only_delega),
        list(&only_cp),
    );
    let confirmed_with_survivors = rows
        .iter()
        .filter(|m| {
            m.cleanup_confirmed == Some(true)
                && (!m.survivors_at_exit.is_empty() || !m.survivors.is_empty())
        })
        .count();
    let _ = writeln!(
        out,
        "- Rows where delega's result said cleanup was confirmed while a process of the case was alive: {confirmed_with_survivors} (including documented residuals). The spike found no counterpart result that named an escaped descendant."
    );
    let timed_out = rows
        .iter()
        .filter(|m| m.outcome.as_deref() == Some("timed_out"))
        .count();
    let _ = writeln!(
        out,
        "- delega reports a time limit as its own outcome, `timed_out`, with exit status 124 ({timed_out} rows); counterpart reported `failed` with `turn_timeout`, due `TimedOut`."
    );
    out
}

#[cfg(test)]
mod tests {
    use super::percentile;

    #[test]
    fn the_live_section_says_when_no_run_is_recorded() {
        let text = super::live_section(&[], None);
        assert!(text.contains("## Live suite"), "{text}");
        assert!(text.contains("No live run is recorded."), "{text}");
    }

    #[test]
    fn the_live_section_lists_each_case_with_versions_date_and_notes() {
        let (meta, rows) =
            crate::live::read_live(include_str!("../tests/fixtures/live/live.jsonl"))
                .expect("the sample live results");
        let text = super::live_section(&rows, Some(&meta));
        assert!(text.contains("- Date: 2026-11-22."), "{text}");
        assert!(text.contains("- claude: claude 2.1.400."), "{text}");
        assert!(text.contains("- codex: codex 0.99.0."), "{text}");
        assert!(
            text.contains("3 cases: 1 pass, 1 fail, 1 not shown."),
            "{text}"
        );
        assert!(
            text.contains(
                "| claude | plain-answer | claude 2.1.400 | pass | completed | yes | 0 | 1 | $0.0010 | none | none |"
            ),
            "{text}"
        );
        assert!(text.contains("not so: cleanup confirmed"), "{text}");
        assert!(
            text.contains("not shown: that the worker's request failed: the harness itself could not reach https://example.com; the temporary file was in /tmp"),
            "{text}"
        );
    }

    #[test]
    fn regenerate_refuses_live_results_whose_metadata_does_not_match() {
        let dir = tempfile::tempdir().expect("scratch");
        let fixture = include_str!("../tests/fixtures/live/live.jsonl");
        let broken = fixture.replacen("\"cases\":3", "\"cases\":4", 1);
        assert_ne!(broken, fixture);
        std::fs::write(dir.path().join("live.jsonl"), broken).expect("write");
        let results = concat!(env!("CARGO_MANIFEST_DIR"), "/results.jsonl");
        let error =
            super::regenerate(dir.path(), std::path::Path::new(results)).expect_err("refused");
        assert!(error.contains("cases"), "{error}");
    }

    #[test]
    fn regenerate_refuses_a_live_results_path_that_is_a_link_dangling_or_not() {
        let results = concat!(env!("CARGO_MANIFEST_DIR"), "/results.jsonl");
        for target in ["exists", "missing"] {
            let dir = tempfile::tempdir().expect("scratch");
            std::fs::write(
                dir.path().join("exists"),
                include_str!("../tests/fixtures/live/live.jsonl"),
            )
            .expect("write");
            std::os::unix::fs::symlink(dir.path().join(target), dir.path().join("live.jsonl"))
                .expect("link");
            let error =
                super::regenerate(dir.path(), std::path::Path::new(results)).expect_err("refused");
            assert!(
                error.ends_with("is not a regular file"),
                "{target}: {error}"
            );
        }
    }

    #[test]
    fn percentile_is_nearest_rank() {
        let v = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100];
        assert_eq!(percentile(&v, 50), Some(50));
        assert_eq!(percentile(&v, 90), Some(90));
        assert_eq!(percentile(&v, 99), Some(100));
        assert_eq!(percentile(&[], 50), None);
        assert_eq!(percentile(&[7], 99), Some(7));
    }
}
