//! The operating system calls the process layer needs, through `rustix`.
//!
//! The workspace denies `unsafe`, so signals and non-reaping waits go through
//! a safe wrapper crate instead of `libc`; the one exception is `new_session`. Nothing else in the module touches
//! the OS about processes except `ps` in `tree.rs`.

use rustix::io::Errno;
use rustix::process::{Pid, WaitId, WaitIdOptions, WaitIdStatus};

use super::limits::EXIT_POLL_INTERVAL;

#[cfg(target_os = "macos")]
pub(super) mod darwin;

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

/// `kill(pid, signal)`. Not used on Linux outside tests: nothing is signalled
/// by number there (see [`signal_checked`]).
#[cfg_attr(target_os = "linux", allow(dead_code))]
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

/// How a child ended, as `waitid` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ExitSummary {
    /// The exit code, when it exited.
    pub code: Option<i32>,
    /// The signal that ended it, when one did.
    pub signal: Option<i32>,
}

fn summary(status: &WaitIdStatus) -> ExitSummary {
    ExitSummary {
        code: status.exit_status(),
        signal: status.terminating_signal(),
    }
}

/// Block until `child` has exited, without reaping it, and say how it ended.
/// As [`wait_for_exit`], a stop is not an ending.
pub(super) fn wait_for_exit_status(child: i32) -> Result<ExitSummary, String> {
    let Some(child) = Pid::from_raw(child) else {
        return Err(format!("invalid pid {child}"));
    };
    loop {
        match rustix::process::waitid(
            WaitId::Pid(child),
            WaitIdOptions::EXITED | WaitIdOptions::NOWAIT,
        ) {
            Ok(Some(status)) if is_ending(&status) => return Ok(summary(&status)),
            Ok(_) => std::thread::sleep(EXIT_POLL_INTERVAL),
            Err(Errno::INTR) => {}
            Err(errno) => return Err(std::io::Error::from(errno).to_string()),
        }
    }
}

/// Reap `child` if it has exited; `true` when it was reaped.
pub(super) fn reap_if_exited(child: i32) -> bool {
    let Some(child) = Pid::from_raw(child) else {
        return false;
    };
    let options = WaitIdOptions::EXITED | WaitIdOptions::NOHANG;
    matches!(
        rustix::process::waitid(WaitId::Pid(child), options),
        Ok(Some(status)) if is_ending(&status)
    )
}

/// Make the calling process the reaper of every orphaned descendant
/// (`prctl(PR_SET_CHILD_SUBREAPER)`). Linux only.
#[cfg(target_os = "linux")]
pub(super) fn become_subreaper() -> Result<(), String> {
    rustix::process::set_child_subreaper(Some(rustix::process::getpid()))
        .map_err(|errno| std::io::Error::from(errno).to_string())
}

/// What `/proc/<pid>/stat` says about one process (Linux).
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ProcStat {
    pub state: char,
    pub parent: i32,
    pub group: i32,
    pub session: i32,
    /// Start time in clock ticks after boot: with the pid, an identity that a
    /// reused pid does not share.
    pub start_ticks: u64,
}

/// Read `/proc/<pid>/stat` (Linux). `None` when the process is gone or the
/// line cannot be read; [`proc_stat_read`] tells the two apart.
#[cfg(target_os = "linux")]
pub(super) fn proc_stat(pid: i32) -> Option<ProcStat> {
    proc_stat_read(pid).ok().flatten()
}

/// A read of `/proc/<pid>/stat` that failed for a reason other than the
/// process being gone, or a line that could not be parsed.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct StatUnreadable;

/// Read `/proc/<pid>/stat` (Linux): `Ok(None)` only when the process is gone
/// (ENOENT or ESRCH), an error when the read failed for any other reason or
/// the line could not be parsed.
#[cfg(target_os = "linux")]
pub(super) fn proc_stat_read(pid: i32) -> Result<Option<ProcStat>, StatUnreadable> {
    if pid <= 0 {
        return Ok(None);
    }
    match std::fs::read(format!("/proc/{pid}/stat")) {
        Ok(bytes) => parse_proc_stat(&bytes).map(Some).ok_or(StatUnreadable),
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                || error.raw_os_error() == Some(Errno::SRCH.raw_os_error()) =>
        {
            Ok(None)
        }
        Err(_) => Err(StatUnreadable),
    }
}

