//! The macOS calls the keeper's fork tracker needs: libproc and `kqueue`.
//!
//! This module and `new_session` in `sys.rs` are the only places with
//! `unsafe` code in production. There are five `unsafe` blocks here, one per
//! foreign call (`proc_pidinfo`, `proc_listpids`, `sysctl`,
//! `proc_signal_with_audittoken`, `kevent`), and one declaration of a
//! function libc does not bind. Each block is in a function with a safe
//! signature and its own SAFETY comment; everything else here is safe code on
//! top of them.
//!
//! Identity on macOS: the kernel gives every process a 64-bit unique id
//! (`p_uniqueid`) that is never reused while the system runs, records the
//! unique id of the parent that forked it (`p_puniqueid`), which does not
//! change when the process is reparented to launchd, and keeps a pid version
//! (`p_idversion`) that a signal by audit token is checked against. All three
//! come from `proc_pidinfo` flavor 17 (`PROC_PIDUNIQIDENTIFIERINFO`), which is
//! not in the public SDK headers. The keeper checks at start that it answers
//! as expected ([`self_test`]) and does not track forks when it does not.

use std::os::fd::OwnedFd;
use std::time::Duration;

use rustix::event::kqueue::{Event, EventFilter, EventFlags, ProcessEvents};
use rustix::io::Errno;
use rustix::process::{Pid, Signal};

/// `PROC_PIDUNIQIDENTIFIERINFO` from xnu's private `proc_info.h`.
const PROC_PIDUNIQIDENTIFIERINFO: i32 = 17;
/// `proc_listpids` kinds from `sys/proc_info.h`.
const PROC_ALL_PIDS: u32 = 1;
const PROC_UID_ONLY: u32 = 4;
const PROC_PPID_ONLY: u32 = 6;
/// Size of `struct proc_uniqidentifierinfo`: `uint8_t p_uuid[16]`,
/// `uint64_t p_uniqueid`, `uint64_t p_puniqueid`, `int32_t p_idversion`,
/// `int32_t p_orig_ppidversion`, `uint64_t p_reserve2`, `uint64_t p_reserve3`.
const UNIQ_INFO_SIZE: usize = 56;
const UNIQ_OFFSET_UNIQUEID: usize = 16;
const UNIQ_OFFSET_PUNIQUEID: usize = 24;
const UNIQ_OFFSET_IDVERSION: usize = 32;

const BSD_INFO_SIZE: usize = std::mem::size_of::<libc::proc_bsdinfo>();

/// Size of `struct kinfo_proc` on 64-bit macOS, which libc does not define.
/// It begins with `struct extern_proc`, which begins with a union whose
/// `struct timeval __p_starttime` (`tv_sec` an `i64`, `tv_usec` an `i32` at
/// byte 8) is the start time.
const KINFO_PROC_SIZE: usize = 648;
const KINFO_OFFSET_START_SEC: usize = 0;
const KINFO_OFFSET_START_USEC: usize = 8;
/// `int p_flag` in `struct extern_proc`, and its `P_WEXIT` bit: the process
/// is exiting.
const KINFO_OFFSET_FLAG: usize = 32;
const P_WEXIT: u32 = 0x2000;
/// `char p_stat` in `struct extern_proc`.
const KINFO_OFFSET_STAT: usize = 36;
/// `uid_t p_ruid` in `kp_eproc.e_pcred`, `pid_t e_ppid` and `pid_t e_pgid`
/// in `kp_eproc`.
const KINFO_OFFSET_RUID: usize = 392;
const KINFO_OFFSET_PPID: usize = 560;
const KINFO_OFFSET_PGID: usize = 564;
/// `p_stat` of a zombie (`SZOMB`).
const SZOMB: u8 = 5;

/// `audit_token_t`: eight 32-bit words; word 5 is the pid and word 7 the pid
/// version, the two the kernel checks before it delivers a signal.
#[repr(C)]
struct AuditToken {
    val: [u32; 8],
}

// `proc_signal_with_audittoken` is declared in the public `libproc.h` (macOS
// 11 and later) and exported by libSystem, which every program links; libc
// does not bind it.
#[allow(unsafe_code)]
unsafe extern "C" {
    fn proc_signal_with_audittoken(token: *mut AuditToken, sig: libc::c_int) -> libc::c_int;
}

