//! The keeper process itself.

use std::io::IoSliceMut;
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use rustix::net::{RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags};
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::protocol::{FromKeeper, Hello, Lines, ToKeeper};
use crate::process::limits::{KEEPER_RELEASE_WAIT, KEEPER_TICK};
use crate::process::sys::{self, Signal};

/// Serialises writes to the socket from the keeper's threads.
static WRITE: Mutex<()> = Mutex::new(());

/// Write one line to the owner. A failure means the owner is gone, which the
/// reader sees as the end of the socket.
fn say(message: &FromKeeper) {
    let line = format!("{}\n", message.line());
    let _held = WRITE.lock().unwrap_or_else(|p| p.into_inner());
    let mut rest = line.as_bytes();
    while !rest.is_empty() {
        match rustix::io::write(rustix::stdio::stdin(), rest) {
            Ok(0) | Err(_) => return,
            Ok(n) => rest = &rest[n..],
        }
    }
}

/// Read the hello and the descriptors that travel with it from fd 0.
fn read_hello() -> Result<(Hello, Vec<OwnedFd>), String> {
    let mut bytes = Vec::new();
    let mut fds: Vec<OwnedFd> = Vec::new();
    let mut buffer = vec![0u8; 64 * 1024];
    {
        let mut space = [MaybeUninit::<u8>::uninit(); rustix::cmsg_space!(ScmRights(3))];
        let mut control = RecvAncillaryBuffer::new(&mut space);
        #[cfg(target_os = "linux")]
        let flags = RecvFlags::CMSG_CLOEXEC;
        #[cfg(not(target_os = "linux"))]
        let flags = RecvFlags::empty();
        let received = rustix::net::recvmsg(
            rustix::stdio::stdin(),
            &mut [IoSliceMut::new(&mut buffer)],
            &mut control,
            flags,
        )
        .map_err(|e| format!("reading the hello: {e}"))?;
        for message in control.drain() {
            if let RecvAncillaryMessage::ScmRights(received) = message {
                fds.extend(received);
            }
        }
        bytes.extend_from_slice(&buffer[..received.bytes]);
    }
    // macOS has no MSG_CMSG_CLOEXEC; nothing else runs in this process yet.
    for fd in &fds {
        rustix::io::fcntl_setfd(fd, rustix::io::FdFlags::CLOEXEC)
            .map_err(|e| format!("marking a descriptor close-on-exec: {e}"))?;
    }
    loop {
        if let Some(total) = Hello::total_len(&bytes)?
            && bytes.len() >= total
        {
            bytes.truncate(total);
            break;
        }
        let n = rustix::io::read(rustix::stdio::stdin(), &mut buffer)
            .map_err(|e| format!("reading the hello: {e}"))?;
        if n == 0 {
            return Err("the owner closed the socket during the hello".into());
        }
        bytes.extend_from_slice(&buffer[..n]);
    }
    let hello = Hello::decode(&bytes)?;
    if fds.len() != hello.descriptors() {
        return Err(format!(
            "the hello came with {} descriptors, not {}",
            fds.len(),
            hello.descriptors()
        ));
    }
    Ok((hello, fds))
}

/// Run the keeper: read the hello from standard input, start the program, and
/// keep it until the owner has it reaped or is gone. Returns the exit status
/// of the keeper process, which nothing reads; what matters goes over the
/// socket.
///
/// Standard input must be the keeper's end of the socket pair the owner made;
/// standard output and standard error are not used.
pub fn keeper_main() -> i32 {
    let (hello, fds) = match read_hello() {
        Ok(read) => read,
        Err(why) => {
            say(&FromKeeper::SpawnFailed {
                errno: 0,
                message: format!("the keeper could not read its work: {why}"),
            });
            return 2;
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            say(&FromKeeper::SpawnFailed {
                errno: 0,
                message: format!("the keeper could not start its runtime: {e}"),
            });
            return 2;
        }
    };
    runtime.block_on(keep(hello, fds))
}

/// Something the keeper must act on.
enum Event {
    Exited(Result<sys::ExitSummary, String>),
    Command(ToKeeper),
    /// The owner's end closed: a read returned end of file.
    Closed,
    /// Reading the owner's end failed for another reason. Not the owner
    /// closing: the program is stopped but left unreaped.
    ReadFailed,
    Signal,
}

/// Where the owner stands when the keeper stops the tree on its own.
enum Owner<'a> {
    /// Its end closed: nothing of it can signal any more, so the program is
    /// reaped.
    Gone,
    /// It may still be there: the program is reaped only after `released` or
    /// its end closing.
    Present(&'a mut mpsc::UnboundedReceiver<Event>),
    /// Its end failed: whether it is there cannot be known, so the program is
    /// left unreaped.
    Unknown,
}

