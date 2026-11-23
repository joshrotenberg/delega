//! The owner's side of the keeper: start it, hand it the program, and follow
//! what it says.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io::{IoSlice, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustix::net::{SendAncillaryBuffer, SendAncillaryMessage, SendFlags};
use tokio::sync::oneshot;

use super::KeeperCommand;
use super::protocol::{FromKeeper, Hello, Lines, ToKeeper};
use crate::process::limits::{KEEPER_REPLY_TIMEOUT, KEEPER_START_TIMEOUT};
use crate::process::sys::{self, ExitSummary};

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// What the keeper is to start.
pub(crate) struct LaunchSpec<'a> {
    pub program: &'a OsStr,
    pub args: &'a [OsString],
    pub env: Vec<(OsString, OsString)>,
    pub cwd: Option<&'a Path>,
    pub stdin: bool,
    pub grace: Duration,
    pub deadline: Duration,
    pub ceiling: Duration,
    pub marker: &'a str,
}

/// Why a kept run did not start.
#[derive(Debug)]
pub(crate) enum StartError {
    /// The keeper itself could not be started or did not answer.
    Keeper(String),
    /// The keeper started and the program could not be: what the operating
    /// system said.
    Program(std::io::Error),
}

/// A kept program that has started.
pub(crate) struct Started {
    pub link: KeeperLink,
    pub stdin: Option<tokio::net::unix::pipe::Sender>,
    pub stdout: tokio::net::unix::pipe::Receiver,
    pub stderr: tokio::net::unix::pipe::Receiver,
    pub exit_rx: oneshot::Receiver<Result<(), String>>,
}

#[derive(Default)]
struct State {
    exit: Mutex<Option<ExitSummary>>,
    exit_tx: Mutex<Option<oneshot::Sender<Result<(), String>>>>,
    exited: Arc<AtomicBool>,
    reaped: AtomicBool,
    reaped_tx: Mutex<Option<oneshot::Sender<()>>>,
    /// The keeper's end closed before it said `reaped`.
    lost: AtomicBool,
    closed: AtomicBool,
    subreaper: AtomicBool,
    pending: Mutex<HashMap<u64, oneshot::Sender<usize>>>,
    warnings: Mutex<Vec<String>>,
}

/// What the owner's shared state holds of the keeper: the socket, and what the
/// keeper said.
pub(crate) struct LinkHandle {
    pub keeper_pid: i32,
    pub provider_pid: i32,
    stream: UnixStream,
    state: Arc<State>,
    next_id: AtomicU64,
    shut: AtomicBool,
}

impl LinkHandle {
    /// Whether the keeper said the program has exited.
    pub(crate) fn exited(&self) -> bool {
        self.state.exited.load(Ordering::SeqCst)
    }

    /// Whether the keeper ended before the program was reaped. The program's
    /// pid then no longer pins its group: no group signal may follow.
    pub(crate) fn lost(&self) -> bool {
        self.state.lost.load(Ordering::SeqCst)
    }

    /// Whether the keeper is a child subreaper (Linux).
    pub(crate) fn subreaper(&self) -> bool {
        self.state.subreaper.load(Ordering::SeqCst)
    }

    /// What the keeper reported as warnings.
    pub(crate) fn warnings(&self) -> Vec<String> {
        lock(&self.state.warnings).clone()
    }

    fn send(&self, message: ToKeeper) -> bool {
        let line = format!("{}\n", message.line());
        (&self.stream).write_all(line.as_bytes()).is_ok()
    }