/// Fields after the command name, counted from the last `)`: state (3), ppid
/// (4), pgrp (5), session (6), ..., starttime (22).
#[cfg(any(target_os = "linux", test))]
fn parse_proc_stat_fields(stat: &[u8]) -> Option<(char, i32, i32, i32, u64)> {
    let end = stat.iter().rposition(|&byte| byte == b')')?;
    let rest = std::str::from_utf8(&stat[end + 1..]).ok()?;
    let fields: Vec<&str> = rest.split_ascii_whitespace().collect();
    let state = fields.first()?.chars().next()?;
    let parent = fields.get(1)?.parse().ok()?;
    let group = fields.get(2)?.parse().ok()?;
    let session = fields.get(3)?.parse().ok()?;
    let start_ticks = fields.get(19)?.parse().ok()?;
    Some((state, parent, group, session, start_ticks))
}

#[cfg(target_os = "linux")]
fn parse_proc_stat(stat: &[u8]) -> Option<ProcStat> {
    let (state, parent, group, session, start_ticks) = parse_proc_stat_fields(stat)?;
    Some(ProcStat {
        state,
        parent,
        group,
        session,
        start_ticks,
    })
}

/// Whether the process behind a pidfd has exited (Linux): a pidfd becomes
/// readable when its process exits. A poll that fails counts as exited, so a
/// caller that relies on the process being alive does not.
#[cfg(target_os = "linux")]
pub(super) fn pidfd_exited(fd: &std::os::fd::OwnedFd) -> bool {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    let mut fds = [PollFd::new(fd, PollFlags::IN)];
    let now = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    !matches!(poll(&mut fds, Some(&now)), Ok(0))
}

/// Whether `target` names no live process (Linux): no process holds the pid,
/// or the one holding it has exited (its pidfd is readable). Used where a
/// process could not be verified: if whatever holds the pid now has exited,
/// the process that was found has exited too (it held the pid then, and a pid
/// is reused only after its holder is reaped), so there is nothing to signal.
/// A pidfd that cannot be opened for another reason, or a poll that fails,
/// is not taken as gone.
#[cfg(target_os = "linux")]
pub(super) fn pid_gone(target: i32) -> bool {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    use rustix::process::{PidfdFlags, pidfd_open};
    let Ok(raw) = pid(target) else {
        return false;
    };
    match pidfd_open(raw, PidfdFlags::empty()) {
        Err(Errno::SRCH) => true,
        Err(_) => false,
        Ok(fd) => {
            let mut fds = [PollFd::new(&fd, PollFlags::IN)];
            let now = Timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            matches!(poll(&mut fds, Some(&now)), Ok(1)) && fds[0].revents().contains(PollFlags::IN)
        }
    }
}

/// The start time of a process in clock ticks (Linux), for checking at
/// delivery that a pid still names the process it named when it was found.
#[cfg(target_os = "linux")]
pub(super) fn start_ticks(pid: i32) -> Option<u64> {
    proc_stat(pid).map(|s| s.start_ticks)
}

/// The start time of a process in clock ticks; only Linux can say.
#[cfg(not(target_os = "linux"))]
pub(super) fn start_ticks(_pid: i32) -> Option<u64> {
    None
}

/// What became of one checked signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Delivery {
    /// The signal was sent to the process meant.
    Sent,
    /// The process meant is gone: no such process, or the pid now names
    /// another one (a different start time, group or session than the one
    /// it was found with).
    Gone,
    /// The process could not be verified (Linux: no pidfd, or no identity to
    /// check against), so it was not signalled. A verdict that depended on
    /// signalling it cannot be `Confirmed`.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Refused,
}

/// Signal one process only if it is still the one meant.
///
/// On Linux the process is first held by a pidfd (`pidfd_open`), then its
/// `/proc/<pid>/stat` is read and given to `check`, and the signal goes
/// through the pidfd (`pidfd_send_signal`): once the pidfd is open its number
/// can no longer name another process, so a pid reused between the check and
/// the signal is never signalled. Nothing is ever signalled by pid number on
/// Linux: without a pidfd (a kernel before 5.3, or a refusal) the process is
/// [`Delivery::Refused`].
///
/// Elsewhere there is nothing to read the identity from at delivery: `check`
/// is not called and the signal goes by pid, as the callers' own listing-time
/// checks decided (the residual in the module documentation).
#[cfg(target_os = "linux")]
pub(super) fn signal_checked(
    target: i32,
    signal: Signal,
    check: impl FnOnce(&ProcStat) -> bool,
) -> Result<Delivery, SignalError> {
    use rustix::process::{PidfdFlags, pidfd_open, pidfd_send_signal};
    let pid = pid(target)?;
    let fd = match pidfd_open(pid, PidfdFlags::empty()) {
        Ok(fd) => fd,
        Err(Errno::SRCH) => return Ok(Delivery::Gone),
        Err(_) => return Ok(Delivery::Refused),
    };
    // Held but unreadable: gone only if the pidfd says it exited; otherwise
    // it could not be verified.
    let Some(stat) = proc_stat(target) else {
        return Ok(if pidfd_exited(&fd) {
            Delivery::Gone
        } else {
            Delivery::Refused
        });
    };
    if !check(&stat) {
        return Ok(Delivery::Gone);
    }
    match pidfd_send_signal(&fd, signal) {
        Ok(()) => Ok(Delivery::Sent),
        Err(Errno::SRCH) => Ok(Delivery::Gone),
        Err(errno) => Err(SignalError::from(errno)),
    }
}

