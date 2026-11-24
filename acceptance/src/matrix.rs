//! The case matrix: provider behaviour crossed with caller action, and what
//! each row is expected to produce.
//!
//! Row ids are `<provider>-<case>-<variant>`. The case letters and variant
//! names of the spike (`delega-dot-spike-outcomes`, commit `7f4da23`) are kept
//! where a row carries over, so results can be compared.

use serde::{Deserialize, Serialize};

pub const PROVIDERS: [&str; 2] = ["claude", "codex"];

/// What the harness does to `delega` once the provider is running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Nothing: wait for `delega` to finish on its own (or on `--timeout`).
    Wait,
    /// Send a signal to `delega` this many milliseconds after it started.
    Signal(Sig, u64),
    /// Close the read end of `delega`'s standard output.
    CloseStdout(u64),
    /// SIGKILL to `delega` and to its keeper together, this many milliseconds
    /// after it started: the residual the keeper cannot cover.
    KillWithKeeper(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sig {
    Int,
    Term,
    Hup,
    Kill,
}

impl Sig {
    pub fn name(self) -> &'static str {
        match self {
            Sig::Int => "SIGINT",
            Sig::Term => "SIGTERM",
            Sig::Hup => "SIGHUP",
            Sig::Kill => "SIGKILL",
        }
    }
}

impl Action {
    pub fn label(self) -> String {
        match self {
            Action::Wait => "wait".to_string(),
            Action::Signal(sig, ms) => format!("{} at {ms} ms", sig.name()),
            Action::CloseStdout(ms) => format!("close stdout at {ms} ms"),
            Action::KillWithKeeper(ms) => format!("SIGKILL to delega and its keeper at {ms} ms"),
        }
    }

    /// When the harness acts, in milliseconds after the start.
    pub fn at_ms(self) -> Option<u64> {
        match self {
            Action::Wait => None,
            Action::Signal(_, ms) | Action::CloseStdout(ms) | Action::KillWithKeeper(ms) => {
                Some(ms)
            }
        }
    }
}

/// What the harness expects on standard output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Docs {
    /// Exactly one JSON document that parses and validates as a `RunResult`.
    One,
    /// The reader closed standard output, so nothing can be read.
    Unread,
    /// `delega` was killed and could not write anything.
    None,
}

/// One case: a provider behaviour, an action, and the time limit given.
#[derive(Clone, Copy, Debug)]
pub struct Case {
    pub id: &'static str,
    /// The fake provider's behaviour, see `acc-lib.sh`.
    pub behaviour: &'static str,
    /// `--timeout`, in seconds.
    pub timeout_secs: Option<u64>,
    pub action: Action,
    pub variants: &'static [&'static str],
    pub desc: &'static str,
    /// Run `delega` with its process listing made unavailable (the test-only
    /// `DELEGA_TEST_NO_PROCESS_LISTING` variable, see `acceptance/README.md`).
    pub ps_unavailable: bool,
    pub expect: ExpectBase,
}

#[derive(Clone, Copy, Debug)]
pub struct ExpectBase {
    pub outcome: &'static str,
    pub exit: Exit,
    pub docs: Docs,
    /// The expected `cleanup.confirmed`; `None` when there is no result.
    pub confirmed: Option<bool>,
}

/// How `delega` is expected to end.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Exit {
    Code(i32),
    Signal(i32),
}

impl Exit {
    pub fn label(self) -> String {
        match self {
            Exit::Code(c) => c.to_string(),
            Exit::Signal(s) => format!("signal {s}"),
        }
    }
}

const ALL: &[&str] = &[
    "plain",
    "child",
    "setsid",
    "ignore",
    "stubborn",
    "pipe",
    "orphan",
    "orphan-late",
    "relay",
    "bare",
];
const SOME: &[&str] = &["plain", "child", "setsid"];
const FEW: &[&str] = &["plain", "child", "setsid", "stubborn"];
const NO_STUBBORN: &[&str] = &[
    "plain",
    "child",
    "setsid",
    "ignore",
    "pipe",
    "orphan",
    "orphan-late",
    "relay",
    "bare",
];

