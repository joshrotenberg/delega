//! The reasons of `Unconfirmed` cleanup verdicts, and their recognition.
//!
//! Every reason delega-core gives an `Unconfirmed` verdict is either one of
//! the fixed texts exported from [`crate::process`] or is built here from
//! fixed parts and the values it carries. [`is_documented`] recognises them,
//! alone or joined by `"; "` as `merge_cleanup` joins the reasons of several
//! reports.
//!
//! Public only so that the acceptance harness and the tests of the binary can
//! recognise the reasons from delega-core's own definitions; it is not part of
//! the supported API.

use std::time::Duration;

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

/// Every recogniser of a single reason. Each matches a whole reason, never a
/// prefix of one.
const RECOGNISERS: &[fn(&str) -> bool] = &[
    is_fixed,
    is_tree_left_at_reap,
    is_inspection_unavailable,
    is_stopping_unconfirmed,
    is_owner_lost,
    is_run_lost,
];

/// Whether `reason` is a reason delega-core gives an `Unconfirmed` verdict, or
/// several such reasons joined by `"; "`. Some reasons contain `"; "`
/// themselves, so every way of splitting at it is tried.
pub fn is_documented(reason: &str) -> bool {
    let seps: Vec<usize> = reason.match_indices("; ").map(|(at, _)| at).collect();
    // `starts[k]`: a run of whole reasons ends just before the k-th place a
    // reason can start (the start of the text, or after the k-th separator).
    let mut starts = vec![false; seps.len() + 1];
    starts[0] = true;
    for k in 0..starts.len() {
        if !starts[k] {
            continue;
        }
        let start = if k == 0 { 0 } else { seps[k - 1] + 2 };
        if is_one(&reason[start..]) {
            return true;
        }
        for (j, &end) in seps.iter().enumerate().skip(k) {
            if is_one(&reason[start..end]) {
                starts[j + 1] = true;
            }
        }
    }
    false
}

fn is_one(reason: &str) -> bool {
    RECOGNISERS.iter().any(|recognise| recognise(reason))
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

/// The reason when process listing did not work: `error` is why, `checked`
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
            .is_some_and(|error| !error.is_empty())
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
/// view's finding, or the error of the listing that failed.
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
            && n.parse::<f64>().is_ok_and(|v| format!("{v:.1}") == n)
    });
    seconds
        && (StopDetail::ALL.iter().any(|d| d.text() == detail)
            || detail
                .strip_prefix(INSPECTION_FAILED)
                .is_some_and(|error| !error.is_empty()))
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