fn last_errno() -> Errno {
    Errno::from_raw_os_error(std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
}

/// `proc_pidinfo(pid, flavor, 0, buf, buf.len())`. `Ok` only when the kernel
/// filled the whole buffer, which is how it answers for the fixed-size
/// flavors used here; ESRCH means no such process, or a zombie (macOS answers
/// every flavor of a zombie with ESRCH).
#[allow(unsafe_code)]
fn pidinfo(pid: i32, flavor: i32, buf: &mut [u8]) -> Result<(), Errno> {
    let len = libc::c_int::try_from(buf.len()).map_err(|_| Errno::INVAL)?;
    // SAFETY: `buf` is a live, writable byte slice of exactly `len` bytes for
    // the duration of the call, and the kernel writes at most `buffersize`
    // bytes into it. Any byte pattern is a valid `u8`, so whatever it writes
    // leaves `buf` valid. The fields are read back from the bytes with safe
    // code; no pointer to a typed struct is ever made.
    let filled = unsafe { libc::proc_pidinfo(pid, flavor, 0, buf.as_mut_ptr().cast(), len) };
    if filled <= 0 {
        return Err(last_errno());
    }
    if usize::try_from(filled).ok() != Some(buf.len()) {
        return Err(Errno::IO);
    }
    Ok(())
}

/// `sysctl(CTL_KERN, KERN_PROC, KERN_PROC_PID, pid)` into `buf`: how many
/// bytes the kernel wrote, 0 when there is no such process. Unlike
/// `proc_pidinfo`, it describes a zombie too.
#[allow(unsafe_code)]
fn kinfo(pid: i32, buf: &mut [u8]) -> Result<usize, Errno> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_PID, pid];
    let mut len: libc::size_t = buf.len();
    // SAFETY: `mib` is a live array of four `c_int` and `namelen` says four;
    // `buf` is a live, writable byte slice of `len` bytes, and the kernel
    // writes at most `*oldlenp` bytes and stores the count it wrote in `len`,
    // a live local. No new value is set (`newp` is null, `newlen` 0). Any
    // byte pattern is a valid `u8`.
    let result = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            4,
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return Err(last_errno());
    }
    if len > buf.len() {
        return Err(Errno::IO);
    }
    Ok(len)
}

/// `proc_listpids(kind, arg, ...)`: the pids the kernel lists, grown until
/// they fit. A buffer that comes back full may have been cut short, so it is
/// never taken as the whole answer.
#[allow(unsafe_code)]
fn list_pids(kind: u32, arg: u32) -> Result<Vec<i32>, Errno> {
    // Tests start small so that growing is exercised with a few children.
    let mut capacity: usize = if cfg!(test) { 4 } else { 256 };
    loop {
        let mut pids = vec![0i32; capacity];
        let bytes = capacity * std::mem::size_of::<i32>();
        let len = libc::c_int::try_from(bytes).map_err(|_| Errno::NOMEM)?;
        // SAFETY: `pids` owns `capacity` initialised `i32`s, which is `len`
        // bytes, writable for the duration of the call; the kernel writes at
        // most `buffersize` bytes and returns how many it wrote. Any bytes
        // are a valid `i32`. Only the returned count is read afterwards.
        // `__error()` returns this thread's errno, a valid `c_int` for the
        // thread's life; it is cleared first because libproc answers a failed
        // call with 0 (not -1) and leaves errno set, the same 0 it answers for
        // an empty filtered list.
        let (filled, errno) = unsafe {
            *libc::__error() = 0;
            let filled = libc::proc_listpids(kind, arg, pids.as_mut_ptr().cast(), len);
            (filled, *libc::__error())
        };
        if filled < 0 || (filled == 0 && errno != 0) {
            return Err(Errno::from_raw_os_error(if errno != 0 {
                errno
            } else {
                libc::EIO
            }));
        }
        let filled = usize::try_from(filled).unwrap_or(0);
        if filled >= bytes {
            if capacity >= 1 << 20 {
                return Err(Errno::NOMEM);
            }
            capacity *= 2;
            continue;
        }
        pids.truncate(filled / std::mem::size_of::<i32>());
        pids.retain(|p| *p > 0);
        return Ok(pids);
    }
}