async fn keep(hello: Hello, fds: Vec<OwnedFd>) -> i32 {
    let parent = rustix::process::getppid();
    #[cfg(target_os = "linux")]
    let subreaper = {
        let became = sys::become_subreaper();
        let ok = became.is_ok();
        say(&FromKeeper::Subreaper(became));
        ok
    };
    // Without a subreaper (and on every system but Linux) an orphan can leave
    // the keeper's tree, so no census of it proves anything at the reap.
    #[cfg(not(target_os = "linux"))]
    let subreaper = false;

    let mut fds = fds.into_iter();
    let stdin = if hello.stdin {
        fds.next().map_or_else(Stdio::null, Stdio::from)
    } else {
        Stdio::null()
    };
    let (Some(stdout), Some(stderr)) = (fds.next(), fds.next()) else {
        say(&FromKeeper::SpawnFailed {
            errno: 0,
            message: "the keeper was given no output pipes".into(),
        });
        return 2;
    };
    // The handlers come first: a terminating signal that arrives once the
    // program exists must stop it, not end the keeper by default action.
    let (tx, mut rx) = mpsc::unbounded_channel();
    spawn_signal_watch(tx.clone());
    let mut command = std::process::Command::new(&hello.program);
    command
        .args(&hello.args)
        .env_clear()
        .envs(hello.env.iter().map(|(k, v)| (k, v)))
        .stdin(stdin)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    if let Some(cwd) = &hello.cwd {
        command.current_dir(cwd);
    }
    sys::new_session(&mut command);
    let mut command = tokio::process::Command::from(command);
    command.kill_on_drop(false);
    let child = match crate::process::spawn::spawn(&mut command) {
        Ok(child) => child,
        Err(error) => {
            say(&FromKeeper::SpawnFailed {
                errno: error.raw_os_error().unwrap_or(0),
                message: error.to_string(),
            });
            return 0;
        }
    };
    // The program has its descriptors; the keeper's copies go.
    drop(command);
    let Some(pid) = child.id().and_then(|id| i32::try_from(id).ok()) else {
        say(&FromKeeper::SpawnFailed {
            errno: 0,
            message: "the program exited before its pid could be read".into(),
        });
        return 0;
    };
    say(&FromKeeper::Started(pid));

    let watch = tx.clone();
    let _ = std::thread::Builder::new()
        .name("keeper-exit".into())
        .spawn(move || {
            let _ = watch.send(Event::Exited(sys::wait_for_exit_status(pid)));
        });
    let commands = tx.clone();
    let _ = std::thread::Builder::new()
        .name("keeper-read".into())
        .spawn(move || read_commands(&commands));
    drop(tx);

    let ceiling = Instant::now() + hello.ceiling;
    let mut exited = false;
    let mut tick = tokio::time::interval(KEEPER_TICK);
    loop {
        tokio::select! {
            event = rx.recv() => match event {
                Some(Event::Exited(result)) => {
                    match result {
                        Ok(summary) => {
                            exited = true;
                            say(&match (summary.code, summary.signal) {
                                (Some(code), _) => FromKeeper::ExitedCode(code),
                                (None, Some(signal)) => FromKeeper::ExitedSignal(signal),
                                (None, None) => FromKeeper::ExitedCode(-1),
                            });
                        }
                        Err(why) => say(&FromKeeper::Warn(format!(
                            "the program's exit could not be observed: {why}"
                        ))),
                    }
                }
                Some(Event::Command(ToKeeper::Census(id))) => {
                    let (live, detail) = census_report(pid);
                    say(&FromKeeper::Tree { id, live, detail });
                }
                Some(Event::Command(ToKeeper::Freeze(id))) => {
                    let (live, detail) = freeze_report(pid).await;
                    say(&FromKeeper::Tree { id, live, detail });
                }
                Some(Event::Command(ToKeeper::Reap)) => {
                    say(&reap_reply(finish(pid, exited, subreaper).await));
                    drop(child);
                    return 0;
                }
                // Only an answer to `releasing` means anything; one that comes
                // late is ignored.
                Some(Event::Command(ToKeeper::Released)) => {}
                Some(Event::Closed) | None => {
                    stop_alone(&hello, pid, Owner::Gone).await;
                    drop(child);
                    return 0;
                }
                Some(Event::ReadFailed) => {
                    stop_alone(&hello, pid, Owner::Unknown).await;
                    drop(child);
                    return 0;
                }
                Some(Event::Signal) => {
                    stop_alone(&hello, pid, Owner::Present(&mut rx)).await;
                    drop(child);
                    return 0;
                }
            },
            _ = tick.tick() => {
                reap_orphans(pid);
                if rustix::process::getppid() != parent || Instant::now() >= ceiling {
                    stop_alone(&hello, pid, Owner::Present(&mut rx)).await;
                    drop(child);
                    return 0;
                }
            }
        }
    }
}

