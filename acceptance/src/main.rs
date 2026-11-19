//! `delega-acceptance`: run the acceptance matrix against the built `delega`,
//! or regenerate the documents from a results file. See `acceptance/README.md`.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};

use clap::Parser;
use delega_acceptance::delega_bin::{build, workspace_root};
use delega_acceptance::matrix::{self, Row};
use delega_acceptance::procs;
use delega_acceptance::report;
use delega_acceptance::run_all::{close_gaps, run_all};
use delega_acceptance::runner::{Context, GAP_CLOSED, GAP_NOT_SHOWN, MARKER_PREFIX, Measured};

#[derive(Parser)]
#[command(about = "Black-box acceptance measurement of the built delega binary")]
struct Args {
    /// Run the whole matrix.
    #[arg(long)]
    all: bool,
    /// Run the few rows `cargo test` runs.
    #[arg(long)]
    smoke: bool,
    /// Run only rows whose id contains this text (repeatable; any match runs).
    #[arg(long)]
    filter: Vec<String>,
    /// Print the row ids and exit.
    #[arg(long)]
    list: bool,
    /// Cases running at once.
    #[arg(long, default_value_t = 8)]
    jobs: usize,
    /// Run every row this many times.
    #[arg(long, default_value_t = 1)]
    repeat: u32,
    /// Write one JSON row per case here. Defaults to acceptance/results.jsonl
    /// for `--all`, and to nothing otherwise.
    #[arg(long)]
    out: Option<PathBuf>,
    /// The delega binary. Default: build the package and use that.
    #[arg(long)]
    delega: Option<PathBuf>,
    /// Regenerate RESULTS.md and COMPARISON.md from a results file instead of
    /// running anything.
    #[arg(long)]
    report: bool,
    /// The results file for `--report`.
    #[arg(long)]
    results: Option<PathBuf>,
    /// Kill every process that carries a marker of any harness run, then exit.
    /// For use after a harness run was killed.
    #[arg(long)]
    sweep: bool,
    /// Do not fail the run when a row is skipped (for example when `perl` is
    /// missing). A skipped row fails the run by default.
    #[arg(long)]
    allow_skips: bool,
    /// Do not print a line per finished case.
    #[arg(long)]
    quiet: bool,
}

fn acceptance_dir() -> PathBuf {
    workspace_root().join("acceptance")
}

fn main() -> ExitCode {
    let args = Args::parse();
    if args.sweep {
        return sweep_all();
    }
    if args.report {
        let results = args
            .results
            .unwrap_or_else(|| acceptance_dir().join("results.jsonl"));
        return match report::regenerate(&acceptance_dir(), &results) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("delega-acceptance: {err}");
                ExitCode::FAILURE
            }
        };
    }

    let mut rows: Vec<Row> = if args.all {
        matrix::all()
    } else if args.smoke {
        matrix::smoke()
    } else if !args.filter.is_empty() || args.list {
        matrix::all()
    } else {
        eprintln!("delega-acceptance: give --all, --smoke or --filter TEXT (see --help)");
        return ExitCode::from(2);
    };
    if !args.filter.is_empty() {
        rows.retain(|r| args.filter.iter().any(|f| r.id.contains(f.as_str())));
    }
    if args.list {
        for row in &rows {
            println!("{}", row.id);
        }
        return ExitCode::SUCCESS;
    }
    if rows.is_empty() {
        eprintln!("delega-acceptance: no row matches");
        return ExitCode::from(2);
    }

    let delega = match args.delega.clone().map_or_else(build, Ok) {
        Ok(path) => path,
        Err(err) => {
            eprintln!("delega-acceptance: {err}");
            return ExitCode::FAILURE;
        }
    };
    let base = match std::fs::canonicalize(std::env::temp_dir()) {
        Ok(p) => p,
        Err(err) => {
            eprintln!("delega-acceptance: temporary directory: {err}");
            return ExitCode::FAILURE;
        }
    };
    let ctx = Context::new(delega.clone(), base);
    eprintln!(
        "running {} rows x {} on {} jobs against {}",
        rows.len(),
        args.repeat,
        args.jobs,
        delega.display()
    );
    let began = std::time::Instant::now();
    let repeat = args.repeat.max(1);
    let mut measured = run_all(&ctx, &rows, args.jobs, repeat, args.quiet);
    close_gaps(&mut measured, repeat);
    let wall = began.elapsed();

    let out = args
        .out
        .clone()
        .or_else(|| args.all.then(|| acceptance_dir().join("results.jsonl")));
    if let Some(path) = &out {
        if let Err(err) = write_results(path, &measured) {
            eprintln!("delega-acceptance: write {}: {err}", path.display());
            return ExitCode::FAILURE;
        }
        if args.all {
            let meta = meta(&delega, args.jobs, args.repeat, rows.len(), wall);
            let _ = std::fs::write(
                path.with_extension("meta.json"),
                serde_json::to_string_pretty(&meta).unwrap_or_default() + "\n",
            );
        }
    }

    // Whatever the cases did, nothing from this run may be left.
    let marker = ctx.run_marker();
    let mut left: Vec<_> = procs::ps()
        .into_iter()
        .filter(|p| !p.stat.starts_with('Z') && p.command.contains(&marker))
        .collect();
    // Processes with no marker in their command line: the pids the ledgers named.
    let bare = ctx.bare.lock().map(|b| b.clone()).unwrap_or_default();
    left.extend(procs::live_bare(&bare));
    if !left.is_empty() {
        for p in &left {
            procs::signal(p.pid, rustix::process::Signal::KILL);
        }
    }

    summarize(&measured, wall, left.len(), args.allow_skips)
}

