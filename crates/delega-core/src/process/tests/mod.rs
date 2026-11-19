//! Tests for the process layer, against small fake children (`/bin/sh`
//! scripts). No provider program is involved, and every test checks that no
//! process it started is left running.

/// Skip a test visibly when `perl` is missing. The tests that start a
/// descendant in its own process group or session need it, because `sh` cannot
/// do that portably (`set -m` is ignored by dash without a terminal).
macro_rules! require_perl {
    () => {
        if !super::fixtures::perl_available() {
            eprintln!(
                "SKIPPED: perl is not installed; this test starts a descendant in its own \
                 process group or session with it"
            );
            return;
        }
    };
}

mod cleanup;
mod endings;
mod fixtures;
mod gaps;
mod io;
mod ported;
mod stop;
mod tap;
