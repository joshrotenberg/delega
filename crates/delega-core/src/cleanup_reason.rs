//! The reasons of `Unconfirmed` cleanup verdicts, and their recognition.
//!
//! Every reason delega-core gives an `Unconfirmed` verdict is either one of
//! the fixed texts exported from [`crate::process`] or is built here from
//! fixed parts and the values it carries. [`is_documented`] recognises them
//! as they can reach a verdict: a provider run joins the reasons of up to
//! three process reports with `"; "` (`merge_cleanup`), and a run that ended
//! without its result has one reason of its own.
//!
//! Public only so that the acceptance harness and the tests of the binary can
//! recognise the reasons from delega-core's own definitions; it is not part of
//! the supported API.

use std::time::Duration;

use crate::process::PS_PROGRAMS;
use crate::process::limits::MAX_ERROR_CHARS;

use crate::process::{
    FINAL_CENSUS_UNPROVEN_REASON, FREEZE_INCOMPLETE_REASON, KEEPER_LOST_REASON,
    KEEPER_UNANSWERED_REASON, KEEPER_UNPROVEN_REASON, LATE_LISTING_REASON, NO_REAP_ACK_REASON,
    NO_SUBREAPER_REASON, PIPES_HELD_REASON, TREE_LEFT_AT_REAP_REASON, UNVERIFIED_SIGNAL_REASON,
};

