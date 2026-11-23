//! The keeper: a small process between the owner and the program.
//!
//! The owner starts the keeper, and the keeper starts the program as its own
//! child, in a session of the program's own exactly as the owner would have.
//! The keeper is delega's answer to the cleanup gaps a process table cannot
//! close (see the module documentation of [`crate::process`]):
//!
//! - On Linux the keeper is a child subreaper (`PR_SET_CHILD_SUBREAPER`), so a
//!   descendant of the program whose parent exits becomes a child of the keeper
//!   instead of init. Ancestry from the keeper reaches every live descendant,
//!   and the keeper can stop its whole tree by walking `/proc` without `ps`.
//! - On every platform the keeper holds the program unreaped until the owner
//!   says `reap`, so the program's pid keeps naming its process group and
//!   session while the owner signals them, as it did when the program was the
//!   owner's own child. It tells the owner how the program ended.
//! - The keeper's standard input is one end of a socket pair whose other end
//!   only the owner holds, close-on-exec. When that end closes (the owner
//!   exited, crashed or was killed with SIGKILL) the keeper stops the whole
//!   tree by itself, TERM then KILL, and exits. It does the same on SIGTERM,
//!   SIGINT, SIGHUP or SIGQUIT, when its parent changes, and at a ceiling of
//!   the time limit plus the cleanup deadline plus
//!   [`KEEPER_CEILING_MARGIN`](super::limits::KEEPER_CEILING_MARGIN).
//!
//! The library does not start a keeper of its own: the caller names a program
//! that runs [`keeper_main`] in a [`KeeperCommand`], as with the command guard.
//! The `delega` binary does this with a hidden subcommand. Without one a run is
//! owned as before, and its report says so (`diagnostics.keeper` is `None`).
//!
//! # Protocol
//!
//! The owner writes one hello: `DKH1`, a little-endian `u32` body length, and
//! records of a tag byte, a little-endian `u32` length and that many bytes (the
//! program, each argument, each `NAME=VALUE` of the environment, the working
//! directory, the stop grace, the cleanup deadline and the ceiling in
//! milliseconds, and the marker's value). The program's standard input (when
//! it has one), output and error travel with the hello as `SCM_RIGHTS`, so the
//! owner keeps the other ends exactly as it does for a child of its own. After
//! that both sides write lines:
//!
//! - keeper: `subreaper on`, `subreaper off <why>`, `started <pid>`,
//!   `spawn-failed <errno> <message>`, `exited code <n>`, `exited signal <n>`,
//!   `tree <id> <live> [<details>]`, `reaped <live> [proven]` (`proven` when
//!   the final census before the reap proved the tree empty, Linux with a
//!   subreaper only), `releasing`, `warn <text>`.
//! - owner: `census <id>` (count the live processes in the keeper's tree),
//!   `freeze <id>` (stop, kill and count them), `reap`, `released`.

mod link;
mod protocol;
#[cfg(target_os = "macos")]
mod registry;
mod run;
#[cfg(target_os = "macos")]
mod track;
#[cfg(all(test, target_os = "macos"))]
pub(crate) use track::NO_TRACKING_ENV;
#[cfg(target_os = "linux")]
mod walk;

use std::ffi::OsString;
use std::path::PathBuf;

#[cfg(test)]
pub(crate) use link::reply_unproven as link_reply_unproven;
pub(crate) use link::{KeeperLink, LaunchSpec, LinkHandle, StartError, start};

/// Test seam: a keeper started by the tests with this variable makes that many
/// of its reads of `/proc` fail once the owner says `reap` (Linux), so no
/// answer reports them and its final census proves nothing.
#[cfg(test)]
pub(crate) const INJECT_READ_FAILURES_ENV: &str = "DELEGA_TEST_KEEPER_FAIL_READS";

/// Test seam: a macOS keeper started by the tests with this variable reports
/// its final census before the reap as having met one process it could not
/// prove either way, as when an orphan is first seen there.
#[cfg(all(test, target_os = "macos"))]
pub(crate) const AMBIGUOUS_AT_REAP_ENV: &str = "DELEGA_TEST_KEEPER_AMBIGUOUS_AT_REAP";

/// Test seam: a keeper started by the tests with this variable holds every
/// freeze of its tree (Linux) until the file it names exists, so a test can
/// order the keeper's freeze after an event of the owner. After 60 s it fails
/// closed: no freeze runs, and the keeper writes the marker
/// [`gate_expired_marker`] names, which the tests assert absent.
#[cfg(test)]
pub(crate) const FREEZE_GATE_ENV: &str = "DELEGA_TEST_KEEPER_FREEZE_GATE";
#[cfg(test)]
pub(crate) use run::gate_expired_marker;
pub use run::keeper_main;

/// A program and arguments that run [`keeper_main`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeeperCommand {
    /// The program, an absolute path.
    pub program: PathBuf,
    /// Arguments that make the program run [`keeper_main`].
    pub args: Vec<String>,
    /// The keeper's own environment. It is started with this and nothing
    /// else; the program it starts gets the run's environment, not this.
    pub env: Vec<(OsString, OsString)>,
    /// The keeper registry (macOS): a directory, owned by the user with mode
    /// 0700, where each keeper publishes the unique ids of the processes it
    /// took, so that one run does not take another run's orphans for its own.
    /// It must not be writable by a provider's build worker (not a temporary
    /// directory). `None`: no registry, and the macOS keeper counts every
    /// orphan of a parent created during the run as possibly its own.
    pub registry: Option<PathBuf>,
}

impl KeeperCommand {
    /// A keeper command from a program and its arguments.
    pub fn new<I, S>(program: impl Into<PathBuf>, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
            env: Vec::new(),
            registry: None,
        }
    }

    /// Use this directory as the keeper registry (see
    /// [`KeeperCommand::registry`]).
    #[must_use]
    pub fn registry(mut self, dir: impl Into<PathBuf>) -> Self {
        self.registry = Some(dir.into());
        self
    }

    /// Add a variable to the keeper's own environment.
    #[must_use]
    pub fn env(mut self, name: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((name.into(), value.into()));
        self
    }
}