    async fn request(&self, make: fn(u64) -> ToKeeper) -> Option<usize> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        lock(&self.state.pending).insert(id, tx);
        if self.state.closed.load(Ordering::SeqCst) || !self.send(make(id)) {
            lock(&self.state.pending).remove(&id);
            return None;
        }
        match tokio::time::timeout(KEEPER_REPLY_TIMEOUT, rx).await {
            Ok(Ok(live)) => Some(live),
            _ => {
                lock(&self.state.pending).remove(&id);
                None
            }
        }
    }

    /// How many processes are alive in the keeper's tree (Linux: every
    /// descendant of the keeper, found by walking `/proc`).
    pub(crate) async fn census(&self) -> Option<usize> {
        self.request(ToKeeper::Census).await
    }

    /// Have the keeper stop, kill and count its tree (Linux).
    pub(crate) async fn freeze(&self) -> Option<usize> {
        self.request(ToKeeper::Freeze).await
    }

    /// Close the owner's end: the keeper stops the tree by itself.
    pub(crate) fn shut(&self) {
        if !self.shut.swap(true, Ordering::SeqCst) {
            let _ = self.stream.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// The keeper as the owner holds it: its own child, and the shared handle.
pub(crate) struct KeeperLink {
    child: tokio::process::Child,
    pub handle: Arc<LinkHandle>,
    reaped_rx: Option<oneshot::Receiver<()>>,
}

impl KeeperLink {
    /// Have the keeper reap the program, wait for it to say so and for the
    /// keeper to exit, and return how the program ended.
    pub(crate) async fn reap(&mut self) -> Result<ExitSummary, String> {
        let state = self.handle.state.clone();
        let sent = !self.handle.lost() && self.handle.send(ToKeeper::Reap);
        let confirmed = match (sent, self.reaped_rx.take()) {
            (true, Some(rx)) => tokio::time::timeout(KEEPER_REPLY_TIMEOUT * 2, rx)
                .await
                .is_ok_and(|r| r.is_ok()),
            _ => state.reaped.load(Ordering::SeqCst),
        };
        // The keeper exits after it said `reaped`; one that does not is
        // killed. It is this process's own child, so its pid is ours.
        if tokio::time::timeout(KEEPER_REPLY_TIMEOUT, self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.start_kill();
            let _ = self.child.wait().await;
        }
        self.handle.shut();
        let exit = *lock(&state.exit);
        match (exit, confirmed) {
            (Some(exit), true) => Ok(exit),
            (Some(_), false) => Err("the keeper did not confirm that it reaped the program".into()),
            (None, _) => Err("the keeper ended without saying how the program ended".into()),
        }
    }
}

fn cloexec_pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    #[cfg(target_os = "linux")]
    {
        rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).map_err(Into::into)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let (read, write) = rustix::pipe::pipe()?;
        rustix::io::fcntl_setfd(&read, rustix::io::FdFlags::CLOEXEC)?;
        rustix::io::fcntl_setfd(&write, rustix::io::FdFlags::CLOEXEC)?;
        Ok((read, write))
    }
}

/// The socket pair and the program's pipes, all close-on-exec. Made under the
/// spawn lock, because on macOS close-on-exec is set only after each is made.
struct Ends {
    owner: UnixStream,
    keeper: UnixStream,
    stdin: Option<(OwnedFd, OwnedFd)>,
    stdout: (OwnedFd, OwnedFd),
    stderr: (OwnedFd, OwnedFd),
}

fn ends(stdin: bool) -> std::io::Result<Ends> {
    let _guard = crate::process::spawn_guard();
    let (owner, keeper) = UnixStream::pair()?;
    Ok(Ends {
        owner,
        keeper,
        stdin: if stdin { Some(cloexec_pipe()?) } else { None },
        stdout: cloexec_pipe()?,
        stderr: cloexec_pipe()?,
    })
}

