//! Tests for the process layer, against small fake children (`/bin/sh`
//! scripts). No provider program is involved, and every test checks that no
//! process it started is left running.

/// Skip a test visibly when `perl` is missing. The tests that start a
/// descendant in its own process group or session need it, because `sh` cannot
/// do that portably (`set -m` is ignored by dash without a terminal). Under CI
/// (`CI` set, as GitHub Actions sets it) a missing perl fails the test
/// instead, so it is never reported as passed.
macro_rules! require_perl {
    () => {
        if !super::fixtures::perl_available() {
            assert!(
                std::env::var_os("CI").is_none(),
                "perl is not installed, and CI requires it for this test"
            );
            eprintln!(
                "SKIPPED: perl is not installed; this test starts a descendant in its own \
                 process group or session with it"
            );
            return;
        }
    };
}

/// One test body run twice: once with the child owned directly and once
/// through the test keeper (`fixtures::sh` reads which). The second test's name
/// ends in `_kept`.
macro_rules! both {
    ($direct:ident, $kept:ident, $body:expr) => {
        #[tokio::test]
        async fn $direct() {
            crate::process::tests::fixtures::KEEP
                .scope(false, $body)
                .await
        }
        #[tokio::test]
        async fn $kept() {
            crate::process::tests::fixtures::KEEP
                .scope(true, $body)
                .await
        }
    };
}

mod budgets;
mod cleanup;
mod endings;
mod environment;
mod fixtures;
mod gaps;
mod io;
mod keeper;
mod ported;
mod spawn_lock;
mod stop;
mod stop_wait;
mod tap;
