//! What a run of a child process reports: one terminal report per child.

use std::time::{Duration, SystemTime};

use crate::contract::outcome::Outcome;
use crate::contract::result::{Cleanup, ErrorDetail};

use super::capture::Captured;
use super::timestamp::rfc3339_utc;
use super::tree::InspectionStats;

/// How the child's run ended. Exactly one per report.
///
/// A timeout and an interruption are endings of their own, not failures with
/// an error code: they are decided by the owner, and what the child then did
/// under the stop signals is in [`ProcessReport::exit`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ending {
    /// The child exited with a status code on its own.
    Exited {
        /// The exit code.
        code: i32,
    },
    /// The child was ended by a signal that the owner did not send as part of
    /// a stop (a crash, or something outside the owner).
    Signaled {
        /// The signal number.
        signal: i32,
        /// The conventional name, such as `SIGSEGV`.
        name: String,
    },
    /// The time limit passed and the owner stopped the child.
    TimedOut {
        /// The limit that applied.
        limit: Duration,
    },
    /// The caller interrupted the run, or dropped its handle, and the owner
    /// stopped the child.
    Interrupted,
    /// The child never started: the program was not found, not executable, or
    /// the working directory was unusable. Nothing was left behind.
    FailedToStart {
        /// What the operating system said.
        message: String,
    },
    /// The child started but its exit status could not be read, or the owner
    /// task ended without a report. See `cleanup` for what is known.
    WaitFailed {
        /// What went wrong.
        message: String,
    },
}

impl Ending {
    /// The contract outcome this ending maps to.
    ///
    /// `Completed` here means only that the process exited with code 0. A
    /// layer that reads the provider's answer decides whether the run
    /// completed. The process layer never produces `Refused`.
    pub fn outcome(&self) -> Outcome {
        match self {
            Ending::Exited { code: 0 } => Outcome::Completed,
            Ending::Exited { .. }
            | Ending::Signaled { .. }
            | Ending::FailedToStart { .. }
            | Ending::WaitFailed { .. } => Outcome::Failed,
            Ending::TimedOut { .. } => Outcome::TimedOut,
            Ending::Interrupted => Outcome::Interrupted,
        }
    }

    /// The contract error for an ending other than a clean exit.
    ///
    /// Kinds: `timed_out` and `interrupted` (as the contract constructors
    /// use), `process_exited`, `process_signaled`, `spawn_failed` and
    /// `wait_failed`.
    pub fn error_detail(&self) -> Option<ErrorDetail> {
        let (kind, message, provider_exit_code) = match self {
            Ending::Exited { code: 0 } => return None,
            Ending::Exited { code } => (
                "process_exited",
                format!("the process exited with code {code}"),
                Some(*code),
            ),
            Ending::Signaled { name, .. } => (
                "process_signaled",
                format!("the process was ended by {name}"),
                None,
            ),
            Ending::TimedOut { limit } => (
                "timed_out",
                format!(
                    "the process was still running after {} s and was stopped",
                    limit.as_secs_f64()
                ),
                None,
            ),
            Ending::Interrupted => (
                "interrupted",
                "the process was stopped at the caller's request".to_string(),
                None,
            ),
            Ending::FailedToStart { message } => ("spawn_failed", message.clone(), None),
            Ending::WaitFailed { message } => ("wait_failed", message.clone(), None),
        };
        Some(ErrorDetail {
            kind: kind.to_string(),
            message,
            provider_exit_code,
        })
    }
}

/// How the child itself ended, as the operating system reported it when the
/// owner reaped it. Present whenever the child started and was reaped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaderExit {
    /// The exit code, when the child exited with one.
    pub code: Option<i32>,
    /// The signal number, when a signal ended it.
    pub signal: Option<i32>,
    /// The name of that signal.
    pub signal_name: Option<String>,
}

/// When the child started and finished, and the limit that applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessTiming {
    /// When the child was started. Absent if it never started.
    pub started_at: Option<SystemTime>,
    /// When the owner finished, after cleanup.
    pub finished_at: SystemTime,
    /// From start to finish. Zero if it never started.
    pub duration: Duration,
    /// The time limit that applied.
    pub time_limit: Duration,
}

impl ProcessTiming {
    /// `started_at` as an RFC 3339 UTC string, for the contract's `Timing`.
    pub fn started_at_rfc3339(&self) -> Option<String> {
        self.started_at.map(rfc3339_utc)
    }

    /// `finished_at` as an RFC 3339 UTC string.
    pub fn finished_at_rfc3339(&self) -> String {
        rfc3339_utc(self.finished_at)
    }

    /// `duration` in whole milliseconds, for the contract's `duration_ms`.
    pub fn duration_ms(&self) -> u64 {
        u64::try_from(self.duration.as_millis()).unwrap_or(u64::MAX)
    }

    /// `time_limit` in whole seconds, rounded up, for `timeout_secs`.
    pub fn time_limit_secs(&self) -> u64 {
        self.time_limit.as_secs() + u64::from(self.time_limit.subsec_nanos() > 0)
    }
}

/// What a signal was aimed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignalTarget {
    /// The child's process group.
    Group,
    /// Tracked descendants, one by one.
    Descendants {
        /// How many were signalled.
        count: usize,
    },
}