/// Start the keeper and have it start the program.
pub(crate) async fn start(
    keeper: &KeeperCommand,
    launch: LaunchSpec<'_>,
) -> Result<Started, StartError> {
    let keeper_error = |what: &str, e: &dyn std::fmt::Display| {
        StartError::Keeper(format!("{what} {}: {e}", keeper.program.to_string_lossy()))
    };
    let ends = ends(launch.stdin).map_err(|e| keeper_error("could not prepare the keeper", &e))?;
    let Ends {
        owner,
        keeper: keeper_end,
        stdin,
        stdout,
        stderr,
    } = ends;

    let mut command = std::process::Command::new(&keeper.program);
    command
        .args(&keeper.args)
        .env_clear()
        .envs(keeper.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::from(OwnedFd::from(keeper_end)))
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(value) = std::env::var_os(crate::process::tree::NO_LISTING_ENV) {
        command.env(crate::process::tree::NO_LISTING_ENV, value);
    }
    sys::new_session(&mut command);
    let mut command = tokio::process::Command::from(command);
    // Killing the keeper is exactly what must not happen when the owner
    // goes: closing the owner's end of the socket stops the tree instead.
    command.kill_on_drop(false);
    let child = crate::process::spawn::spawn(&mut command)
        .map_err(|e| keeper_error("could not start the keeper", &e))?;
    drop(command);
    let keeper_pid = child
        .id()
        .and_then(|id| i32::try_from(id).ok())
        .ok_or_else(|| {
            StartError::Keeper("the keeper exited before its pid could be read".into())
        })?;

    let hello = Hello {
        program: launch.program.to_os_string(),
        args: launch.args.to_vec(),
        env: launch.env,
        cwd: launch.cwd.map(Path::to_path_buf),
        stdin: launch.stdin,
        grace: launch.grace,
        deadline: launch.deadline,
        ceiling: launch.ceiling,
        marker: launch.marker.to_string(),
    };
    let (stdin_keep, stdin_give) = match stdin {
        Some((read, write)) => (Some(write), Some(read)),
        None => (None, None),
    };
    let (stdout_keep, stdout_give) = (stdout.0, stdout.1);
    let (stderr_keep, stderr_give) = (stderr.0, stderr.1);
    let mut give: Vec<OwnedFd> = Vec::new();
    give.extend(stdin_give);
    give.push(stdout_give);
    give.push(stderr_give);

    let state = Arc::new(State::default());
    let (exit_tx, exit_rx) = oneshot::channel();
    *lock(&state.exit_tx) = Some(exit_tx);
    let (reaped_tx, reaped_rx) = oneshot::channel();
    *lock(&state.reaped_tx) = Some(reaped_tx);
    let (hand_tx, hand_rx) = oneshot::channel::<Result<i32, StartError>>();
    let reader = owner
        .try_clone()
        .map_err(|e| keeper_error("could not prepare the keeper", &e))?;
    let thread_state = state.clone();
    let spawned = std::thread::Builder::new()
        .name(format!("keeper-{keeper_pid}"))
        .spawn(move || follow(reader, hello, give, thread_state, hand_tx));
    let mut child = child;
    if let Err(e) = spawned {
        let _ = child.start_kill();
        let _ = child.wait().await;
        return Err(keeper_error("could not follow the keeper", &e));
    }

    let handshake = tokio::time::timeout(KEEPER_START_TIMEOUT, hand_rx).await;
    let pid = match handshake {
        Ok(Ok(Ok(pid))) => pid,
        other => {
            let _ = owner.shutdown(std::net::Shutdown::Both);
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(match other {
                Ok(Ok(Err(error))) => error,
                Err(_) => StartError::Keeper(format!(
                    "the keeper {} did not start the program within {} s",
                    keeper.program.to_string_lossy(),
                    KEEPER_START_TIMEOUT.as_secs()
                )),
                _ => StartError::Keeper(format!(
                    "the keeper {} ended before it started the program",
                    keeper.program.to_string_lossy()
                )),
            });
        }
    };
    let pipes = (|| -> std::io::Result<_> {
        Ok((
            stdin_keep
                .map(tokio::net::unix::pipe::Sender::from_owned_fd)
                .transpose()?,
            tokio::net::unix::pipe::Receiver::from_owned_fd(stdout_keep)?,
            tokio::net::unix::pipe::Receiver::from_owned_fd(stderr_keep)?,
        ))
    })();
    let handle = Arc::new(LinkHandle {
        keeper_pid,
        provider_pid: pid,
        stream: owner,
        state,
        next_id: AtomicU64::new(1),
        shut: AtomicBool::new(false),
    });
    let (stdin, stdout, stderr) = match pipes {
        Ok(pipes) => pipes,
        Err(e) => {
            // The program runs; closing the owner's end has the keeper stop it.
            handle.shut();
            let _ = child.wait().await;
            return Err(keeper_error(
                "could not read the program's output through",
                &e,
            ));
        }
    };
    Ok(Started {
        link: KeeperLink {
            child,
            handle,
            reaped_rx: Some(reaped_rx),
        },
        stdin,
        stdout,
        stderr,
        exit_rx,
    })
}