/// See the Linux version: here `check` is not called.
#[cfg(not(target_os = "linux"))]
pub(super) fn signal_checked(
    target: i32,
    signal: Signal,
    _check: impl FnOnce(&()) -> bool,
) -> Result<Delivery, SignalError> {
    match signal_process(target, signal) {
        Ok(()) => Ok(Delivery::Sent),
        Err(SignalError::NoSuchProcess) => Ok(Delivery::Gone),
        Err(error) => Err(error),
    }
}

/// The session id of a process, when it can be read.
#[cfg(test)]
pub(super) fn session_of(target: i32) -> Option<i32> {
    match session_lookup(target) {
        SessionLookup::In(session) => Some(session),
        SessionLookup::Gone | SessionLookup::Unknown => None,
    }
}

/// What the OS said about the session of one process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SessionLookup {
    /// The process is in this session. On Linux this is 0 for a kernel thread
    /// and for a process whose session leader is outside this pid namespace;
    /// a 0 is a real answer that never names the child's session.
    In(i32),
    /// The process no longer exists (ESRCH, or no `/proc` entry).
    Gone,
    /// The OS would not say.
    Unknown,
}

/// The session of a process, telling a vanished process from an unreadable one.
///
/// Linux reads `/proc/<pid>/stat`, because `getsid(2)` answers 0 there for
/// kernel threads and for processes whose session leader is outside the pid
/// namespace, and rustix turns the answer into a `Pid` that cannot be 0
/// (a panic in a debug build, undefined behaviour in a release build). Other
/// systems call `getsid` through rustix. On macOS `ps -ax` lists no pid 0 row
/// and `getsid` of every listed process was non-zero when checked; pid 0 is
/// refused above, so a zero answer is not expected there.
pub(super) fn session_lookup(target: i32) -> SessionLookup {
    if target <= 0 {
        return SessionLookup::Unknown;
    }
    platform_session(target)
}

#[cfg(target_os = "linux")]
fn platform_session(target: i32) -> SessionLookup {
    match std::fs::read(format!("/proc/{target}/stat")) {
        Ok(bytes) => parse_stat_session(&bytes).map_or(SessionLookup::Unknown, SessionLookup::In),
        // ESRCH: the process exited while its proc entry was being read.
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                || error.raw_os_error() == Some(Errno::SRCH.raw_os_error()) =>
        {
            SessionLookup::Gone
        }
        Err(_) => SessionLookup::Unknown,
    }
}

#[cfg(not(target_os = "linux"))]
fn platform_session(target: i32) -> SessionLookup {
    let Some(target) = Pid::from_raw(target) else {
        return SessionLookup::Unknown;
    };
    match rustix::process::getsid(Some(target)) {
        Ok(pid) => SessionLookup::In(pid.as_raw_nonzero().get()),
        Err(Errno::SRCH) => SessionLookup::Gone,
        Err(_) => SessionLookup::Unknown,
    }
}