/// One signal the owner delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalRecord {
    /// The signal name.
    pub signal: String,
    /// When it was delivered.
    pub at: SystemTime,
    /// What it was aimed at.
    pub target: SignalTarget,
}

/// Observations behind the report, for diagnosis. None of these change the
/// ending or the cleanup verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostics {
    /// Problems that did not stop the run, such as a failed process listing.
    pub warnings: Vec<String>,
    /// The signals delivered, in order, up to a bound.
    pub signals: Vec<SignalRecord>,
    /// Signals beyond the bound that were not recorded.
    pub signals_omitted: u64,
    /// Whether descendant tracking was on.
    pub tracking_enabled: bool,
    /// Process listing receipts.
    pub inspection: InspectionStats,
    /// True when the descendant picture may have a hole: the pre-stop scan ran
    /// over budget, a listing failed, tracking was off, or the child's own
    /// identity was never observed.
    pub capture_gap_observed: bool,
    /// A signal was delivered to the group after the leader had exited. It
    /// records cleanup activity, not proof of live descendants.
    pub post_exit_group_signal: bool,
    /// The owner closed its ends of the output pipes because something still
    /// held them after the kill and the drain bound.
    pub pipes_closed_by_owner: bool,
    /// Always true. The cleanup verdict covers the child's process group and
    /// the descendants that were observed. A descendant that was never
    /// observed and left the group is not covered. This is a known gap.
    pub unobserved_descendants_possible: bool,
}

/// The terminal report for one child. Every started child produces exactly
/// one, including when it failed to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessReport {
    /// The child's pid, when it started.
    pub pid: Option<u32>,
    /// How the run ended.
    pub ending: Ending,
    /// How the child itself ended, when known.
    pub exit: Option<LeaderExit>,
    /// Captured standard output.
    pub stdout: Captured,
    /// Captured standard error.
    pub stderr: Captured,
    /// Start, finish, duration and limit.
    pub timing: ProcessTiming,
    /// Whether every process the owner started is known to be gone, and if
    /// not, which pids and groups may remain and why. See
    /// [`Diagnostics::unobserved_descendants_possible`] for what `Confirmed`
    /// does not cover.
    pub cleanup: Cleanup,
    /// Observations behind the report.
    pub diagnostics: Diagnostics,
}

impl ProcessReport {
    /// The contract outcome of the ending.
    pub fn outcome(&self) -> Outcome {
        self.ending.outcome()
    }

    /// The contract error of the ending, absent for a clean exit.
    pub fn error_detail(&self) -> Option<ErrorDetail> {
        self.ending.error_detail()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_ending_maps_to_one_outcome() {
        let cases = [
            (Ending::Exited { code: 0 }, Outcome::Completed),
            (Ending::Exited { code: 3 }, Outcome::Failed),
            (
                Ending::Signaled {
                    signal: 11,
                    name: "SIGSEGV".into(),
                },
                Outcome::Failed,
            ),
            (
                Ending::TimedOut {
                    limit: Duration::from_secs(1),
                },
                Outcome::TimedOut,
            ),
            (Ending::Interrupted, Outcome::Interrupted),
            (
                Ending::FailedToStart {
                    message: "x".into(),
                },
                Outcome::Failed,
            ),
            (
                Ending::WaitFailed {
                    message: "x".into(),
                },
                Outcome::Failed,
            ),
        ];
        for (ending, outcome) in cases {
            assert_eq!(ending.outcome(), outcome, "{ending:?}");
        }
    }

    #[test]
    fn a_timeout_is_typed_as_timed_out_not_as_an_error_code() {
        let ending = Ending::TimedOut {
            limit: Duration::from_millis(1500),
        };
        let detail = ending.error_detail().expect("a timeout has an error");
        assert_eq!(detail.kind, "timed_out");
        assert_eq!(detail.provider_exit_code, None);
        assert!(detail.message.contains("1.5 s"), "{}", detail.message);
        assert_eq!(ending.outcome(), Outcome::TimedOut);
    }

    #[test]
    fn a_clean_exit_has_no_error_and_a_nonzero_exit_carries_its_code() {
        assert_eq!(Ending::Exited { code: 0 }.error_detail(), None);
        let detail = Ending::Exited { code: 7 }.error_detail().unwrap();
        assert_eq!(detail.kind, "process_exited");
        assert_eq!(detail.provider_exit_code, Some(7));
        assert_eq!(
            Ending::Interrupted.error_detail().unwrap().kind,
            "interrupted"
        );
    }

    #[test]
    fn timing_converts_to_the_contract_units() {
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        let timing = ProcessTiming {
            started_at: Some(at),
            finished_at: at + Duration::from_millis(2500),
            duration: Duration::from_millis(2500),
            time_limit: Duration::from_millis(1500),
        };
        assert_eq!(
            timing.started_at_rfc3339().as_deref(),
            Some("2001-09-09T01:46:40.000Z")
        );
        assert_eq!(timing.finished_at_rfc3339(), "2001-09-09T01:46:42.500Z");
        assert_eq!(timing.duration_ms(), 2500);
        assert_eq!(timing.time_limit_secs(), 2);
    }
}