const fn ok(outcome: &'static str, code: i32) -> ExpectBase {
    ExpectBase {
        outcome,
        exit: Exit::Code(code),
        docs: Docs::One,
        confirmed: Some(true),
    }
}

/// As `ok`, for a run whose process listing is unavailable: cleanup must be
/// reported as not confirmed, and say why.
const fn unconfirmed(outcome: &'static str, code: i32) -> ExpectBase {
    ExpectBase {
        outcome,
        exit: Exit::Code(code),
        docs: Docs::One,
        confirmed: Some(false),
    }
}

const SIGINT_1S: Action = Action::Signal(Sig::Int, 1000);

pub const CASES: &[Case] = &[
    Case {
        id: "a",
        behaviour: "success",
        timeout_secs: None,
        action: Action::Wait,
        variants: ALL,
        desc: "normal completion",
        ps_unavailable: false,
        expect: ok("completed", 0),
    },
    Case {
        id: "b",
        behaviour: "exit3",
        timeout_secs: None,
        action: Action::Wait,
        variants: SOME,
        desc: "provider exits 3 with an error",
        ps_unavailable: false,
        expect: ok("failed", 1),
    },
    Case {
        id: "c",
        behaviour: "busy",
        timeout_secs: Some(2),
        action: Action::Wait,
        variants: ALL,
        desc: "timeout 2 s, provider keeps producing output",
        ps_unavailable: false,
        expect: ok("timed_out", 124),
    },
    Case {
        id: "d",
        behaviour: "busy",
        timeout_secs: None,
        action: SIGINT_1S,
        variants: ALL,
        desc: "SIGINT to delega at 1 s",
        ps_unavailable: false,
        expect: ok("interrupted", 130),
    },
    Case {
        id: "dt",
        behaviour: "busy",
        timeout_secs: None,
        action: Action::Signal(Sig::Term, 1000),
        variants: FEW,
        desc: "SIGTERM to delega at 1 s",
        ps_unavailable: false,
        expect: ok("interrupted", 130),
    },
    Case {
        id: "dh",
        behaviour: "busy",
        timeout_secs: None,
        action: Action::Signal(Sig::Hup, 1000),
        variants: FEW,
        desc: "SIGHUP to delega at 1 s",
        ps_unavailable: false,
        expect: ok("interrupted", 130),
    },
    Case {
        id: "e",
        behaviour: "crash",
        timeout_secs: None,
        action: Action::Wait,
        variants: NO_STUBBORN,
        desc: "provider SIGKILLs itself 0.5 s in",
        ps_unavailable: false,
        expect: ok("failed", 1),
    },
    Case {
        id: "ed",
        behaviour: "crash",
        // The row is about a crash that happens while a time limit is pending,
        // not about the limit. The limit is far longer than the crash so that
        // a slow start on a loaded machine cannot let it fire first; a `delega`
        // that waited for the pipe to close would run into the 30 s hang limit
        // and fail the row as a hang.
        timeout_secs: Some(25),
        action: Action::Wait,
        variants: &["plain", "pipe"],
        desc: "provider SIGKILLs itself 0.5 s in, timeout 25 s",
        ps_unavailable: false,
        expect: ok("failed", 1),
    },
    Case {
        id: "fd",
        behaviour: "hang",
        timeout_secs: Some(3),
        action: Action::Wait,
        variants: ALL,
        desc: "provider hangs with no output, timeout 3 s",
        ps_unavailable: false,
        expect: ok("timed_out", 124),
    },
    Case {
        id: "hd",
        behaviour: "closeout",
        timeout_secs: Some(3),
        action: Action::Wait,
        variants: &["plain", "child"],
        desc: "provider closes stdout and stays alive, timeout 3 s",
        ps_unavailable: false,
        expect: ok("timed_out", 124),
    },
    Case {
        id: "i",
        behaviour: "nofinal",
        timeout_secs: None,
        action: Action::Wait,
        variants: SOME,
        desc: "output ends without a final answer, provider exits 0",
        ps_unavailable: false,
        expect: ok("failed", 1),
    },
    Case {
        id: "id",
        behaviour: "nofinal",
        timeout_secs: Some(3),
        action: Action::Wait,
        variants: &["plain"],
        desc: "as i, timeout 3 s",
        ps_unavailable: false,
        expect: ok("failed", 1),
    },
    Case {
        id: "fl",
        behaviour: "flood",
        timeout_secs: Some(3),
        action: Action::Wait,
        variants: &["plain", "child", "ignore"],
        desc: "provider writes output at full speed, timeout 3 s",
        ps_unavailable: false,
        expect: ok("timed_out", 124),
    },
    Case {
        id: "fli",
        behaviour: "flood",
        timeout_secs: None,
        action: SIGINT_1S,
        variants: &["plain", "child"],
        desc: "provider writes output at full speed, SIGINT at 1 s",
        ps_unavailable: false,
        expect: ok("interrupted", 130),
    },
    Case {
        id: "dc",
        behaviour: "busy",
        timeout_secs: Some(3),
        action: Action::CloseStdout(1000),
        variants: SOME,
        desc: "reader closes delega's stdout at 1 s, timeout 3 s",
        ps_unavailable: false,
        expect: ExpectBase {
            outcome: "timed_out",
            exit: Exit::Code(124),
            docs: Docs::Unread,
            confirmed: None,
        },
    },
    Case {
        id: "dca",
        behaviour: "slow",
        timeout_secs: None,
        action: Action::CloseStdout(300),
        variants: &["plain"],
        desc: "reader closes delega's stdout at 0.3 s, provider finishes at 1.5 s",
        ps_unavailable: false,
        expect: ExpectBase {
            outcome: "completed",
            exit: Exit::Code(0),
            docs: Docs::Unread,
            confirmed: None,
        },
    },
    Case {
        id: "k",
        behaviour: "hang",
        timeout_secs: None,
        action: Action::Signal(Sig::Kill, 1000),
        variants: ALL,
        desc: "SIGKILL to delega at 1 s",
        ps_unavailable: false,
        expect: ExpectBase {
            outcome: "none",
            exit: Exit::Signal(9),
            docs: Docs::None,
            confirmed: None,
        },
    },
    Case {
        id: "kk",
        behaviour: "hang",
        timeout_secs: None,
        action: Action::KillWithKeeper(1000),
        variants: &["plain"],
        desc: "SIGKILL to delega and its keeper at 1 s",
        ps_unavailable: false,
        expect: ExpectBase {
            outcome: "none",
            exit: Exit::Signal(9),
            docs: Docs::None,
            confirmed: None,
        },
    },
    Case {
        id: "u",
        behaviour: "success",
        timeout_secs: None,
        action: Action::Wait,
        variants: &["plain", "child", "setsid", "orphan", "bare"],
        desc: "normal completion, process listing unavailable",
        ps_unavailable: true,
        expect: unconfirmed("completed", 0),
    },
    Case {
        id: "ud",
        behaviour: "hang",
        timeout_secs: Some(3),
        action: Action::Wait,
        variants: &["plain", "child", "setsid", "stubborn"],
        desc: "provider hangs, timeout 3 s, process listing unavailable",
        ps_unavailable: true,
        expect: unconfirmed("timed_out", 124),
    },
    Case {
        id: "ui",
        behaviour: "busy",
        timeout_secs: None,
        action: SIGINT_1S,
        variants: &["plain", "child", "setsid", "ignore"],
        desc: "SIGINT to delega at 1 s, process listing unavailable",
        ps_unavailable: true,
        expect: unconfirmed("interrupted", 130),
    },
];