/// Signal the process with this pid only if its pid version is still
/// `version`. The kernel checks the version and delivers the signal under one
/// lookup, so a pid that names another process by now is never signalled.
#[allow(unsafe_code)]
fn signal_token(pid: i32, version: i32, signal: Signal) -> Result<(), Errno> {
    let mut token = AuditToken { val: [0; 8] };
    token.val[5] = pid.cast_unsigned();
    token.val[7] = version.cast_unsigned();
    // SAFETY: `token` is a properly aligned `#[repr(C)]` array of eight
    // `u32`, the layout of `audit_token_t`, on this stack frame for the whole
    // call; the function reads it and keeps no pointer to it. The signal
    // number comes from a `Signal`, so it is a valid one.
    let result = unsafe { proc_signal_with_audittoken(&mut token, signal.as_raw()) };
    match result {
        0 => Ok(()),
        // It answers with the error number, and sets errno to it as well.
        n if n > 0 => Err(Errno::from_raw_os_error(n)),
        _ => Err(last_errno()),
    }
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    let mut word = [0u8; 8];
    word.copy_from_slice(&bytes[at..at + 8]);
    u64::from_ne_bytes(word)
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    let mut word = [0u8; 4];
    word.copy_from_slice(&bytes[at..at + 4]);
    u32::from_ne_bytes(word)
}

/// Who a process is: its pid with what the kernel never reuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::process) struct Ident {
    pub pid: i32,
    /// Never reused while the system runs.
    pub uniqueid: u64,
    /// The unique id of the process that forked it; kept when it is
    /// reparented.
    pub puniqueid: u64,
    /// The pid version a signal by audit token is checked against.
    pub version: i32,
}

/// The identity of `pid`: `Ok(None)` when there is no such process or it is a
/// zombie, an error when it could not be read for another reason.
pub(in crate::process) fn identity(pid: i32) -> Result<Option<Ident>, Errno> {
    if pid <= 0 {
        return Ok(None);
    }
    let mut buf = [0u8; UNIQ_INFO_SIZE];
    match pidinfo(pid, PROC_PIDUNIQIDENTIFIERINFO, &mut buf) {
        Ok(()) => Ok(Some(Ident {
            pid,
            uniqueid: u64_at(&buf, UNIQ_OFFSET_UNIQUEID),
            puniqueid: u64_at(&buf, UNIQ_OFFSET_PUNIQUEID),
            version: u32_at(&buf, UNIQ_OFFSET_IDVERSION).cast_signed(),
        })),
        Err(Errno::SRCH) => Ok(None),
        Err(e) => Err(e),
    }
}

/// What `PROC_PIDTBSDINFO` says about a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::process) struct BsdInfo {
    pub ppid: i32,
    pub pgid: i32,
    pub ruid: u32,
    /// `p_stat`: `SSTOP` when stopped.
    pub status: u32,
    /// Start time in microseconds since the epoch.
    pub start_us: u64,
}

impl BsdInfo {
    pub(in crate::process) fn stopped(&self) -> bool {
        self.status == libc::SSTOP
    }
}

/// `PROC_PIDTBSDINFO` of `pid`; `Ok(None)` when it is gone or a zombie.
pub(in crate::process) fn bsd_info(pid: i32) -> Result<Option<BsdInfo>, Errno> {
    use std::mem::offset_of;
    if pid <= 0 {
        return Ok(None);
    }
    let mut buf = [0u8; BSD_INFO_SIZE];
    match pidinfo(pid, libc::PROC_PIDTBSDINFO, &mut buf) {
        Ok(()) => {
            let sec = u64_at(&buf, offset_of!(libc::proc_bsdinfo, pbi_start_tvsec));
            let usec = u64_at(&buf, offset_of!(libc::proc_bsdinfo, pbi_start_tvusec));
            Ok(Some(BsdInfo {
                ppid: u32_at(&buf, offset_of!(libc::proc_bsdinfo, pbi_ppid)).cast_signed(),
                pgid: u32_at(&buf, offset_of!(libc::proc_bsdinfo, pbi_pgid)).cast_signed(),
                ruid: u32_at(&buf, offset_of!(libc::proc_bsdinfo, pbi_ruid)),
                status: u32_at(&buf, offset_of!(libc::proc_bsdinfo, pbi_status)),
                start_us: sec.saturating_mul(1_000_000).saturating_add(usec),
            }))
        }
        Err(Errno::SRCH) => Ok(None),
        Err(e) => Err(e),
    }
}