fn write_results(path: &PathBuf, rows: &[Measured]) -> std::io::Result<()> {
    let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
    for row in rows {
        let line = serde_json::to_string(row).map_err(std::io::Error::other)?;
        writeln!(file, "{line}")?;
    }
    file.flush()
}

fn meta(
    delega: &PathBuf,
    jobs: usize,
    repeat: u32,
    rows: usize,
    wall: std::time::Duration,
) -> serde_json::Value {
    let run = |program: &str, args: &[&str]| -> String {
        Command::new(program)
            .args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };
    let version = Command::new(delega)
        .args(["version", "--output", "json"])
        .env_clear()
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice::<serde_json::Value>(&o.stdout).ok())
        .unwrap_or(serde_json::Value::Null);
    serde_json::json!({
        "os": run("uname", &["-srm"]),
        "jobs": jobs,
        "repeat": repeat,
        "rows": rows,
        "wall_secs": wall.as_secs(),
        "delega_version": version,
        "harness_commit": run("git", &["-C", &workspace_root().to_string_lossy(), "rev-parse", "--short", "HEAD"]),
    })
}

fn summarize(
    measured: &[Measured],
    wall: std::time::Duration,
    left: usize,
    allow_skips: bool,
) -> ExitCode {
    let count = |v: &str| measured.iter().filter(|m| m.verdict == v).count();
    println!(
        "{} cases in {:.0} s: {} pass, {} expected failures, {} gap not shown, {} gap closed, {} unexpected passes, {} fail, {} skipped",
        measured.len(),
        wall.as_secs_f64(),
        count("pass"),
        count("expected_failure"),
        count(GAP_NOT_SHOWN),
        count(GAP_CLOSED),
        count("unexpected_pass"),
        count("fail"),
        count("skipped"),
    );
    let not_shown: Vec<&Measured> = measured
        .iter()
        .filter(|m| m.verdict == GAP_NOT_SHOWN)
        .collect();
    let known_gap_runs = measured
        .iter()
        .filter(|m| {
            matches!(
                m.verdict.as_str(),
                "expected_failure" | GAP_NOT_SHOWN | GAP_CLOSED
            )
        })
        .count();
    println!(
        "{} of {} known-gap runs did not show their gap (not a failure, unless every timing-dependent gap run of this run shows none, or a row shows none in every run of --repeat 3 or more: that is gap_closed and fails)",
        not_shown.len(),
        known_gap_runs
    );
    for m in &not_shown {
        println!("  {} #{}: gap not shown", m.id, m.repeat);
    }
    let retried = measured.iter().filter(|m| m.attempts > 1).count();
    let attempts: u32 = measured.iter().map(|m| m.attempts).sum();
    println!(
        "{retried} rows needed a retry after a late process listing; {attempts} runs in total for {} rows",
        measured.len()
    );
    for m in measured.iter().filter(|m| {
        matches!(
            m.verdict.as_str(),
            "fail" | "unexpected_pass" | "skipped" | GAP_CLOSED
        )
    }) {
        println!(
            "  {} #{}: {}: {}",
            m.id,
            m.repeat,
            m.verdict,
            m.problems.join("; ")
        );
    }
    if left > 0 {
        println!(
            "{left} processes carrying this run's marker were alive at the end and were killed"
        );
    } else {
        println!("no process carrying this run's marker was alive at the end");
    }
    let skipped = if allow_skips { 0 } else { count("skipped") };
    if count("fail") + count("unexpected_pass") + count(GAP_CLOSED) + skipped > 0 || left > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Kill every process whose command line carries a harness marker, and every
/// `bare` process that a ledger left in a case directory names.
fn sweep_all() -> ExitCode {
    let found: Vec<_> = procs::ps()
        .into_iter()
        .filter(|p| !p.stat.starts_with('Z') && p.command.contains(MARKER_PREFIX))
        .collect();
    // The harness's own command line does not carry a marker, but a shell
    // running this command could; never signal this process.
    let me = i32::try_from(std::process::id()).unwrap_or(0);
    let mut killed = 0;
    for p in found.iter().filter(|p| p.pid != me) {
        procs::signal(p.pid, rustix::process::Signal::KILL);
        killed += 1;
    }
    let mut bare = Vec::new();
    let top = std::fs::canonicalize(std::env::temp_dir()).unwrap_or_default();
    for case_root in read_dirs(&top, "dacc-") {
        for dir in read_dirs(&case_root, MARKER_PREFIX) {
            bare.extend(procs::bare_pids_in(&dir));
        }
    }
    for p in procs::live_bare(&bare) {
        procs::signal(p.pid, rustix::process::Signal::KILL);
        killed += 1;
    }
    println!("killed {killed} processes");
    ExitCode::SUCCESS
}

/// The subdirectories of `dir` whose names start with `prefix`.
fn read_dirs(dir: &std::path::Path, prefix: &str) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect()
}