/// Rows that are spike rows, listed so the report can say which carried over.
/// A case id that is in the spike keeps its meaning; these are new here.
pub const NEW_CASES: &[&str] = &[
    "dt", "dh", "fl", "fli", "dc", "dca", "k", "kk", "u", "ud", "ui",
];

/// Cases of the spike that are not run, and why.
pub const DROPPED: &[(&str, &str)] = &[
    (
        "f",
        "Provider hangs with no output and no deadline. `delega run` has a default time limit of 600 s, so the row would take ten minutes; `fd` (timeout 3 s) and `k`/`d` cover the hang.",
    ),
    (
        "h",
        "Provider closes stdout and stays alive with no deadline. Same 600 s default; `hd` covers it with a 3 s timeout.",
    ),
    (
        "g, gd, gr",
        "A permission asked and nobody answers. `delega run` answers every request no at once (`permissions.on_request` is `deny`); there is no live request to leave unanswered.",
    ),
    (
        "j",
        "The `wait` future is dropped. The CLI has no future; the nearest caller actions are `dc` (reader closes stdout) and `k` (caller killed).",
    ),
    (
        "j2",
        "The handle is dropped. The CLI has no handle; SIGKILL of the caller (`k`) is the nearest row.",
    ),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeakClass {
    /// Nothing may survive unless the report says so, and the survivors are
    /// exactly the listed roles.
    None,
    /// A gap the process module documents.
    DocumentedGap,
    /// A gap the process module does not document, found by this harness.
    UndocumentedGap,
    /// A defect found by this harness.
    Defect,
}

impl LeakClass {
    pub fn label(self) -> &'static str {
        match self {
            LeakClass::None => "none",
            LeakClass::DocumentedGap => "documented gap",
            LeakClass::UndocumentedGap => "undocumented gap",
            LeakClass::Defect => "defect",
        }
    }
}