/// Read the owner's lines until its end closes.
fn read_commands(tx: &mpsc::UnboundedSender<Event>) {
    let mut lines = Lines::default();
    let mut buffer = [0u8; 1024];
    loop {
        match rustix::io::read(rustix::stdio::stdin(), &mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                for line in lines.push(&buffer[..n]) {
                    if let Some(command) = ToKeeper::parse(&line) {
                        let _ = tx.send(Event::Command(command));
                    }
                }
            }
            Err(rustix::io::Errno::INTR) => {}
            Err(_) => {
                let _ = tx.send(Event::ReadFailed);
                return;
            }
        }
    }
    let _ = tx.send(Event::Closed);
}

/// SIGTERM, SIGINT, SIGHUP and SIGQUIT stop the tree as the owner's end
/// closing does: `pkill delega` matches the keeper too, and the keeper must not
/// die and leave the program's group unpinned.
fn spawn_signal_watch(tx: mpsc::UnboundedSender<Event>) {
    use tokio::signal::unix::{SignalKind, signal};
    let kinds = [
        SignalKind::terminate(),
        SignalKind::interrupt(),
        SignalKind::hangup(),
        SignalKind::quit(),
    ];
    for kind in kinds {
        let Ok(mut stream) = signal(kind) else {
            say(&FromKeeper::Warn(
                "a signal handler could not be installed".into(),
            ));
            continue;
        };
        let tx = tx.clone();
        tokio::spawn(async move {
            if stream.recv().await.is_some() {
                let _ = tx.send(Event::Signal);
            }
        });
    }
}

