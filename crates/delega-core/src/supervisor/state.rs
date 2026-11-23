//! The states of a run and of a worker, and which changes between them are
//! allowed.
//!
//! The supervisor derives a worker's state from what the worker holds and then
//! applies it through [`WorkerState::transition`], so a change the table does
//! not allow is a defect that is caught, not a state that is silently reached.
//!
//! Run states and transitions:
//!
//! | From | To | When |
//! | --- | --- | --- |
//! | `queued` | `running` | a running slot is free and the run is next on its worker |
//! | `queued` | `finished` | interrupted, discarded by `close` or shutdown, or refused at dispatch |
//! | `running` | `finished` | the provider ended, or the run was stopped |
//!
//! `finished` is final. Worker states and transitions:
//!
//! | From | To | When |
//! | --- | --- | --- |
//! | (new) | `queued` or `running` | `start` admitted the worker |
//! | `queued` | `running` | its run was dispatched |
//! | `queued` | `idle` | its queued runs were all interrupted |
//! | `queued` | `closed` | `close` or shutdown |
//! | `running` | `queued` | the run finished, the next one waits for a running slot |
//! | `running` | `idle` | the run finished and nothing is queued |
//! | `running` | `closed` | `close` or shutdown |
//! | `idle` | `queued` | `follow_up` while no running slot is free |
//! | `idle` | `running` | `follow_up` with a running slot free |
//! | `idle` | `closed` | `close` or shutdown |
//!
//! `closed` is final.

use std::fmt;

use schemars::JsonSchema;
use serde::Serialize;

/// Where a run is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    /// Waiting behind another run on its worker, or for a running slot.
    Queued,
    /// The provider is running.
    Running,
    /// Ended, with exactly one result.
    Finished,
}

/// Where a worker is. Derived from its runs: see the module documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkerState {
    /// No run is running and at least one waits for a running slot.
    Queued,
    /// A run is running. More may be queued behind it.
    Running,
    /// The current run has a pending permission interaction.
    WaitingForAnswer,
    /// Nothing is running or queued. A follow-up can be given.
    Idle,
    /// Closed. Final. Its results stay readable for the retention time.
    Closed,
}

/// A change of state the table does not allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IllegalTransition<S> {
    /// The state it was in.
    pub from: S,
    /// The state it was asked to enter.
    pub to: S,
}

impl<S: fmt::Debug> fmt::Display for IllegalTransition<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "illegal transition from {:?} to {:?}",
            self.from, self.to
        )
    }
}

impl RunState {
    /// Whether the table allows `self` to become `to`.
    pub fn can_become(self, to: RunState) -> bool {
        use RunState::*;
        matches!(
            (self, to),
            (Queued, Running) | (Queued, Finished) | (Running, Finished)
        )
    }

    /// `to`, if the table allows the change.
    pub fn transition(self, to: RunState) -> Result<RunState, IllegalTransition<RunState>> {
        if self.can_become(to) {
            Ok(to)
        } else {
            Err(IllegalTransition { from: self, to })
        }
    }
}

impl WorkerState {
    /// Whether the table allows `self` to become `to`.
    pub fn can_become(self, to: WorkerState) -> bool {
        use WorkerState::*;
        matches!(
            (self, to),
            (Queued, Running)
                | (Queued, Idle)
                | (Queued, Closed)
                | (Running, Queued)
                | (Running, Idle)
                | (Running, Closed)
                | (Idle, Queued)
                | (Idle, Running)
                | (Idle, Closed)
        )
    }

    /// `to`, if the table allows the change.
    pub fn transition(
        self,
        to: WorkerState,
    ) -> Result<WorkerState, IllegalTransition<WorkerState>> {
        if self.can_become(to) {
            Ok(to)
        } else {
            Err(IllegalTransition { from: self, to })
        }
    }

    /// The state a worker with this content is in.
    pub fn derive(closed: bool, has_running: bool, queued: usize) -> WorkerState {
        if closed {
            WorkerState::Closed
        } else if has_running {
            WorkerState::Running
        } else if queued > 0 {
            WorkerState::Queued
        } else {
            WorkerState::Idle
        }
    }
}