/// The session field of one `/proc/<pid>/stat` line: the sixth field. The
/// command name (field 2) is in parentheses and may hold spaces, parentheses
/// and any bytes, so fields are counted from the last `)`.
#[cfg(any(target_os = "linux", test))]
fn parse_stat_session(stat: &[u8]) -> Option<i32> {
    let end = stat.iter().rposition(|&byte| byte == b')')?;
    let rest = std::str::from_utf8(&stat[end + 1..]).ok()?;
    // After the name: state, ppid, pgrp, session.
    let session: i32 = rest.split_ascii_whitespace().nth(3)?.parse().ok()?;
    (session >= 0).then_some(session)
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
/// Used for the provider and for the keeper alike.
///
/// This is the only `unsafe` block in production code (the tests of
/// `delega-mcp` have one more, for `mkfifo`): `Command::pre_exec` is unsafe
/// because the closure runs in the forked child before `exec`.
#[allow(unsafe_code)]
pub(super) fn new_session(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: the closure calls only `setsid(2)`, which is async-signal-safe,
    // through rustix (a direct syscall on Linux, libc's `setsid` wrapper on
    // macOS and other systems, which is async-signal-safe as POSIX requires). It allocates nothing, takes no locks and
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
    fn parses_the_session_of_a_stat_line() {
        let line = b"1234 (sleep) S 1 1234 1234 34816 1234 4194304 100 0 0 0 0 0";
        assert_eq!(parse_stat_session(line), Some(1234));
    }

    #[test]
    fn parses_the_session_when_the_name_has_spaces_and_parentheses() {
        let line = b"77 (a) b (c d) ) Z 5 66 88 0 -1 0";
        assert_eq!(parse_stat_session(line), Some(88));
        let line = b"2 (kthreadd) S 0 0 0 0 -1 2129984 0";
        assert_eq!(parse_stat_session(line), Some(0));
    }

    #[test]
    fn rejects_stat_lines_that_cannot_be_read() {
        assert_eq!(parse_stat_session(b""), None);
        assert_eq!(parse_stat_session(b"1 (x"), None);
        assert_eq!(parse_stat_session(b"1 (x) S 1 1"), None);
        assert_eq!(parse_stat_session(b"1 (x) S 1 1 abc 0"), None);
        assert_eq!(parse_stat_session(b"1 (x) S 1 1 -4 0"), None);
        assert_eq!(parse_stat_session(b"1 (x\xff) S 1 1 7 0"), Some(7));
    }

    #[test]
    fn a_stat_line_gives_state_parent_group_session_and_start_ticks() {
        // pid (comm) state ppid pgrp session tty tpgid flags minflt cminflt
        // majflt cmajflt utime stime cutime cstime priority nice threads
        // itrealvalue starttime ...
        let line = b"77 (a) b (c d) ) S 5 66 88 0 -1 0 1 2 3 4 5 6 7 8 20 0 1 0 123456 9";
        assert_eq!(
            parse_proc_stat_fields(line),
            Some(('S', 5, 66, 88, 123_456))
        );
        assert_eq!(parse_proc_stat_fields(b"1 (x) S 1 1 1"), None);
    }

    #[test]
    fn pid_zero_and_negative_pids_have_no_readable_session() {
        assert_eq!(session_lookup(0), SessionLookup::Unknown);
        assert_eq!(session_lookup(-1), SessionLookup::Unknown);
    }

    /// Not run on macOS; runs in the Linux CI job.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_kernel_thread_session_reads_without_panicking() {
        // Pid 2 is kthreadd; any other session-0 process found works too.
        let candidate = std::iter::once(2).chain(
            std::fs::read_dir("/proc")
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|entry| entry.file_name().to_str()?.parse::<i32>().ok()),
        );
        let mut seen = false;
        for pid in candidate {
            let Ok(bytes) = std::fs::read(format!("/proc/{pid}/stat")) else {
                continue;
            };
            if parse_stat_session(&bytes) == Some(0) {
                assert_eq!(session_lookup(pid), SessionLookup::In(0));
                seen = true;
                break;
            }
        }
        if !seen {
            eprintln!("no session-0 process visible; skipped");
        }
    }

    /// Not run on macOS; runs in the Linux CI job.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_missing_process_is_gone_on_linux() {
        assert_eq!(session_lookup(i32::MAX - 1), SessionLookup::Gone);
        // A live process reads as a session. It may be 0 when the test's
        // session leader is outside its pid namespace (a container), so only
        // the kind of answer is checked.
        assert!(matches!(
            session_lookup(std::process::id() as i32),
            SessionLookup::In(_)
        ));
    }

    /// A live process is never taken as gone, an exited one that is not yet
    /// reaped is, and so is a pid nothing holds. Not run on macOS; runs in the
    /// Linux CI job.
    #[cfg(target_os = "linux")]
    #[test]
    fn only_a_pid_that_names_no_live_process_is_gone() {
        use crate::test_spawn::Guarded;
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .guarded_spawn()
            .expect("start a child");
        let pid = i32::try_from(child.id()).expect("a pid");
        let alive = pid_gone(pid);
        let _ = child.kill();
        // Exited but not reaped: a zombie whose pidfd is readable.
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !leader_has_exited(pid) && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let zombie = pid_gone(pid);
        let _ = child.wait();
        assert!(!alive, "a live process was taken as gone");
        assert!(zombie, "an exited process was not taken as gone");
        assert!(pid_gone(i32::MAX - 1));
        assert!(!pid_gone(std::process::id() as i32));
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
