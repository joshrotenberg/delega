//! The closed set of ways a run ends.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// How a run ended. Every run ends in exactly one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The provider finished the assignment and returned a final answer.
    Completed,
    /// The provider or delega hit an error; `error.kind` says which.
    Failed,
    /// The run's time limit passed.
    TimedOut,
    /// The caller stopped the run, or went away.
    Interrupted,
    /// The run did not go ahead: invalid request, a limit, or a denied
    /// permission that stopped it.
    Refused,
}

impl Outcome {
    /// Every outcome, in the order the design document lists them.
    pub const ALL: [Outcome; 5] = [
        Outcome::Completed,
        Outcome::Failed,
        Outcome::TimedOut,
        Outcome::Interrupted,
        Outcome::Refused,
    ];

    /// The process exit status the command line uses for this outcome.
    pub const fn exit_status(self) -> u8 {
        match self {
            Outcome::Completed => 0,
            Outcome::Failed => 1,
            Outcome::Refused => 2,
            Outcome::TimedOut => 124,
            Outcome::Interrupted => 130,
        }
    }
}