/// What `sysctl(KERN_PROC_PID)` says of a process, which it says of a zombie
/// and of a process being created too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::process) struct KernProc {
    /// Start time in microseconds since the epoch.
    pub start_us: u64,
    pub ppid: i32,
    pub pgid: i32,
    pub ruid: u32,
    stat: u8,
    flags: u32,
}

impl KernProc {
    pub(in crate::process) fn zombie(&self) -> bool {
        self.stat == SZOMB
    }

    /// Exiting (`P_WEXIT`): libproc no longer describes it, it forks nothing
    /// more, and it becomes a zombie.
    pub(in crate::process) fn exiting(&self) -> bool {
        self.flags & P_WEXIT != 0
    }
}

/// The kernel's record of the process with this pid, alive, being created or
/// a zombie; `Ok(None)` when there is none.
pub(in crate::process) fn kern_proc(pid: i32) -> Result<Option<KernProc>, Errno> {
    if pid <= 0 {
        return Ok(None);
    }
    let mut buf = [0u8; KINFO_PROC_SIZE];
    match kinfo(pid, &mut buf)? {
        0 => Ok(None),
        n if n == KINFO_PROC_SIZE => {
            let sec = u64_at(&buf, KINFO_OFFSET_START_SEC);
            let usec = u64::from(u32_at(&buf, KINFO_OFFSET_START_USEC));
            Ok(Some(KernProc {
                start_us: sec.saturating_mul(1_000_000).saturating_add(usec),
                ppid: u32_at(&buf, KINFO_OFFSET_PPID).cast_signed(),
                pgid: u32_at(&buf, KINFO_OFFSET_PGID).cast_signed(),
                ruid: u32_at(&buf, KINFO_OFFSET_RUID),
                stat: buf[KINFO_OFFSET_STAT],
                flags: u32_at(&buf, KINFO_OFFSET_FLAG),
            }))
        }
        _ => Err(Errno::IO),
    }
}

/// When the process with this pid started, in microseconds since the epoch,
/// whether it is alive or a zombie; `Ok(None)` when there is none.
pub(in crate::process) fn start_of(pid: i32) -> Result<Option<u64>, Errno> {
    Ok(kern_proc(pid)?.map(|k| k.start_us))
}

/// The children of `pid`, zombies included; empty when it has none or is
/// gone.
pub(in crate::process) fn children(pid: i32) -> Result<Vec<i32>, Errno> {
    let Ok(parent) = u32::try_from(pid) else {
        return Ok(Vec::new());
    };
    list_pids(PROC_PPID_ONLY, parent)
}

/// The pids of the processes whose effective user is `uid`.
pub(in crate::process) fn user_pids(uid: u32) -> Result<Vec<i32>, Errno> {
    list_pids(PROC_UID_ONLY, uid)
}

/// Every pid on the system. An empty answer is a failure: this process is
/// on the system.
pub(in crate::process) fn all_pids() -> Result<Vec<i32>, Errno> {
    let pids = list_pids(PROC_ALL_PIDS, 0)?;
    if pids.is_empty() {
        return Err(Errno::IO);
    }
    Ok(pids)
}

/// What became of a signal sent by identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::process) enum Sent {
    Delivered,
    /// No process has this pid and version any more.
    Gone,
    /// The kernel refused (EPERM, for a process of another user or a
    /// platform process), or answered something else.
    Refused,
}

