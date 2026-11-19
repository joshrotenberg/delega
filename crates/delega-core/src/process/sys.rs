//! The operating system calls the process layer needs, through `rustix`.
//!
//! The workspace denies `unsafe`, so signals and non-reaping waits go through
//! a safe wrapper crate instead of `libc`; the one exception is `new_session`. Nothing else in the module touches
//! the OS about processes except `ps` in `tree.rs`.

use rustix::io::Errno;
use rustix::process::{Pid, WaitId, WaitIdOptions, WaitIdStatus};

use super::limits::EXIT_POLL_INTERVAL;

pub(super) use rustix::process::Signal;

/// Why a signal was not delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SignalError {
    /// No such process or group (ESRCH).
    NoSuchProcess,
    /// The OS refused (EPERM). On macOS this is also the answer for a group
    /// that holds only zombies.
    PermissionDenied,
    /// Anything else, as text.
    Other(String),
}

impl From<Errno> for SignalError {
    fn from(errno: Errno) -> Self {
        if errno == Errno::SRCH {
            SignalError::NoSuchProcess
        } else if errno == Errno::PERM {
            SignalError::PermissionDenied
        } else {
            SignalError::Other(std::io::Error::from(errno).to_string())
        }
    }
}

fn pid(raw: i32) -> Result<Pid, SignalError> {
    Pid::from_raw(raw).ok_or_else(|| SignalError::Other(format!("invalid pid {raw}")))
}

/// `kill(-pgid, signal)`.
pub(super) fn signal_group(pgid: i32, signal: Signal) -> Result<(), SignalError> {
    rustix::process::kill_process_group(pid(pgid)?, signal).map_err(SignalError::from)
}

/// `kill(pid, signal)`.
pub(super) fn signal_process(target: i32, signal: Signal) -> Result<(), SignalError> {
    rustix::process::kill_process(pid(target)?, signal).map_err(SignalError::from)
}

/// Whether a wait status is an ending. A stopped or continued child is still
/// running: `waitid` with `WEXITED | WNOWAIT` reports a stop on macOS.
fn is_ending(status: &WaitIdStatus) -> bool {
    status.exited() || status.killed() || status.dumped()
}

/// Whether `child` has exited, without reaping it (`waitid` with `WNOWAIT`).
/// A child that is only stopped has not exited.
pub(super) fn leader_has_exited(child: i32) -> bool {
    let Some(child) = Pid::from_raw(child) else {
        return false;
    };
    let options = WaitIdOptions::EXITED | WaitIdOptions::NOWAIT | WaitIdOptions::NOHANG;
    matches!(
        rustix::process::waitid(WaitId::Pid(child), options),
        Ok(Some(status)) if is_ending(&status)
    )
}

/// Block until `child` has exited, without reaping it. Used on a dedicated
/// thread so the owner never waits behind process inspection. A stopped child
/// keeps the thread polling, since the stop is reported again at once.
pub(super) fn wait_for_exit(child: i32) -> Result<(), String> {
    let Some(child) = Pid::from_raw(child) else {
        return Err(format!("invalid pid {child}"));
    };
    loop {
        match rustix::process::waitid(
            WaitId::Pid(child),
            WaitIdOptions::EXITED | WaitIdOptions::NOWAIT,
        ) {
            Ok(Some(status)) if is_ending(&status) => return Ok(()),
            Ok(_) => std::thread::sleep(EXIT_POLL_INTERVAL),
            Err(Errno::INTR) => {}
            Err(errno) => return Err(std::io::Error::from(errno).to_string()),
        }
    }
}

/// The session id of a process, when it can be read.
#[cfg(test)]
pub(super) fn session_of(target: i32) -> Option<i32> {
    let target = Pid::from_raw(target)?;
    rustix::process::getsid(Some(target))
        .ok()
        .map(|pid| pid.as_raw_nonzero().get())
}

/// What `getsid` said about one process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SessionLookup {
    /// The process is in this session.
    In(i32),
    /// The process no longer exists (ESRCH).
    Gone,
    /// The OS would not say.
    Unknown,
}

