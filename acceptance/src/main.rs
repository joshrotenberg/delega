//! `delega-acceptance`: run the acceptance matrix against the built `delega`,
//! run the live suite against the installed providers (`--live` only), or
//! regenerate the documents from a results file. See `acceptance/README.md`.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};

use clap::Parser;
use delega_acceptance::delega_bin::{build, require_absent_managed_mcp, workspace_root};
use delega_acceptance::live;
use delega_acceptance::matrix::{self, Row};
use delega_acceptance::procs;
use delega_acceptance::report;
use delega_acceptance::run_all::{close_gaps, run_all};
use delega_acceptance::runner::{Context, GAP_CLOSED, GAP_NOT_SHOWN, MARKER_PREFIX, Measured};
use delega_acceptance::spawn::Guarded;
use delega_core::provider::managed::SystemPaths;

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
    /// Run only the rows whose expectation is a timing-dependent known gap
    /// (the matrix flag `timing_dependent`). Combines with `--repeat`, and
    /// narrows `--all`, `--smoke` and `--filter`. Exits 2 if no row is selected.
    #[arg(long)]
    timing_gaps: bool,
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
    /// Run the live suite against the installed `claude` and `codex`, with
    /// the real HOME. Calls the real models and costs money. Nothing live runs
    /// without this flag. Combines with `--provider`, `--out`, `--delega`,
    /// `--list` and `--quiet` only.
    #[arg(long)]
    live: bool,
    /// The provider for `--live`: claude or codex. Default: both.
    #[arg(long, value_parser = ["claude", "codex"])]
    provider: Option<String>,
}

/// Refuse a live option without `--live`, and `--live` with a matrix option.
fn check_live_args(args: &Args) -> Result<(), String> {
    if !args.live {
        if args.provider.is_some() {
            return Err(
                "--provider selects the provider of the live suite, which runs only with --live"
                    .to_string(),
            );
        }
        return Ok(());
    }
    let matrix_options = [
        ("--all", args.all),
        ("--smoke", args.smoke),
        ("--filter", !args.filter.is_empty()),
        ("--timing-gaps", args.timing_gaps),
        ("--repeat", args.repeat != 1),
        ("--jobs", args.jobs != 8),
        ("--report", args.report),
        ("--results", args.results.is_some()),
        ("--sweep", args.sweep),
        ("--allow-skips", args.allow_skips),
    ];
    let given: Vec<&str> = matrix_options
        .iter()
        .filter(|(_, on)| *on)
        .map(|(name, _)| *name)
        .collect();
    if given.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "--live cannot be combined with {}",
            given.join(", ")
        ))
    }
}

fn acceptance_dir() -> PathBuf {
    workspace_root().join("acceptance")
}