/// Defines a closed set of texts that a reason can carry, with every value
/// listed in `ALL` so that recognition cannot miss one.
macro_rules! texts {
    (
        $(#[$meta:meta])*
        $name:ident { $($(#[$vmeta:meta])* $variant:ident => $text:literal,)+ }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub(crate) enum $name {
            $($(#[$vmeta])* $variant,)+
        }

        impl $name {
            /// Every value.
            pub(crate) const ALL: &[Self] = &[$(Self::$variant,)+];

            /// The text the reason carries.
            pub(crate) fn text(self) -> &'static str {
                match self {
                    $(Self::$variant => $text,)+
                }
            }
        }
    };
}

/// The reasons that carry no value, each a complete reason.
const FIXED: &[&str] = &[
    FINAL_CENSUS_UNPROVEN_REASON,
    FREEZE_INCOMPLETE_REASON,
    KEEPER_LOST_REASON,
    KEEPER_UNANSWERED_REASON,
    KEEPER_UNPROVEN_REASON,
    LATE_LISTING_REASON,
    NO_REAP_ACK_REASON,
    NO_SUBREAPER_REASON,
    PIPES_HELD_REASON,
    UNVERIFIED_SIGNAL_REASON,
];

/// Every recogniser of the reason of one process report. Each matches a whole
/// reason, never a prefix of one.
const PROCESS_RECOGNISERS: &[fn(&str) -> bool] = &[
    is_fixed,
    is_tree_left_at_reap,
    is_inspection_unavailable,
    is_stopping_unconfirmed,
    is_owner_lost,
];

/// Every recogniser of a reason that is a verdict's whole reason, never
/// joined to another: the provider run and the supervisor build it alone.
const STANDALONE_RECOGNISERS: &[fn(&str) -> bool] = &[is_run_lost];

/// The most process reports a provider run merges: the version probe, the
/// Codex MCP discovery and the run itself (`execute_steps` in
/// `provider/run.rs`).
const MAX_MERGED_REPORTS: usize = 3;

/// Whether `reason` is a reason delega-core gives an `Unconfirmed` verdict:
/// one standalone reason, or the reasons of one to [`MAX_MERGED_REPORTS`]
/// process reports joined by `"; "`, in any order. Some reasons contain
/// `"; "` themselves, so every way of splitting at it is tried.
pub fn is_documented(reason: &str) -> bool {
    if STANDALONE_RECOGNISERS.iter().any(|r| r(reason)) {
        return true;
    }
    let seps: Vec<usize> = reason.match_indices("; ").map(|(at, _)| at).collect();
    // `fewest[k]`: the fewest whole process reasons that end just before the
    // k-th place a reason can start (the start of the text, or after the k-th
    // separator), if any do.
    let mut fewest: Vec<Option<usize>> = vec![None; seps.len() + 1];
    fewest[0] = Some(0);
    for k in 0..fewest.len() {
        let Some(before) = fewest[k] else {
            continue;
        };
        if before == MAX_MERGED_REPORTS {
            continue;
        }
        let start = if k == 0 { 0 } else { seps[k - 1] + 2 };
        if is_process_reason(&reason[start..]) {
            return true;
        }
        for (j, &end) in seps.iter().enumerate().skip(k) {
            if is_process_reason(&reason[start..end]) {
                let count = before + 1;
                fewest[j + 1] = Some(fewest[j + 1].map_or(count, |c| c.min(count)));
            }
        }
    }
    false
}

fn is_process_reason(reason: &str) -> bool {
    PROCESS_RECOGNISERS
        .iter()
        .any(|recognise| recognise(reason))
}

fn is_fixed(reason: &str) -> bool {
    FIXED.contains(&reason)
}

/// `"{count} {TREE_LEFT_AT_REAP_REASON}"`, with a count above zero.
pub(crate) fn tree_left_at_reap(count: usize) -> String {
    format!("{count} {TREE_LEFT_AT_REAP_REASON}")
}

fn is_tree_left_at_reap(reason: &str) -> bool {
    reason
        .strip_suffix(TREE_LEFT_AT_REAP_REASON)
        .and_then(|rest| rest.strip_suffix(' '))
        .is_some_and(|count| {
            count
                .parse::<usize>()
                .is_ok_and(|n| n > 0 && n.to_string() == count)
        })
}

texts! {
    /// What the single check that needs no process listing found.
    GroupCheck {
        /// No process has the child's group id.
        Empty => "no process has the child's group id",
        /// A process may still have it.
        Present => "a process may still have the child's group id",
        /// The leader had not exited, so the group was not probed.
        LeaderAlive => "the child had not exited",
    }
}

const INSPECTION_UNAVAILABLE_START: &str = "process inspection was unavailable (";
const INSPECTION_UNAVAILABLE_CHECKED: &str =
    "); only the child's process group was checked with kill(-pgid, 0): ";
const INSPECTION_UNAVAILABLE_END: &str =
    ". Descendants outside the group and session could not be checked.";
const DESCENDANTS_LISTED: &str =
    " Descendants seen at the latest scan are listed because they cannot be checked.";

/// The reason when process listing did not work: `error` is why (one of the
/// listing errors [`is_listing_error`] recognises), `checked`
/// what the group probe found, and `listed` whether descendants seen earlier
/// are listed in the verdict.
pub(crate) fn inspection_unavailable(error: &str, checked: GroupCheck, listed: bool) -> String {
    format!(
        "{INSPECTION_UNAVAILABLE_START}{error}{INSPECTION_UNAVAILABLE_CHECKED}{}\
         {INSPECTION_UNAVAILABLE_END}{}",
        checked.text(),
        if listed { DESCENDANTS_LISTED } else { "" },
    )
}

fn is_inspection_unavailable(reason: &str) -> bool {
    let Some(rest) = reason.strip_prefix(INSPECTION_UNAVAILABLE_START) else {
        return false;
    };
    let rest = rest.strip_suffix(DESCENDANTS_LISTED).unwrap_or(rest);
    let Some(rest) = rest.strip_suffix(INSPECTION_UNAVAILABLE_END) else {
        return false;
    };
    GroupCheck::ALL.iter().any(|checked| {
        rest.strip_suffix(checked.text())
            .and_then(|rest| rest.strip_suffix(INSPECTION_UNAVAILABLE_CHECKED))
            .is_some_and(is_listing_error)
    })
}

texts! {
    /// What the last view showed when stopping was not confirmed in time.
    StopDetail {
        /// The child's group still had live members.
        GroupLive => "the child's process group still had live members",
        /// Processes in the child's session were still running.
        SessionRunning => "processes in the child's session were still running",
        /// Tracked descendants were still running.
        TrackedRunning => "tracked descendants were still running",
        /// The leader had not exited, or a sweep was not clean yet.
        NotClean => "the child had not exited or the sweeps were not yet clean",
        /// No process listing completed.
        NoInspection => "no process inspection completed",
    }
}

const STOPPING_START: &str = "stopping was not confirmed within ";
const INSPECTION_FAILED: &str = "process inspection failed: ";

/// The reason when cleanup did not confirm the stop within `waited`: the last
/// view's finding, or the error of the listing that failed (one of the listing
/// errors [`is_listing_error`] recognises).
pub(crate) fn stopping_unconfirmed(waited: Duration, detail: Result<StopDetail, &str>) -> String {
    let waited = format!("{:.1} s", waited.as_secs_f64());
    match detail {
        Ok(detail) => format!("{STOPPING_START}{waited}: {}", detail.text()),
        Err(error) => format!("{STOPPING_START}{waited}: {INSPECTION_FAILED}{error}"),
    }
}

fn is_stopping_unconfirmed(reason: &str) -> bool {
    let Some((waited, detail)) = reason
        .strip_prefix(STOPPING_START)
        .and_then(|rest| rest.split_once(": "))
    else {
        return false;
    };
    let seconds = waited.strip_suffix(" s").is_some_and(|n| {
        n.bytes().all(|b| b.is_ascii_digit() || b == b'.')
            && n.parse::<f64>()
                .is_ok_and(|v| v <= Duration::MAX.as_secs_f64() && format!("{v:.1}") == n)
    });
    seconds
        && (StopDetail::ALL.iter().any(|d| d.text() == detail)
            || detail
                .strip_prefix(INSPECTION_FAILED)
                .is_some_and(is_listing_error))
}

texts! {
    /// A process listing error that carries no value.
    ListingFailure {
        /// `ps` did not finish within its deadline.
        TimedOut => "process listing timed out",
        /// The test seam switched listing off (macOS marker listing).
        SwitchedOff => "process listing is switched off",
        /// `ps` had no output pipe.
        NoStdout => "ps: no stdout pipe",
        /// `ps` wrote more than the listing bound.
        TooLarge => "ps: output too large",
        /// The listing held no rows.
        NoRows => "the process listing held no rows",
        /// The listing did not hold this process.
        OwnMissing => "the process listing does not contain this process",
        /// The listing task panicked.
        InspectorPanicked => "process inspector panicked",
        /// Building the listing task panicked.
        FactoryPanicked => "process inspector factory panicked",
        /// The marker reader panicked.
        MarkerReaderPanicked => "the marker reader panicked",
        /// A marker listing line did not parse.
        MarkerUnparsed => "the marker listing had a line that could not be parsed",
        /// The marker listing held no rows.
        MarkerNoRows => "the marker listing held no rows",
        /// The marker listing did not hold this process.
        MarkerOwnMissing => "the marker listing does not contain this process",
    }
}

const PS_IO: &str = "ps: ";
const PS_EXIT_START: &str = "ps failed (exit ";
const UNPARSED_START: &str = "the process listing had ";
const UNPARSED_END: &str = " lines that could not be parsed";
/// Starts the error for a listing program that could not be started.
pub(crate) const NOT_STARTED_START: &str = "ps could not be started (";
/// The detail of that error when no listing program is configured.
pub(crate) const NO_PS_PROGRAM: &str = "no ps program configured";
/// Ends that error when it was cut at [`MAX_ERROR_CHARS`].
pub(crate) const NOT_STARTED_CUT: &str = "...";

/// A failed read of, or wait for, `ps`.
pub(crate) fn ps_io_error(error: &std::io::Error) -> String {
    format!("{PS_IO}{error}")
}

/// `ps` exited unsuccessfully with `code`.
pub(crate) fn ps_exit(code: Option<i32>) -> String {
    format!("{PS_EXIT_START}{code:?})")
}

/// The listing had `count` lines that did not parse.
pub(crate) fn unparsed_lines(count: usize) -> String {
    format!("{UNPARSED_START}{count}{UNPARSED_END}")
}

/// Whether `error` is the text of a process listing error (`InspectError`):
/// a fixed text, an exit status, a count of unparsed lines, or an error whose
/// only free text is an OS error's description (see [`is_os_error`]).
fn is_listing_error(error: &str) -> bool {
    ListingFailure::ALL.iter().any(|f| f.text() == error)
        || error.strip_prefix(PS_IO).is_some_and(is_os_error)
        || error
            .strip_prefix(PS_EXIT_START)
            .and_then(|rest| rest.strip_suffix(')'))
            .is_some_and(|code| {
                code == "None"
                    || code
                        .strip_prefix("Some(")
                        .and_then(|n| n.strip_suffix(')'))
                        .is_some_and(|n| n.parse::<i32>().is_ok_and(|v| v.to_string() == n))
            })
        || error
            .strip_prefix(UNPARSED_START)
            .and_then(|rest| rest.strip_suffix(UNPARSED_END))
            .is_some_and(|n| {
                n.parse::<usize>()
                    .is_ok_and(|v| v > 0 && v.to_string() == n)
            })
        || is_not_started(error)
}

/// `"{description} (os error {code})"`, as `std::io::Error` shows an OS
/// error. The description is the system's text: not empty, on one line, and
/// without `;`.
fn is_os_error(text: &str) -> bool {
    text.strip_suffix(')')
        .and_then(|rest| rest.rsplit_once(" (os error "))
        .is_some_and(|(description, code)| {
            !description.is_empty()
                && !description.contains([';', '\n'])
                && code.parse::<i32>().is_ok_and(|v| v.to_string() == code)
        })
}

/// The error for a listing program that could not be started
/// (`tree::not_started`): `"ps could not be started ({detail})"`, where the
/// detail is [`NO_PS_PROGRAM`] or `"{path}: {os error}"` for each program
/// tried, joined by `", "`, and the paths are the listing programs. A message
/// over [`MAX_ERROR_CHARS`] characters is cut to that length ending in
/// [`NOT_STARTED_CUT`]; what is left must still start like one.
fn is_not_started(error: &str) -> bool {
    let chars = error.chars().count();
    if chars > MAX_ERROR_CHARS {
        return false;
    }
    let Some(rest) = error.strip_prefix(NOT_STARTED_START) else {
        return false;
    };
    if let Some(detail) = rest.strip_suffix(')')
        && (detail == NO_PS_PROGRAM || is_spawn_failures(detail))
    {
        return true;
    }
    chars == MAX_ERROR_CHARS
        && rest.strip_suffix(NOT_STARTED_CUT).is_some_and(|cut| {
            !cut.contains([';', '\n'])
                && PS_PROGRAMS
                    .iter()
                    .any(|p| cut.strip_prefix(p).is_some_and(|r| r.starts_with(": ")))
        })
}

/// `"{path}: {os error}"` for one or more listing programs, joined by `", "`.
fn is_spawn_failures(detail: &str) -> bool {
    PS_PROGRAMS.iter().any(|program| {
        let Some(rest) = detail
            .strip_prefix(program)
            .and_then(|r| r.strip_prefix(": "))
        else {
            return false;
        };
        is_os_error(rest)
            || rest
                .match_indices(", ")
                .any(|(at, _)| is_os_error(&rest[..at]) && is_spawn_failures(&rest[at + 2..]))
    })
}

texts! {
    /// Why the owner of a process ended without a report.
    OwnerLost {
        /// The report had been taken already.
        ReportTaken => "the report was already taken",
        /// The owner task panicked.
        Panicked => "the owner task panicked",
        /// The runtime shut down first.
        RuntimeShutDown => "the runtime shut down before the run finished",
    }
}

const OWNER_LOST_KILL: &str =
    "; a best-effort SIGKILL was sent to the process group but no check confirmed it";
const OWNER_LOST_KEEPER: &str =
    "; the keeper stops what is left when the owner's end of its socket closes";

/// The reason when the owner ended without a report; `kept` when a keeper ran
/// the program.
pub(crate) fn owner_lost(cause: OwnerLost, kept: bool) -> String {
    format!(
        "{}{OWNER_LOST_KILL}{}",
        cause.text(),
        if kept { OWNER_LOST_KEEPER } else { "" }
    )
}

fn is_owner_lost(reason: &str) -> bool {
    let rest = reason.strip_suffix(OWNER_LOST_KEEPER).unwrap_or(reason);
    rest.strip_suffix(OWNER_LOST_KILL)
        .is_some_and(|cause| OwnerLost::ALL.iter().any(|c| c.text() == cause))
}

texts! {
    /// Why a provider run ended without its result.
    RunLost {
        /// The result had been taken already.
        ResultTaken => "the result was already taken",
        /// The run task panicked.
        TaskPanicked => "the run task panicked",
        /// The runtime shut down first.
        RuntimeShutDown => "the runtime shut down before the run finished",
        /// The supervisor had no request to start the run with.
        NoRequest => "the run has no request to start",
        /// The supervisor had no worker for the run.
        NoWorker => "the run has no worker",
        /// The supervisor's task that follows the run panicked.
        FollowerPanicked => "the task that follows the run panicked",
        /// The supervisor's task that follows the run was cancelled.
        FollowerCancelled => "the task that follows the run was cancelled",
    }
}

const RUN_LOST_END: &str = "; the provider process, if one started, was sent an interrupt but nothing confirmed it stopped";

/// The reason when a provider run ended without its result.
pub(crate) fn run_lost(cause: RunLost) -> String {
    format!("{}{RUN_LOST_END}", cause.text())
}

fn is_run_lost(reason: &str) -> bool {
    reason
        .strip_suffix(RUN_LOST_END)
        .is_some_and(|cause| RunLost::ALL.iter().any(|c| c.text() == cause))
}

#[cfg(test)]
mod tests;