/// Signal the process `who` names, and only it.
pub(in crate::process) fn signal(who: &Ident, signal: Signal) -> Sent {
    match signal_token(who.pid, who.version, signal) {
        Ok(()) => Sent::Delivered,
        Err(Errno::SRCH) => Sent::Gone,
        Err(_) => Sent::Refused,
    }
}

/// Check that the calls the tracker relies on answer as expected here: the
/// keeper's own identity reads, names its parent's unique id as the one that
/// forked it, and a signal by token reaches the keeper with its own pid
/// version and nothing with another one. SIGCONT to a running process does
/// nothing.
pub(in crate::process) fn self_test() -> Result<(), String> {
    let me = rustix::process::getpid().as_raw_nonzero().get();
    let parent = rustix::process::getppid()
        .map(|p| p.as_raw_nonzero().get())
        .ok_or("the keeper has no parent")?;
    let own = identity(me)
        .map_err(|e| format!("proc_pidinfo flavor 17 of the keeper failed: {e}"))?
        .ok_or("proc_pidinfo flavor 17 found no keeper")?;
    let up = identity(parent)
        .map_err(|e| format!("proc_pidinfo flavor 17 of the keeper's parent failed: {e}"))?
        .ok_or("proc_pidinfo flavor 17 found no parent")?;
    if own.uniqueid == 0 || own.puniqueid != up.uniqueid {
        return Err("proc_pidinfo flavor 17 does not name the keeper's parent".into());
    }
    // The kernel record read through `sysctl` is decoded at fixed offsets of
    // `struct kinfo_proc`; check them against what libproc says of the
    // keeper itself.
    let info = bsd_info(me)
        .map_err(|e| format!("proc_pidinfo of the keeper failed: {e}"))?
        .ok_or("proc_pidinfo found no keeper")?;
    let record = kern_proc(me)
        .map_err(|e| format!("sysctl KERN_PROC of the keeper failed: {e}"))?
        .ok_or("sysctl KERN_PROC found no keeper")?;
    if record.start_us != info.start_us
        || record.ppid != info.ppid
        || record.pgid != info.pgid
        || record.ruid != info.ruid
        || record.zombie()
        || record.exiting()
    {
        return Err("sysctl KERN_PROC does not describe the keeper as libproc does".into());
    }
    if signal(&own, Signal::CONT) != Sent::Delivered {
        return Err("a signal by audit token did not reach the keeper".into());
    }
    let stale = Ident {
        version: own.version.wrapping_add(1),
        ..own
    };
    if signal(&stale, Signal::CONT) != Sent::Gone {
        return Err("a signal by audit token with a stale pid version was not refused".into());
    }
    Ok(())
}

/// One process event from the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::process) struct ProcEvent {
    pub pid: i32,
    pub forked: bool,
    pub exited: bool,
}

/// A `kqueue` that watches processes for `NOTE_FORK` and `NOTE_EXIT`, and
/// nothing else: only process filters are ever put into it.
pub(in crate::process) struct ProcQueue {
    fd: OwnedFd,
}

impl ProcQueue {
    pub(in crate::process) fn new() -> Result<Self, Errno> {
        Ok(Self {
            fd: rustix::event::kqueue::kqueue()?,
        })
    }

    /// `kevent` on this queue: apply `changes`, then collect up to the spare
    /// capacity of `out` (nothing when `out` is `None`). The events in
    /// `changes` are made only by [`ProcQueue::watch`].
    #[allow(unsafe_code)]
    fn kevent(
        &self,
        changes: &[Event],
        out: Option<&mut Vec<Event>>,
        timeout: Duration,
    ) -> Result<(), Errno> {
        let mut none: [Event; 0] = [];
        // SAFETY: rustix requires that file descriptors named by the events
        // stay valid while the kqueue lives. Every event this queue is given
        // is an `EVFILT_PROC` event made in `watch`, which names a process,
        // not a file descriptor, so there is no descriptor to keep valid.
        // `spare_capacity` hands the kernel only the vector's unused
        // capacity and sets its length to what was written; an empty array
        // asks for no events at all.
        unsafe {
            match out {
                Some(out) => rustix::event::kqueue::kevent(
                    &self.fd,
                    changes,
                    rustix::buffer::spare_capacity(out),
                    Some(timeout),
                )
                .map(|_| ()),
                None => rustix::event::kqueue::kevent(&self.fd, changes, &mut none, Some(timeout))
                    .map(|_| ()),
            }
        }
    }

