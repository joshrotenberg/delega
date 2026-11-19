//! The user-level configuration file, read for a command.
//!
//! [`delega_core::config`] is the specification and the reader; this is only
//! the environment it reads from, which is this process's.

use delega_core::config::Status;

/// Read the configuration file as the current user, from this process's
/// environment (`DELEGA_CONFIG`, `XDG_CONFIG_HOME`, `HOME`).
pub(crate) fn read() -> Status {
    delega_core::config::read(|name| std::env::var_os(name))
}
