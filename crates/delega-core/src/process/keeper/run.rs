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
use crate::process::limits::KEEPER_TICK;
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
    /// The owner's end closed.
    Closed,
    Signal,
}

async fn keep(hello: Hello, fds: Vec<OwnedFd>) -> i32 {
    let parent = rustix::process::getppid();
    #[cfg(target_os = "linux")]
    say(&FromKeeper::Subreaper(sys::become_subreaper()));

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

    let (tx, mut rx) = mpsc::unbounded_channel();
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
    spawn_signal_watch(tx);

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
                    say(&FromKeeper::Tree { id, live: census(pid) });
                }
                Some(Event::Command(ToKeeper::Freeze(id))) => {
                    let live = freeze(pid).await;
                    say(&FromKeeper::Tree { id, live });
                }
                Some(Event::Command(ToKeeper::Reap)) => {
                    finish(pid, exited).await;
                    say(&FromKeeper::Reaped);
                    drop(child);
                    return 0;
                }
                Some(Event::Closed | Event::Signal) | None => {
                    stop_alone(&hello, pid).await;
                    drop(child);
                    return 0;
                }
            },
            _ = tick.tick() => {
                reap_orphans(pid);
                if rustix::process::getppid() != parent || Instant::now() >= ceiling {
                    stop_alone(&hello, pid).await;
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
            Err(_) => break,
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

/// The owner asked for the reap: everything is settled on its side. Anything
/// still alive in the tree is stopped first (Linux), the program is killed if
/// it has not exited (the owner reaps only an exited program, so this is a
/// guard), then reaped, and the orphans with it.
async fn finish(pid: i32, exited: bool) {
    #[cfg(target_os = "linux")]
    if census(pid) > 0 {
        let _ = freeze(pid).await;
    }
    if !exited && !sys::leader_has_exited(pid) {
        let _ = sys::signal_process(pid, Signal::KILL);
        let _ = wait_exit(pid, Duration::from_secs(5)).await;
    }
    let _ = tokio::task::spawn_blocking(move || {
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while !sys::reap_if_exited(pid) && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
    })
    .await;
    reap_orphans(pid);
}

/// The live processes in the keeper's tree.
fn census(_pid: i32) -> usize {
    #[cfg(target_os = "linux")]
    {
        super::walk::live_descendants(rustix::process::getpid().as_raw_nonzero().get())
    }
    #[cfg(not(target_os = "linux"))]
    {
        usize::from(!sys::leader_has_exited(_pid))
    }
}

/// Stop, kill and count the keeper's tree (Linux). Elsewhere the keeper has no
/// tree beyond the program and answers with the census.
async fn freeze(pid: i32) -> usize {
    #[cfg(target_os = "linux")]
    {
        let _ = pid;
        tokio::task::spawn_blocking(|| {
            super::walk::freeze_kill(rustix::process::getpid().as_raw_nonzero().get())
        })
        .await
        .unwrap_or(usize::MAX)
    }
    #[cfg(not(target_os = "linux"))]
    {
        census(pid)
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
async fn stop_alone(hello: &Hello, pid: i32) {
    let deadline = Instant::now() + hello.deadline.max(hello.grace);
    #[cfg(target_os = "linux")]
    {
        let _ = sys::signal_group(pid, Signal::TERM);
        let _ = sys::signal_group(pid, Signal::CONT);
        stop_alone_linux(hello, pid, deadline).await;
    }
    #[cfg(not(target_os = "linux"))]
    stop_alone_listed(hello, pid, deadline).await;
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
    for descendant in super::walk::descendants(me) {
        let _ = sys::signal_process(descendant, Signal::TERM);
        let _ = sys::signal_process(descendant, Signal::CONT);
    }
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