    /// Watch `pid` for forks and its exit. ESRCH when it has exited (a zombie
    /// included).
    pub(in crate::process) fn watch(&self, pid: i32) -> Result<(), Errno> {
        let pid = Pid::from_raw(pid).ok_or(Errno::SRCH)?;
        let event = Event::new(
            EventFilter::Proc {
                pid,
                flags: ProcessEvents::FORK | ProcessEvents::EXIT,
            },
            EventFlags::ADD | EventFlags::CLEAR,
            std::ptr::null_mut(),
        );
        self.kevent(&[event], None, Duration::ZERO)
    }

    /// Wait up to `timeout` for events, appended to `out`. EINTR is an empty
    /// wait.
    pub(in crate::process) fn wait(
        &self,
        timeout: Duration,
        out: &mut Vec<ProcEvent>,
    ) -> Result<(), Errno> {
        let mut events = Vec::with_capacity(256);
        match self.kevent(&[], Some(&mut events), timeout) {
            Ok(()) => {}
            Err(Errno::INTR) => return Ok(()),
            Err(e) => return Err(e),
        }
        for event in events {
            if let EventFilter::Proc { pid, flags } = event.filter() {
                out.push(ProcEvent {
                    pid: pid.as_raw_nonzero().get(),
                    forked: flags.contains(ProcessEvents::FORK),
                    exited: flags.contains(ProcessEvents::EXIT),
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_spawn::Guarded;
    use std::time::Instant;

    fn me() -> i32 {
        i32::try_from(std::process::id()).expect("a pid")
    }

    #[test]
    fn the_unique_id_layout_matches_the_kernel_here() {
        self_test().expect("the self-test passes on this system");
        let own = identity(me()).expect("readable").expect("alive");
        assert_ne!(own.uniqueid, 0);
        let info = bsd_info(me()).expect("readable").expect("alive");
        assert_eq!(
            Some(info.ppid),
            rustix::process::getppid().map(|p| p.as_raw_nonzero().get())
        );
        assert!(info.start_us > 0);
    }

    #[test]
    fn a_child_names_this_process_as_the_one_that_forked_it_and_is_listed() {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("5")
            .guarded_spawn()
            .expect("start a child");
        let pid = i32::try_from(child.id()).expect("a pid");
        let own = identity(me()).expect("readable").expect("alive");
        let theirs = identity(pid).expect("readable").expect("alive");
        let listed = children(me()).expect("listed");
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(theirs.puniqueid, own.uniqueid);
        assert!(listed.contains(&pid), "{listed:?}");
        // Reaped: gone, by every call.
        assert_eq!(identity(pid), Ok(None));
        assert_eq!(signal(&theirs, Signal::CONT), Sent::Gone);
    }

    #[test]
    fn an_orphan_keeps_the_unique_id_of_the_parent_that_forked_it() {
        // perl forks a grandchild that calls setsid and sleeps, prints its
        // pid and its own pid, and exits: the grandchild is reparented.
        let out = std::process::Command::new("/usr/bin/perl")
            .args([
                "-e",
                "use POSIX; my $g = fork(); if ($g == 0) { POSIX::setsid(); \
                 close STDOUT; close STDERR; sleep 5; exit 0 } print qq($$ $g\\n);",
            ])
            .guarded_output();
        let Ok(out) = out else {
            eprintln!("perl not available; skipped");
            return;
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let mut words = text.split_whitespace().map(|w| w.parse::<i32>().unwrap());
        let (_parent, orphan) = (words.next().unwrap(), words.next().unwrap());
        let until = Instant::now() + Duration::from_secs(5);
        let mut info = bsd_info(orphan).unwrap();
        while info.is_some_and(|i| i.ppid != 1) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
            info = bsd_info(orphan).unwrap();
        }
        let theirs = identity(orphan).unwrap().expect("the orphan is alive");
        // The parent was this test's child; its unique id is not readable
        // any more, but the orphan's record of it must not be launchd's.
        let launchd = identity(1).unwrap().expect("launchd");
        assert_eq!(info.map(|i| i.ppid), Some(1));
        assert_ne!(theirs.puniqueid, launchd.uniqueid);
        let own = identity(me()).unwrap().unwrap();
        assert!(theirs.puniqueid > own.uniqueid);
        assert_eq!(signal(&theirs, Signal::KILL), Sent::Delivered);
    }

    #[test]
    fn the_start_of_a_zombie_reads_and_matches_its_start_while_alive() {
        let mut child = std::process::Command::new("/usr/bin/true")
            .guarded_spawn()
            .expect("start a child");
        let pid = i32::try_from(child.id()).expect("a pid");
        let alive = start_of(pid).unwrap();
        // Wait for it to exit without reaping it: a zombie.
        let until = Instant::now() + Duration::from_secs(5);
        while identity(pid).unwrap().is_some() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        let zombie = start_of(pid).unwrap();
        let state = kern_proc(pid).unwrap();
        let _ = child.wait();
        assert!(state.is_some_and(|k| k.zombie()), "{state:?}");
        let reaped = start_of(pid).unwrap();
        assert!(zombie.is_some_and(|z| z > 0), "{zombie:?}");
        if let Some(alive) = alive {
            assert_eq!(Some(alive), zombie);
        }
        assert_eq!(reaped, None);
        let own = kern_proc(me()).unwrap().unwrap();
        let info = bsd_info(me()).unwrap().unwrap();
        assert_eq!(own.start_us, info.start_us);
        assert_eq!(own.ppid, info.ppid);
        assert_eq!(own.pgid, info.pgid);
        assert_eq!(own.ruid, info.ruid);
        assert_eq!(own.ruid, rustix::process::getuid().as_raw());
    }

    #[test]
    fn a_signal_with_a_stale_pid_version_reaches_nothing() {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("5")
            .guarded_spawn()
            .expect("start a child");
        let pid = i32::try_from(child.id()).expect("a pid");
        let who = identity(pid).unwrap().unwrap();
        let stale = Ident {
            version: who.version.wrapping_add(1),
            ..who
        };
        let refused = signal(&stale, Signal::KILL);
        let still = identity(pid).unwrap().is_some();
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(refused, Sent::Gone);
        assert!(still);
    }

    #[test]
    fn a_fork_is_reported_and_an_exited_process_cannot_be_watched() {
        let queue = ProcQueue::new().expect("a kqueue");
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "read x; /bin/sleep 0.1; /bin/sleep 0.1"])
            .stdin(std::process::Stdio::piped())
            .guarded_spawn()
            .expect("start a shell");
        let pid = i32::try_from(child.id()).expect("a pid");
        queue.watch(pid).expect("watch a live child");
        drop(child.stdin.take());
        let mut events = Vec::new();
        let until = Instant::now() + Duration::from_secs(5);
        while !events.iter().any(|e: &ProcEvent| e.exited) && Instant::now() < until {
            queue
                .wait(Duration::from_millis(100), &mut events)
                .expect("wait");
        }
        // A zombie cannot be watched.
        let zombie = queue.watch(pid);
        let _ = child.wait();
        assert!(
            events.iter().any(|e| e.pid == pid && e.forked),
            "{events:?}"
        );
        assert!(
            events.iter().any(|e| e.pid == pid && e.exited),
            "{events:?}"
        );
        assert_eq!(zombie, Err(Errno::SRCH));
    }

    #[test]
    fn a_listing_longer_than_the_first_buffer_is_complete() {
        let all = all_pids().expect("listed");
        assert!(all.contains(&me()), "{}", all.len());
        let mut kids = Vec::new();
        for _ in 0..12 {
            kids.push(
                std::process::Command::new("/bin/sleep")
                    .arg("5")
                    .guarded_spawn()
                    .expect("start a child"),
            );
        }
        let listed = children(me()).expect("listed");
        for kid in &mut kids {
            let _ = kid.kill();
            let _ = kid.wait();
        }
        for kid in &kids {
            assert!(listed.contains(&i32::try_from(kid.id()).unwrap()));
        }
    }
}