/// What one row is expected to do.
#[derive(Clone, Debug)]
pub struct Expect {
    pub outcome: &'static str,
    pub exit: Exit,
    pub docs: Docs,
    pub confirmed: Option<bool>,
    pub leak: LeakClass,
    /// Survivor roles that identify the gap in the report.
    pub survivors: Vec<&'static str>,
    pub reason: Option<&'static str>,
    /// Whether to include this gap when selecting `--timing-gaps`.
    pub timing_dependent: bool,
}

/// A gap to observe for the rows that match. An empty list
/// matches anything. The first match wins.
struct Gap {
    cases: &'static [&'static str],
    variants: &'static [&'static str],
    class: LeakClass,
    survivors: &'static [&'static str],
    reason: &'static str,
    /// See [`Expect::timing_dependent`].
    timing_dependent: bool,
    /// Only on this operating system (`std::env::consts::OS`); `None` on all.
    os: Option<&'static str>,
    /// Only on a system that hides the environment of its own programs from
    /// `ps -E` (see [`Machine`]).
    hidden_env_only: bool,
}

/// What the expectations depend on besides the matrix: the operating system,
/// and on macOS whether it hides the environment of its own programs from
/// `ps -E` (System Integrity Protection does, for Apple's programs). The run
/// marker finds an orphan of the fakes, which run `sh`, `sleep` and `perl`,
/// only when it does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Machine {
    pub os: &'static str,
    pub hides_system_env: bool,
}

impl Machine {
    /// This machine, probed once (see
    /// [`system_programs_hide_environment`](crate::procs::system_programs_hide_environment)).
    pub fn current() -> Self {
        Self {
            os: std::env::consts::OS,
            hides_system_env: crate::procs::system_programs_hide_environment(),
        }
    }
}

const KEEPER_KILLED_REASON: &str = "SIGKILL of delega and of its keeper together: no code of either runs, so nothing stops the worker or enforces its time limit. The accepted residual on every platform (process module docs, \"What remains possible\").";
const MACOS_PLATFORM_REASON: &str = "macOS residual on a system that hides the environment of its own programs from `ps -E` (System Integrity Protection does): the process left the provider's session and its parent is gone, so no listing ties it to the run, macOS has no subreaper, and the run marker is not visible because these fakes run sh, sleep and perl. Where the environment is visible the marker can find it. Linux closes this through the keeper (process module docs, \"What remains possible\").";
const MACOS_RELAY_REASON: &str = "macOS residual: a fast fork relay can leave each listing before cleanup signals it. Linux closes this through the keeper's own walk of its tree (process module docs, \"What remains possible\").";

