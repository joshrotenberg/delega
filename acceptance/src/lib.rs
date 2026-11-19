//! Black-box acceptance measurement of the built `delega` binary.
//!
//! It measures the first two acceptance criteria of the README: every run
//! ends in exactly one stated outcome, and no process is left behind or the
//! result says what may remain. See `acceptance/README.md`.

pub mod delega_bin;
pub mod matrix;
pub mod procs;
pub mod report;
pub mod run_all;
pub mod runner;