fn main() -> ExitCode {
    let args = Args::parse();
    if let Err(err) = check_live_args(&args) {
        eprintln!("delega-acceptance: {err}");
        return ExitCode::from(2);
    }
    if args.live {
        return run_live(&args);
    }
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
    } else if !args.filter.is_empty() || args.timing_gaps || args.list {
        matrix::all()
    } else {
        eprintln!(
            "delega-acceptance: give --all, --smoke, --filter TEXT, --timing-gaps or --live (see --help)"
        );
        return ExitCode::from(2);
    };
    if !args.filter.is_empty() {
        rows.retain(|r| args.filter.iter().any(|f| r.id.contains(f.as_str())));
    }
    if args.timing_gaps {
        rows.retain(matrix::is_timing_gap);
    }
    if args.list {
        for row in &rows {
            println!("{}", row.id);
        }
        return ExitCode::SUCCESS;
    }
    if rows.is_empty() && args.timing_gaps && args.filter.is_empty() {
        // Linux has no timing-dependent gap rows since the keeper closed them.
        println!("delega-acceptance: no timing-dependent gap rows on this system");
        return ExitCode::SUCCESS;
    }
    if rows.is_empty() {
        eprintln!("delega-acceptance: no row matches");
        return ExitCode::from(2);
    }

    let release = !cfg!(debug_assertions);
    // A supplied executable may be a release build regardless of our profile.
    if (release || args.delega.is_some())
        && rows.iter().any(|row| row.provider == "claude")
        && let Err(err) = require_absent_managed_mcp(&SystemPaths::default().claude_mcp_file())
    {
        eprintln!("delega-acceptance: {err}");
        return ExitCode::FAILURE;
    }
    let delega = match args.delega.clone().map_or_else(|| build(release), Ok) {
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
    let ctx = Context::new(delega.clone(), base, !release);
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

/// The live suite: every planned case in turn, one line each, then the
/// summary. Exits non-zero when a case fails.
fn run_live(args: &Args) -> ExitCode {
    let planned = match live::plan(args.provider.as_deref()) {
        Ok(p) => p,
        Err(err) => {
            eprintln!("delega-acceptance: {err}");
            return ExitCode::from(2);
        }
    };
    if args.list {
        for p in &planned {
            println!("{}", p.id());
        }
        return ExitCode::SUCCESS;
    }
    let delega = match args
        .delega
        .clone()
        .map_or_else(|| build(!cfg!(debug_assertions)), Ok)
    {
        Ok(path) => path,
        Err(err) => {
            eprintln!("delega-acceptance: {err}");
            return ExitCode::FAILURE;
        }
    };
    let root = match live::temp_root() {
        Ok(root) => root,
        Err(err) => {
            eprintln!("delega-acceptance: {err}");
            return ExitCode::FAILURE;
        }
    };
    let suite = live::Live::new(delega.clone(), root, live::Env::Inherit);
    if let Err(err) = live::install_stop_handler() {
        eprintln!("delega-acceptance: {err}");
        return ExitCode::FAILURE;
    }
    let runs: u32 = planned.iter().map(|p| p.case.runs()).sum();
    eprintln!(
        "live suite: {} cases, {runs} model runs, against the installed providers (Claude --model {}, Codex --effort {})",
        planned.len(),
        live::CLAUDE_MODEL,
        live::CODEX_EFFORT
    );
    if args.provider.is_some() {
        eprintln!(
            "live suite: doctor is skipped; it checks both providers and has no option to check one"
        );
    }
    let began = std::time::Instant::now();
    let mut rows = Vec::new();
    for p in planned {
        let row = suite.run(p);
        if !args.quiet || row.verdict != "pass" {
            println!("{}", live::line(&row));
        }
        rows.push(row);
        if live::STOP.load(std::sync::atomic::Ordering::SeqCst) {
            eprintln!(
                "delega-acceptance: stopped by the maintainer after {}; its directory was removed and no results were written",
                p.id()
            );
            return ExitCode::from(130);
        }
    }
    let wall = began.elapsed();
    if let Some(path) = &args.out {
        let meta = live::meta(&delega, &rows, wall);
        // A Ctrl-C while the metadata was gathered still stops the suite
        // before anything is published.
        if live::STOP.load(std::sync::atomic::Ordering::SeqCst) {
            eprintln!("delega-acceptance: stopped by the maintainer; no results were written");
            return ExitCode::from(130);
        }
        if let Err(err) = live::write_live(path, &meta, &rows) {
            eprintln!("delega-acceptance: write {}: {err}", path.display());
            return ExitCode::FAILURE;
        }
    }
    let (text, no_failure) = live::summary(&rows);
    print!("{text}");
    println!("finished in {:.0} s", wall.as_secs_f64());
    if no_failure {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
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
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .guarded_output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };
    let version = Command::new(delega)
        .args(["version", "--output", "json"])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .guarded_output()
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

#[cfg(test)]
mod tests {
    use super::{Args, check_live_args};
    use clap::Parser;

    fn parse(args: &[&str]) -> Args {
        Args::try_parse_from(std::iter::once("delega-acceptance").chain(args.iter().copied()))
            .expect("arguments parse")
    }

    #[test]
    fn a_provider_without_live_is_refused_so_nothing_live_starts_by_accident() {
        let error = check_live_args(&parse(&["--provider", "claude"])).expect_err("refused");
        assert!(error.contains("only with --live"), "{error}");
        assert!(check_live_args(&parse(&["--smoke"])).is_ok());
        assert!(!parse(&["--smoke"]).live);
    }

    #[test]
    fn live_is_refused_with_a_matrix_option() {
        for extra in [
            &["--all"][..],
            &["--smoke"],
            &["--filter", "x"],
            &["--timing-gaps"],
            &["--repeat", "3"],
            &["--report"],
            &["--sweep"],
        ] {
            let mut args = vec!["--live"];
            args.extend_from_slice(extra);
            let error = check_live_args(&parse(&args)).expect_err("refused");
            assert!(error.contains(extra[0]), "{error}");
        }
    }

    #[test]
    fn live_accepts_a_provider_an_output_file_and_a_list() {
        let args = parse(&[
            "--live",
            "--provider",
            "codex",
            "--out",
            "/x.jsonl",
            "--list",
        ]);
        assert!(check_live_args(&args).is_ok());
        assert!(
            Args::try_parse_from(["delega-acceptance", "--live", "--provider", "gemini"]).is_err()
        );
    }
}