// Only this table can mark a row as a documented residual on a platform.
const GAPS: &[Gap] = &[
    Gap {
        cases: &["kk"],
        variants: &[],
        class: LeakClass::DocumentedGap,
        survivors: &["provider"],
        reason: KEEPER_KILLED_REASON,
        timing_dependent: false,
        os: None,
        hidden_env_only: false,
    },
    Gap {
        cases: &[],
        variants: &["relay"],
        class: LeakClass::DocumentedGap,
        survivors: &["relay-heartbeat"],
        reason: MACOS_RELAY_REASON,
        timing_dependent: true,
        os: Some("macos"),
        hidden_env_only: false,
    },
    Gap {
        cases: &[],
        variants: &["orphan"],
        class: LeakClass::DocumentedGap,
        survivors: &["leaf-setsid"],
        reason: MACOS_PLATFORM_REASON,
        timing_dependent: true,
        os: Some("macos"),
        hidden_env_only: true,
    },
    Gap {
        cases: &["a"],
        variants: &["bare"],
        class: LeakClass::DocumentedGap,
        survivors: &["bare"],
        reason: MACOS_PLATFORM_REASON,
        timing_dependent: true,
        os: Some("macos"),
        hidden_env_only: true,
    },
];

/// Roles that survive when the process listing is unavailable: the worker's
/// group is killed, but a descendant in a session of its own is not found.
/// The report says so (cleanup not confirmed), so this is not a failure.
fn unlisted_survivors(variant: &str, os: &str) -> Vec<&'static str> {
    // On Linux the keeper stops its whole tree by walking `/proc`, which needs
    // no process listing program.
    if os == "linux" {
        return Vec::new();
    }
    match variant {
        "setsid" | "orphan" => vec!["leaf-setsid"],
        "bare" => vec!["bare"],
        _ => Vec::new(),
    }
}

/// The expectation of one row on this machine.
pub fn expect_for(case: &Case, variant: &str) -> Expect {
    expect_for_machine(case, variant, Machine::current())
}

/// The expectation of one row on `machine`.
pub fn expect_for_machine(case: &Case, variant: &str, machine: Machine) -> Expect {
    let os = machine.os;
    let mut expect = Expect {
        outcome: case.expect.outcome,
        exit: case.expect.exit,
        docs: case.expect.docs,
        confirmed: case.expect.confirmed,
        leak: LeakClass::None,
        survivors: Vec::new(),
        reason: None,
        timing_dependent: false,
    };
    if case.ps_unavailable {
        expect.survivors = unlisted_survivors(variant, os);
        return expect;
    }
    for gap in GAPS {
        let hit = (gap.cases.is_empty() || gap.cases.contains(&case.id))
            && (gap.variants.is_empty() || gap.variants.contains(&variant))
            && gap.os.is_none_or(|only| only == os)
            && (!gap.hidden_env_only || machine.hides_system_env);
        if hit {
            expect.leak = gap.class;
            expect.survivors = gap.survivors.to_vec();
            expect.reason = Some(gap.reason);
            expect.timing_dependent = gap.timing_dependent;
            break;
        }
    }
    expect
}

/// One row to run.
#[derive(Clone, Debug)]
pub struct Row {
    pub id: String,
    pub provider: &'static str,
    pub case: &'static Case,
    pub variant: &'static str,
    pub expect: Expect,
}

pub fn all() -> Vec<Row> {
    all_for(Machine::current())
}

/// Every row with its expectation on `machine`, so results recorded on one
/// system can be checked on another.
pub fn all_for(machine: Machine) -> Vec<Row> {
    let mut rows = Vec::new();
    for provider in PROVIDERS {
        for case in CASES {
            for variant in case.variants {
                rows.push(Row {
                    id: format!("{provider}-{}-{variant}", case.id),
                    provider,
                    case,
                    variant,
                    expect: expect_for_machine(case, variant, machine),
                });
            }
        }
    }
    rows
}

/// Whether a row's expectation is a timing-dependent known gap, the rows
/// `--timing-gaps` selects.
pub fn is_timing_gap(row: &Row) -> bool {
    row.expect.timing_dependent
}