/// The session of a process, telling a vanished process from an unreadable one.
pub(super) fn session_lookup(target: i32) -> SessionLookup {
    let Some(target) = Pid::from_raw(target) else {
        return SessionLookup::Unknown;
    };
    match rustix::process::getsid(Some(target)) {
        Ok(pid) => SessionLookup::In(pid.as_raw_nonzero().get()),
        Err(Errno::SRCH) => SessionLookup::Gone,
        Err(_) => SessionLookup::Unknown,
    }
}

/// What `kill(-pgid, 0)` says about a process group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GroupProbe {
    /// No process has this group id.
    Empty,
    /// A process exists in the group, or the OS would not say (EPERM).
    Present,
}

/// Probe a process group without signalling it.
pub(super) fn probe_group(pgid: i32) -> GroupProbe {
    let Some(pgid) = Pid::from_raw(pgid) else {
        return GroupProbe::Present;
    };
    match rustix::process::test_kill_process_group(pgid) {
        Err(Errno::SRCH) => GroupProbe::Empty,
        _ => GroupProbe::Present,
    }
}

/// Make the child lead a new session, so it has no controlling terminal and
/// every descendant that does not call `setsid` itself shares its session id.
///
/// This is the only `unsafe` in the workspace: `Command::pre_exec` is unsafe
/// because the closure runs in the forked child before `exec`.
#[allow(unsafe_code)]
pub(super) fn new_session(command: &mut tokio::process::Command) {
    // SAFETY: the closure calls only `setsid(2)`, which is async-signal-safe,
    // through rustix's direct syscall. It allocates nothing, takes no locks and
    // touches no state of the parent; a failure becomes an `io::Error` built
    // from the raw errno, which does not allocate either. Nothing else of ours
    // runs between fork and exec (std does its own setup there, outside this
    // closure).
    unsafe {
        command.pre_exec(|| {
            rustix::process::setsid()
                .map(|_| ())
                .map_err(std::io::Error::from)
        });
    }
}

const NAMES: [(Signal, &str); 24] = [
    (Signal::HUP, "SIGHUP"),
    (Signal::INT, "SIGINT"),
    (Signal::QUIT, "SIGQUIT"),
    (Signal::ILL, "SIGILL"),
    (Signal::TRAP, "SIGTRAP"),
    (Signal::ABORT, "SIGABRT"),
    (Signal::BUS, "SIGBUS"),
    (Signal::FPE, "SIGFPE"),
    (Signal::KILL, "SIGKILL"),
    (Signal::USR1, "SIGUSR1"),
    (Signal::SEGV, "SIGSEGV"),
    (Signal::USR2, "SIGUSR2"),
    (Signal::PIPE, "SIGPIPE"),
    (Signal::ALARM, "SIGALRM"),
    (Signal::TERM, "SIGTERM"),
    (Signal::CHILD, "SIGCHLD"),
    (Signal::CONT, "SIGCONT"),
    (Signal::STOP, "SIGSTOP"),
    (Signal::TSTP, "SIGTSTP"),
    (Signal::TTIN, "SIGTTIN"),
    (Signal::TTOU, "SIGTTOU"),
    (Signal::URG, "SIGURG"),
    (Signal::XCPU, "SIGXCPU"),
    (Signal::XFSZ, "SIGXFSZ"),
];

/// The conventional name of a signal number, or `SIG<n>` when unknown.
pub(super) fn signal_name(number: i32) -> String {
    Signal::from_named_raw(number)
        .and_then(|signal| NAMES.iter().find(|(s, _)| *s == signal))
        .map_or_else(|| format!("SIG{number}"), |(_, name)| (*name).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_known_and_unknown_signals() {
        assert_eq!(signal_name(Signal::KILL.as_raw()), "SIGKILL");
        assert_eq!(signal_name(Signal::TERM.as_raw()), "SIGTERM");
        assert_eq!(signal_name(9999), "SIG9999");
    }

    #[test]
    fn a_pid_that_is_not_our_child_has_not_exited() {
        assert!(!leader_has_exited(0));
        assert!(!leader_has_exited(i32::MAX));
    }

    #[test]
    fn signalling_a_missing_group_reports_no_such_process() {
        assert_eq!(
            signal_group(i32::MAX - 1, Signal::TERM),
            Err(SignalError::NoSuchProcess)
        );
    }
}