/// Send the hello with the descriptors attached to its first bytes.
fn send_hello(stream: &UnixStream, hello: &[u8], fds: &[OwnedFd]) -> std::io::Result<()> {
    use std::os::fd::AsFd;
    let borrowed: Vec<_> = fds.iter().map(AsFd::as_fd).collect();
    let mut space = [MaybeUninit::<u8>::uninit(); rustix::cmsg_space!(ScmRights(3))];
    let mut control = SendAncillaryBuffer::new(&mut space);
    if !control.push(SendAncillaryMessage::ScmRights(&borrowed)) {
        return Err(std::io::Error::other("too many descriptors for the keeper"));
    }
    let sent = rustix::net::sendmsg(
        stream,
        &[IoSlice::new(hello)],
        &mut control,
        SendFlags::empty(),
    )?;
    let mut writer = stream;
    writer.write_all(&hello[sent..])
}

/// The thread that hands the keeper its work and then reads what it says,
/// until the keeper's end closes.
fn follow(
    mut stream: UnixStream,
    hello: Hello,
    give: Vec<OwnedFd>,
    state: Arc<State>,
    hand_tx: oneshot::Sender<Result<i32, StartError>>,
) {
    let mut hand_tx = Some(hand_tx);
    let mut handshake = |result: Result<i32, StartError>| {
        if let Some(tx) = hand_tx.take() {
            let _ = tx.send(result);
        }
    };
    let sent = send_hello(&stream, &hello.encode(), &give);
    // The keeper has its copies now; the owner's go.
    drop(give);
    if let Err(e) = sent {
        handshake(Err(StartError::Keeper(format!(
            "could not hand the program to the keeper: {e}"
        ))));
        return;
    }
    let mut lines = Lines::default();
    let mut buffer = [0u8; 4096];
    let mut started = false;
    loop {
        let n = match stream.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for line in lines.push(&buffer[..n]) {
            let Some(message) = FromKeeper::parse(&line) else {
                continue;
            };
            match message {
                FromKeeper::Subreaper(result) => match result {
                    Ok(()) => state.subreaper.store(true, Ordering::SeqCst),
                    Err(why) => lock(&state.warnings).push(format!(
                        "The keeper could not become a child subreaper: {why}"
                    )),
                },
                FromKeeper::Started(pid) => {
                    started = true;
                    handshake(Ok(pid));
                }
                FromKeeper::SpawnFailed { errno, message } => {
                    let error = if errno > 0 {
                        std::io::Error::from_raw_os_error(errno)
                    } else {
                        std::io::Error::other(message)
                    };
                    handshake(Err(StartError::Program(error)));
                }
                FromKeeper::ExitedCode(code) => exited(
                    &state,
                    ExitSummary {
                        code: Some(code),
                        signal: None,
                    },
                ),
                FromKeeper::ExitedSignal(signal) => exited(
                    &state,
                    ExitSummary {
                        code: None,
                        signal: Some(signal),
                    },
                ),
                FromKeeper::Tree { id, live } => {
                    if let Some(tx) = lock(&state.pending).remove(&id) {
                        let _ = tx.send(live);
                    }
                }
                FromKeeper::Reaped => {
                    state.reaped.store(true, Ordering::SeqCst);
                    if let Some(tx) = lock(&state.reaped_tx).take() {
                        let _ = tx.send(());
                    }
                }
                FromKeeper::Warn(text) => lock(&state.warnings).push(format!("Keeper: {text}")),
            }
        }
    }
    state.closed.store(true, Ordering::SeqCst);
    if !state.reaped.load(Ordering::SeqCst) {
        state.lost.store(true, Ordering::SeqCst);
        if started {
            lock(&state.warnings).push(
                "The keeper ended before it reaped the program; the program's process group \
                 was no longer signalled"
                    .into(),
            );
        }
    }
    handshake(Err(StartError::Keeper(
        "the keeper ended before it started the program".into(),
    )));
    if let Some(tx) = lock(&state.exit_tx).take() {
        let _ = tx.send(Err("the keeper ended before the program did".into()));
    }
    lock(&state.pending).clear();
}

fn exited(state: &State, summary: ExitSummary) {
    *lock(&state.exit) = Some(summary);
    state.exited.store(true, Ordering::SeqCst);
    if let Some(tx) = lock(&state.exit_tx).take() {
        let _ = tx.send(Ok(()));
    }
}