/// A handful of rows for `cargo test`: both providers, a normal run, an
/// error, a timeout with a child, a signal, a crash, and a SIGKILL of delega
/// after which its keeper must leave nothing.
pub const SMOKE: &[&str] = &[
    "claude-a-plain",
    "codex-a-plain",
    "claude-b-plain",
    "codex-c-child",
    "claude-d-plain",
    "codex-e-plain",
    "claude-i-plain",
    "codex-dc-plain",
    "claude-k-plain",
    "claude-a-ignore",
    "codex-u-plain",
];

pub fn smoke() -> Vec<Row> {
    all()
        .into_iter()
        .filter(|r| SMOKE.contains(&r.id.as_str()))
        .collect()
}

/// Roles the fake records for a variant, beyond `provider`; the harness waits
/// for them before it acts so a descendant exists when the action lands.
pub fn roles_of(variant: &str) -> &'static [&'static str] {
    match variant {
        "child" | "pipe" => &["child"],
        "ignore" => &["child-ignore"],
        "setsid" => &["mid", "leaf-setsid"],
        "orphan" | "orphan-late" => &["mid", "leaf-setsid"],
        "relay" => &["relay"],
        "bare" => &["bare"],
        _ => &[],
    }
}

/// Whether a variant needs `perl` (descendants are started through it).
pub fn needs_perl(variant: &str) -> bool {
    variant != "plain" && variant != "stubborn"
}

#[cfg(test)]
mod timing_gap_tests {
    use super::*;

    #[test]
    fn only_the_listed_platform_residuals_get_the_survivor_exemption() {
        let relay = [
            "a-relay", "c-relay", "d-relay", "e-relay", "fd-relay", "k-relay",
        ];
        let hidden = [
            "a-bare",
            "a-orphan",
            "c-orphan",
            "d-orphan",
            "e-orphan",
            "fd-orphan",
            "k-orphan",
        ];
        for (os, hides_system_env) in [("linux", false), ("macos", false), ("macos", true)] {
            let mut expected = vec!["kk-plain"];
            if os == "macos" {
                expected.extend(relay);
                if hides_system_env {
                    expected.extend(hidden);
                }
            }
            let expected = PROVIDERS
                .iter()
                .flat_map(|provider| expected.iter().map(move |id| format!("{provider}-{id}")))
                .collect::<std::collections::BTreeSet<_>>();
            let actual = all_for(Machine {
                os,
                hides_system_env,
            })
            .into_iter()
            .filter(|row| row.expect.leak == LeakClass::DocumentedGap)
            .map(|row| row.id)
            .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(
                actual, expected,
                "{os}, hidden environment: {hides_system_env}"
            );
        }
    }

    #[test]
    fn timing_gap_selection_is_exactly_the_timing_dependent_gap_rows() {
        // macOS with its programs' environment hidden has the most gap rows;
        // Linux has none.
        let linux = all_for(Machine {
            os: "linux",
            hides_system_env: false,
        });
        assert!(linux.iter().all(|r| !is_timing_gap(r)));
        let rows = all_for(Machine {
            os: "macos",
            hides_system_env: true,
        });
        let visible = all_for(Machine {
            os: "macos",
            hides_system_env: false,
        });
        let count = |rows: &[Row]| rows.iter().filter(|r| is_timing_gap(r)).count();
        assert!(count(&visible) < count(&rows));
        assert!(
            visible
                .iter()
                .all(|r| !is_timing_gap(r) || r.variant == "relay")
        );
        let expected = rows.iter().filter(|r| r.expect.timing_dependent).count();
        let selected: Vec<&Row> = rows.iter().filter(|r| is_timing_gap(r)).collect();
        assert!(expected > 0, "the matrix has timing-dependent gap rows");
        assert_eq!(selected.len(), expected);
        assert!(selected.len() < rows.len());
        for row in &selected {
            assert!(row.expect.timing_dependent);
            assert_ne!(row.expect.leak, LeakClass::None, "{}", row.id);
        }
        for row in rows.iter().filter(|r| !is_timing_gap(r)) {
            assert!(!row.expect.timing_dependent, "{}", row.id);
        }
    }
}
