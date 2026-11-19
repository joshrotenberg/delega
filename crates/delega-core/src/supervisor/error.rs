//! Errors a supervisor operation can return.
//!
//! A run that cannot go ahead is not an error: it is a run that ends
//! `refused`, and `read` returns its result. An error is a call that cannot be
//! answered at all.

use std::fmt;

use serde::Serialize;

/// Why a call could not be answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// No run has this id, or its result was kept past the retention bound.
    UnknownRun(String),
    /// No worker has this id, or it was closed and its results have gone.
    UnknownWorker(String),
    /// The arguments do not make sense together, such as `wait` with nothing
    /// to wait for. The text says what to change.
    InvalidArguments(String),
    /// The supervisor was shut down and takes no new work.
    ShutDown,
    /// The supervisor's task has ended, so nothing can answer.
    Stopped,
    /// The supervisor's own task failed. The text says what happened to the
    /// providers it was running.
    Failed(String),
}

/// A stable label for an [`Error`], for a surface that returns it as data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// See [`Error::UnknownRun`].
    UnknownRun,
    /// See [`Error::UnknownWorker`].
    UnknownWorker,
    /// See [`Error::InvalidArguments`].
    InvalidArguments,
    /// See [`Error::ShutDown`].
    ShutDown,
    /// See [`Error::Stopped`].
    Stopped,
    /// See [`Error::Failed`].
    Failed,
}

impl Error {
    /// The label for this error.
    pub fn kind(&self) -> ErrorKind {
        match self {
            Error::UnknownRun(_) => ErrorKind::UnknownRun,
            Error::UnknownWorker(_) => ErrorKind::UnknownWorker,
            Error::InvalidArguments(_) => ErrorKind::InvalidArguments,
            Error::ShutDown => ErrorKind::ShutDown,
            Error::Stopped => ErrorKind::Stopped,
            Error::Failed(_) => ErrorKind::Failed,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::UnknownRun(id) => write!(
                f,
                "no run {id}: the id is wrong, or its result was kept for the retention time \
                 and has gone"
            ),
            Error::UnknownWorker(id) => write!(
                f,
                "no worker {id}: the id is wrong, or the worker was closed and its results \
                 have gone"
            ),
            Error::InvalidArguments(why) => f.write_str(why),
            Error::ShutDown => f.write_str("the supervisor was shut down and takes no new work"),
            Error::Stopped => f.write_str("the supervisor has stopped"),
            Error::Failed(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for Error {}