/// Wait up to `limit` for the program to exit, without reaping it.
async fn wait_exit(pid: i32, limit: Duration) -> bool {
    let until = Instant::now() + limit;
    loop {
        if sys::leader_has_exited(pid) {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// What the keeper says when it has reaped the program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Finished {
    /// What the last freeze left alive.
    live: usize,
    /// Whether the final census proved the tree empty (Linux, with a
    /// subreaper).
    proven: bool,
}

/// The owner asked for the reap: everything is settled on its side. Anything
/// still alive in the tree is stopped first (Linux), the program is killed if
/// it has not exited (the owner reaps only an exited program, so this is a
/// guard; through a pidfd, never by number), then a final census is taken
/// (Linux), then the program is reaped, and the orphans with it. Returns what
/// the last freeze left alive and whether the final census proved the tree
/// empty, or why the program could not be killed or reaped; then `reaped` is
/// not sent.
async fn finish(pid: i32, exited: bool, subreaper: bool) -> Result<Finished, String> {
    // Test seam: injected only now, at the reap, so no answer to the owner
    // can report the failures; only the proof can show them.
    #[cfg(all(test, target_os = "linux"))]
    super::walk::inject_read_failures_from_env();
    #[cfg(target_os = "linux")]
    let live = if census(pid) > 0 {
        freeze(pid).await
    } else {
        0
    };
    #[cfg(not(target_os = "linux"))]
    let live = 0;
    if !exited && !sys::leader_has_exited(pid) {
        if !signal_program(pid, Signal::KILL) {
            return Err("the program was alive at the reap and could not be signalled".into());
        }
        let _ = wait_exit(pid, Duration::from_secs(5)).await;
    }
    // The final census, taken once the program has exited and before it is
    // reaped.
    #[cfg(target_os = "linux")]
    let last = if subreaper {
        let me = rustix::process::getpid().as_raw_nonzero().get();
        tokio::task::spawn_blocking(move || super::walk::census(me, pid))
            .await
            .ok()
    } else {
        None
    };
    let reaped = tokio::task::spawn_blocking(move || {
        consume_reap(pid, sys::reap_if_exited, Duration::from_secs(5))
    })
    .await
    .unwrap_or_else(|_| Err("the reap panicked".into()));
    reap_orphans(pid);
    // Decided after the reap of orphans, the last walk that can meet a failed
    // read: a read of `/proc` that failed at any point since the keeper
    // started withholds the proof.
    #[cfg(target_os = "linux")]
    let proven = last.is_some_and(|walked| super::walk::proof_holds(&walked));
    #[cfg(not(target_os = "linux"))]
    let proven = {
        let _ = subreaper;
        false
    };
    reaped.map(|()| Finished { live, proven })
}

/// Reap `pid` with `reap` until it succeeds or `bound` passes.
fn consume_reap(
    pid: i32,
    mut reap: impl FnMut(i32) -> bool,
    bound: Duration,
) -> Result<(), String> {
    let until = std::time::Instant::now() + bound;
    loop {
        if reap(pid) {
            return Ok(());
        }
        if std::time::Instant::now() >= until {
            return Err(format!(
                "the program could not be reaped within {} s",
                bound.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The answer to `reap`: `reaped` only when the program was reaped.
fn reap_reply(finished: Result<Finished, String>) -> FromKeeper {
    match finished {
        Ok(Finished { live, proven }) => FromKeeper::Reaped { live, proven },
        Err(why) => FromKeeper::Warn(why),
    }
}

/// Signal the program, the keeper's own child: through a pidfd on Linux,
/// never by number; elsewhere by its pid, which the keeper holds unreaped.
fn signal_program(pid: i32, signal: Signal) -> bool {
    #[cfg(target_os = "linux")]
    {
        use rustix::process::{PidfdFlags, pidfd_open, pidfd_send_signal};
        rustix::process::Pid::from_raw(pid)
            .and_then(|p| pidfd_open(p, PidfdFlags::empty()).ok())
            .is_some_and(|fd| pidfd_send_signal(&fd, signal).is_ok())
    }
    #[cfg(not(target_os = "linux"))]
    {
        sys::signal_process(pid, signal).is_ok()
    }
}

/// The live processes in the keeper's tree.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn census(pid: i32) -> usize {
    census_report(pid).0
}

/// The census with its pass details, for the owner's diagnostics.
fn census_report(_pid: i32) -> (usize, String) {
    #[cfg(target_os = "linux")]
    {
        let walked = super::walk::census(rustix::process::getpid().as_raw_nonzero().get(), _pid);
        (walked.effective_live(), walked.detail())
    }
    #[cfg(not(target_os = "linux"))]
    {
        (usize::from(!sys::leader_has_exited(_pid)), String::new())
    }
}

/// Stop, kill and count the keeper's tree (Linux). Elsewhere the keeper has no
/// tree beyond the program and answers with the census.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
async fn freeze(pid: i32) -> usize {
    freeze_report(pid).await.0
}

/// The file named by [`super::FREEZE_GATE_ENV`] in the keeper's environment,
/// read once (a test seam).
#[cfg(all(test, target_os = "linux"))]
fn freeze_gate() -> Option<&'static std::path::Path> {
    static GATE: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    GATE.get_or_init(|| std::env::var_os(super::FREEZE_GATE_ENV).map(Into::into))
        .as_deref()
}

/// The freeze with its pass details, for the owner's diagnostics.
async fn freeze_report(pid: i32) -> (usize, String) {
    #[cfg(target_os = "linux")]
    {
        tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            if let Some(gate) = freeze_gate() {
                let until = std::time::Instant::now() + Duration::from_secs(60);
                while !gate.exists() && std::time::Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            let walked =
                super::walk::freeze_kill(rustix::process::getpid().as_raw_nonzero().get(), pid);
            (walked.effective_live(), walked.detail())
        })
        .await
        .unwrap_or((usize::MAX, "the freeze panicked".into()))
    }
    #[cfg(not(target_os = "linux"))]
    {
        census_report(pid)
    }
}

/// Reap every exited child but the program (Linux, where orphans of the tree
/// become the keeper's children). The program is never reaped here: its pid
/// must keep naming its group until the owner says `reap`.
fn reap_orphans(_pid: i32) {
    #[cfg(target_os = "linux")]
    super::walk::reap_children(rustix::process::getpid().as_raw_nonzero().get(), _pid);
}

/// The owner is gone (or asked nothing in time): stop the tree by itself,
/// TERM, the grace, then KILL until nothing is left or the cleanup deadline
/// passes, then reap the program and exit.
async fn stop_alone(hello: &Hello, pid: i32, owner: Owner<'_>) {
    let deadline = Instant::now() + hello.deadline.max(hello.grace);
    #[cfg(target_os = "linux")]
    {
        let _ = sys::signal_group(pid, Signal::TERM);
        let _ = sys::signal_group(pid, Signal::CONT);
        stop_alone_linux(hello, pid, deadline).await;
    }
    #[cfg(not(target_os = "linux"))]
    stop_alone_listed(hello, pid, deadline).await;
    // An owner that may still be there must stop relying on the program's
    // pid before the reap frees it: say so and wait for its answer or its end
    // closing. Without either within the bound the program is left unreaped:
    // the keeper exits, and the program goes to the reaper above it (init or
    // another subreaper), which frees its pid only when it reaps it.
    match owner {
        Owner::Gone => {}
        Owner::Unknown => return,
        Owner::Present(rx) => {
            say(&FromKeeper::Releasing);
            let acknowledged = tokio::time::timeout(KEEPER_RELEASE_WAIT, async {
                loop {
                    match rx.recv().await {
                        // Only `released`, or a real end of file, lets the
                        // program's pid go.
                        Some(Event::Command(ToKeeper::Released) | Event::Closed) => break true,
                        Some(Event::ReadFailed) | None => break false,
                        Some(_) => {}
                    }
                }
            })
            .await
            .unwrap_or(false);
            if !acknowledged {
                return;
            }
        }
    }
    let _ = tokio::task::spawn_blocking(move || {
        let until = std::time::Instant::now() + Duration::from_secs(2);
        while !sys::reap_if_exited(pid) && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
    })
    .await;
    reap_orphans(pid);
}

#[cfg(target_os = "linux")]
async fn stop_alone_linux(hello: &Hello, pid: i32, deadline: Instant) {
    let me = rustix::process::getpid().as_raw_nonzero().get();
    super::walk::signal_tree(me, Signal::TERM);
    let grace_end = Instant::now() + hello.grace;
    while Instant::now() < grace_end.min(deadline) {
        reap_orphans(pid);
        if sys::leader_has_exited(pid) && census(pid) == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    loop {
        let _ = sys::signal_group(pid, Signal::KILL);
        let live = freeze(pid).await;
        reap_orphans(pid);
        if live == 0 && sys::leader_has_exited(pid) {
            return;
        }
        if Instant::now() >= deadline {
            say(&FromKeeper::Warn(format!(
                "{live} processes were still alive when the cleanup deadline passed"
            )));
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Without a subreaper the keeper finds the tree the way the owner does: a
/// process listing from the program (its group, its session, and ancestry),
/// and the run marker for what left the session.
#[cfg(not(target_os = "linux"))]
async fn stop_alone_listed(hello: &Hello, pid: i32, deadline: Instant) {
    use crate::process::tree::{ProcessTree, system_lister};
    let mut tree = ProcessTree::new(pid, system_lister(), true);
    if !hello.marker.is_empty() {
        tree = tree.with_marker(hello.marker.clone());
    }
    // One listing before the first signal, as the owner takes one: the TERM
    // can end a process whose child left the session, and that child is then
    // orphaned and tied to the run by nothing a later listing shows.
    let _ = tree.capture().await;
    let _ = sys::signal_group(pid, Signal::TERM);
    let _ = sys::signal_group(pid, Signal::CONT);
    let _ = tree.sweep(Some(Signal::TERM)).await;
    let grace_end = Instant::now() + hello.grace;
    while Instant::now() < grace_end.min(deadline) {
        if sys::leader_has_exited(pid) && tree.sweep(None).await.is_ok_and(|v| v.is_empty()) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let mut clean = 0;
    loop {
        let _ = sys::signal_group(pid, Signal::KILL);
        match tree.sweep(Some(Signal::KILL)).await {
            Ok(view) if view.is_empty() && sys::leader_has_exited(pid) => {
                clean += 1;
                if clean >= 2 {
                    return;
                }
            }
            _ => clean = 0,
        }
        if Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reap_that_never_succeeds_is_not_acknowledged() {
        let result = consume_reap(1, |_| false, Duration::from_millis(30));
        assert!(result.is_err());
        let reply = reap_reply(result.map(|()| Finished {
            live: 0,
            proven: true,
        }));
        assert!(matches!(reply, FromKeeper::Warn(_)), "{reply:?}");
    }

    #[test]
    fn a_reap_that_succeeds_is_acknowledged_with_what_was_left() {
        let mut tries = 0;
        let result = consume_reap(
            1,
            |_| {
                tries += 1;
                tries == 3
            },
            Duration::from_secs(5),
        );
        assert_eq!(result, Ok(()));
        assert_eq!(
            reap_reply(Ok(Finished {
                live: 2,
                proven: false
            })),
            FromKeeper::Reaped {
                live: 2,
                proven: false
            }
        );
    }
}
