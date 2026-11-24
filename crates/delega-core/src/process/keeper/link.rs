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
use crate::process::limits::{KEEPER_ABANDON_WAIT, KEEPER_REPLY_TIMEOUT, KEEPER_START_TIMEOUT};
use crate::process::sys::{self, ExitSummary};
use crate::process::tree::GroupPin;

/// How many census and freeze answers the diagnostics keep.
const TREE_LOG_ENTRIES: usize = 32;

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
    /// The keeper itself could not be started or did not answer. With
    /// `maybe_started` the keeper may have started the program before it
    /// stopped answering: it was told to stop whatever it started, and the
    /// run's cleanup cannot be confirmed.
    Keeper {
        message: String,
        maybe_started: bool,
    },
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
    reaped_tx: Mutex<Option<oneshot::Sender<Reaped>>>,
    /// The keeper's end closed before it said `reaped`.
    lost: AtomicBool,
    closed: AtomicBool,
    subreaper: AtomicBool,
    /// Census and freeze requests sent and not yet answered. An entry stays
    /// after its request timed out, so the answer, when it comes, is still
    /// judged by its verb.
    pending: Mutex<HashMap<u64, Pending>>,
    /// The latest answers to census and freeze requests, for the diagnostics.
    tree_log: Mutex<Vec<String>>,
    warnings: Mutex<Vec<String>>,
    /// Whether the program's pid still names its group and session: until
    /// the keeper says `releasing` or its end closes. Group and member signals
    /// are sent while holding this lock.
    pin: Arc<GroupPin>,
    /// See [`LinkHandle::unproven`].
    unproven: AtomicBool,
}

/// A census or freeze request waiting for its answer.
struct Pending {
    verb: &'static str,
    /// The awaiting request; `None` once it timed out.
    tx: Option<oneshot::Sender<(usize, String)>>,
}

impl State {
    fn log(&self, entry: String) {
        let mut log = lock(&self.tree_log);
        if log.len() >= TREE_LOG_ENTRIES {
            log.remove(0);
        }
        log.push(entry);
    }

    /// An answer to a census or freeze request, as the socket reader reads
    /// it. A proof failure no later answer repairs is recorded here, before
    /// the answer goes to any waiter, so it counts also when the request was
    /// cancelled or timed out and the waiter is gone.
    fn tree_answer(&self, id: u64, live: usize, detail: String) {
        let pending = lock(&self.pending).remove(&id);
        // An answer to no known request is judged by the strictest verb.
        let verb = pending.as_ref().map_or("freeze", |p| p.verb);
        if reply_unproven(verb, &detail) {
            self.unproven.store(true, Ordering::SeqCst);
        }
        let late = match pending.and_then(|p| p.tx) {
            Some(tx) => tx.send((live, detail.clone())).is_err(),
            None => true,
        };
        if late {
            self.log(
                format!("{verb}: late live={live} {detail}")
                    .trim_end()
                    .to_string(),
            );
        }
    }

    /// The keeper's answer to `reap`. What the keeper recorded as unproven
    /// during the run is recorded here before the answer goes to the reap.
    fn reaped(&self, reaped: Reaped) {
        if reaped.unproven {
            self.unproven.store(true, Ordering::SeqCst);
        }
        self.reaped.store(true, Ordering::SeqCst);
        if let Some(tx) = lock(&self.reaped_tx).take() {
            let _ = tx.send(reaped);
        }
    }
}

/// What the keeper said when it reaped the program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Reaped {
    /// How many processes of its tree the last freeze left alive.
    pub live: usize,
    /// Whether the final census before the reap proved the tree empty
    /// (Linux, with a subreaper).
    pub proven: bool,
    /// Whether any answer the keeper gave during the run, or worked out at
    /// the reap, was unproven in a way no later answer repairs (see
    /// [`reply_unproven`]), recorded by the keeper before it sent it.
    pub unproven: bool,
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

    /// Whether any census or freeze answer of this run was unproven in a way a
    /// later answer cannot repair (see [`reply_unproven`]). Sticky, and no
    /// final census supersedes it.
    pub(crate) fn unproven(&self) -> bool {
        self.state.unproven.load(Ordering::SeqCst)
    }

    /// The lock that says whether the program's pid still pins its group
    /// and session.
    pub(crate) fn pin(&self) -> Arc<GroupPin> {
        self.state.pin.clone()
    }

    /// What the keeper reported as warnings.
    pub(crate) fn warnings(&self) -> Vec<String> {
        lock(&self.state.warnings).clone()
    }

    fn send(&self, message: ToKeeper) -> bool {
        let line = format!("{}\n", message.line());
        (&self.stream).write_all(line.as_bytes()).is_ok()
    }

    async fn request(&self, make: fn(u64) -> ToKeeper, verb: &'static str) -> Option<usize> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        lock(&self.state.pending).insert(id, Pending { verb, tx: Some(tx) });
        if self.state.closed.load(Ordering::SeqCst) || !self.send(make(id)) {
            lock(&self.state.pending).remove(&id);
            self.log(format!("{verb}: not sent"));
            return None;
        }
        match tokio::time::timeout(KEEPER_REPLY_TIMEOUT, rx).await {
            // The reader recorded any proof failure before it delivered this.
            Ok(Ok((live, detail))) => {
                self.log(
                    format!("{verb}: live={live} {detail}")
                        .trim_end()
                        .to_string(),
                );
                Some(live)
            }
            _ => {
                // The entry stays for the reader to judge a late answer.
                if let Some(pending) = lock(&self.state.pending).get_mut(&id) {
                    pending.tx = None;
                }
                self.log(format!("{verb}: no answer"));
                None
            }
        }
    }

    fn log(&self, entry: String) {
        self.state.log(entry);
    }

    /// The latest answers to census and freeze requests, oldest first.
    pub(crate) fn tree_log(&self) -> Vec<String> {
        lock(&self.state.tree_log).clone()
    }

    /// How many processes are alive in the keeper's tree (Linux: every
    /// descendant of the keeper, found by walking `/proc`).
    pub(crate) async fn census(&self) -> Option<usize> {
        self.request(ToKeeper::Census, "census").await
    }

    /// Have the keeper stop, kill and count its tree (Linux).
    pub(crate) async fn freeze(&self) -> Option<usize> {
        self.request(ToKeeper::Freeze, "freeze").await
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
    reaped_rx: Option<oneshot::Receiver<Reaped>>,
}

impl KeeperLink {
    /// Have the keeper reap the program, wait for it to say so and for the
    /// keeper to exit, and return how the program ended and what the keeper
    /// said of its tree at the reap.
    pub(crate) async fn reap(&mut self) -> Result<(ExitSummary, Reaped), String> {
        let state = self.handle.state.clone();
        let sent = !self.handle.lost() && self.handle.send(ToKeeper::Reap);
        let confirmed = match (sent, self.reaped_rx.take()) {
            (true, Some(rx)) => tokio::time::timeout(KEEPER_REPLY_TIMEOUT * 2, rx)
                .await
                .ok()
                .and_then(Result::ok),
            _ => None,
        };
        if let Some(reaped) = confirmed {
            let unproven = if reaped.unproven {
                " unproven=true"
            } else {
                ""
            };
            self.handle.log(format!(
                "reaped: live={} proven={}{unproven}",
                reaped.live, reaped.proven
            ));
        }
        // The keeper exits after it said `reaped`; one that does not is
        // killed. It is this process's own child, so its pid is ours.
        end_child(&mut self.child, KEEPER_REPLY_TIMEOUT).await;
        self.handle.shut();
        let exit = *lock(&state.exit);
        match (exit, confirmed) {
            (Some(exit), Some(reaped)) => Ok((exit, reaped)),
            (Some(_), None) => Err("the keeper did not confirm that it reaped the program".into()),
            (None, _) => Err("the keeper ended without saying how the program ended".into()),
        }
    }
}

/// Whether a census or freeze answer leaves a hole no later answer repairs
/// (the owner's reader and the keeper both record it, before the answer is
/// delivered or sent):
/// a refused hold (a process that was never signalled), a failed read of
/// `/proc`, or a freeze that did not complete. A census that is incomplete
/// because the tree keeps changing reports a nonzero count and never claimed
/// an empty tree, so it is not one. An answer without details (no walk) is
/// not one.
pub(crate) fn reply_unproven(verb: &str, detail: &str) -> bool {
    let field = |name: &str| -> Option<&str> {
        detail
            .split_ascii_whitespace()
            .find_map(|word| word.strip_prefix(name)?.strip_prefix('='))
    };
    let nonzero = |name: &str| field(name).is_some_and(|v| v != "0");
    nonzero("refused")
        || nonzero("read_failures")
        || (verb == "freeze" && field("complete") == Some("false"))
}

/// Wait up to `bound` for the keeper, the owner's own child, to exit; kill it
/// if it has not, and wait up to `bound` again. A keeper still not reaped then
/// is left to the runtime's reaping of dropped children.
async fn end_child(child: &mut tokio::process::Child, bound: Duration) {
    if tokio::time::timeout(bound, child.wait()).await.is_err() {
        let _ = child.start_kill();
        let _ = tokio::time::timeout(bound, child.wait()).await;
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
    let keeper_error = |what: &str, e: &dyn std::fmt::Display| StartError::Keeper {
        message: format!("{what} {}: {e}", keeper.program.to_string_lossy()),
        maybe_started: false,
    };
    // One bound for the whole start, the keeper's own spawn included.
    let started_at = tokio::time::Instant::now();
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
        .ok_or_else(|| StartError::Keeper {
            message: "the keeper exited before its pid could be read".into(),
            maybe_started: false,
        })?;

    let launch_grace = launch.grace;
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
    // A write to a keeper that stopped reading must not block the owner.
    let _ = owner.set_write_timeout(Some(KEEPER_REPLY_TIMEOUT));
    let reader = owner
        .try_clone()
        .map_err(|e| keeper_error("could not prepare the keeper", &e))?;
    let thread_state = state.clone();
    let spawned = std::thread::Builder::new()
        .name(format!("keeper-{keeper_pid}"))
        .spawn(move || follow(reader, hello, give, thread_state, hand_tx));
    let mut child = child;
    if let Err(e) = spawned {
        // No hello was sent, so the keeper started nothing.
        let _ = owner.shutdown(std::net::Shutdown::Both);
        end_child(&mut child, KEEPER_REPLY_TIMEOUT).await;
        return Err(keeper_error("could not follow the keeper", &e));
    }

    let handshake = tokio::time::timeout_at(started_at + KEEPER_START_TIMEOUT, hand_rx).await;
    let pid = match handshake {
        Ok(Ok(Ok(pid))) => pid,
        other => {
            // Closing the owner's end has a keeper that did start the program
            // stop it by itself; it gets the grace and a margin for that
            // before it is killed.
            let _ = owner.shutdown(std::net::Shutdown::Both);
            end_child(&mut child, launch_grace + KEEPER_ABANDON_WAIT).await;
            let name = keeper.program.to_string_lossy();
            return Err(match other {
                Ok(Ok(Err(error))) => error,
                Err(_) => StartError::Keeper {
                    message: format!(
                        "the keeper {name} did not start the program within {} s",
                        KEEPER_START_TIMEOUT.as_secs()
                    ),
                    maybe_started: true,
                },
                _ => StartError::Keeper {
                    message: format!("the keeper {name} ended before it started the program"),
                    maybe_started: true,
                },
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
            // The program runs; closing the owner's end has the keeper stop it,
            // within the grace and a margin.
            handle.shut();
            end_child(&mut child, launch_grace + KEEPER_ABANDON_WAIT).await;
            return Err(StartError::Keeper {
                message: format!(
                    "could not read the program's output through the keeper {}: {e}",
                    keeper.program.to_string_lossy()
                ),
                maybe_started: true,
            });
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
        // The keeper cannot decode a hello that was not written in full, so
        // it started nothing.
        handshake(Err(StartError::Keeper {
            message: format!("could not hand the program to the keeper: {e}"),
            maybe_started: false,
        }));
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
                FromKeeper::Tree { id, live, detail } => state.tree_answer(id, live, detail),
                FromKeeper::Reaped {
                    live,
                    proven,
                    unproven,
                } => state.reaped(Reaped {
                    live,
                    proven,
                    unproven,
                }),
                FromKeeper::Releasing => {
                    // From here the keeper reaps the program on its own. No
                    // group or member signal is in flight once the pin is
                    // released under its lock, so the answer is true when
                    // sent.
                    state.pin.release();
                    state.lost.store(true, Ordering::SeqCst);
                    lock(&state.warnings).push(
                        "The keeper stopped the program on its own (a signal, its ceiling, or a \
                         new parent) before the owner had it reaped"
                            .into(),
                    );
                    let _ =
                        (&stream).write_all(format!("{}\n", ToKeeper::Released.line()).as_bytes());
                }
                FromKeeper::Warn(text) => lock(&state.warnings).push(format!("Keeper: {text}")),
            }
        }
    }
    state.closed.store(true, Ordering::SeqCst);
    if !state.reaped.load(Ordering::SeqCst) {
        state.pin.release();
        state.lost.store(true, Ordering::SeqCst);
        if started {
            lock(&state.warnings).push(
                "The keeper ended before it reaped the program; the program's process group \
                 was no longer signalled"
                    .into(),
            );
        }
    }
    handshake(Err(StartError::Keeper {
        message: "the keeper ended before it started the program".into(),
        maybe_started: true,
    }));
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A refused hold in one answer is not repaired by a later clean one: the
    /// flag the owner keeps is sticky for the run.
    #[test]
    fn a_keeper_refusal_followed_by_a_clean_retry_stays_unproven() {
        let flag = AtomicBool::new(false);
        let answers = [
            (
                "freeze",
                "passes=2 complete=false refused=1 read_failures=0",
            ),
            ("freeze", "passes=1 complete=true refused=0 read_failures=0"),
            ("census", "passes=2 complete=true refused=0 read_failures=0"),
        ];
        for (verb, detail) in answers {
            if reply_unproven(verb, detail) {
                flag.store(true, Ordering::SeqCst);
            }
        }
        assert!(flag.load(Ordering::SeqCst));
    }

    /// A link whose keeper end is returned and never answers by itself; the
    /// test plays the socket reader with [`State::tree_answer`].
    fn silent_link() -> (LinkHandle, UnixStream) {
        let (owner, keeper) = UnixStream::pair().expect("a socket pair");
        let handle = LinkHandle {
            keeper_pid: 0,
            provider_pid: 0,
            stream: owner,
            state: Arc::new(State::default()),
            next_id: AtomicU64::new(1),
            shut: AtomicBool::new(false),
        };
        (handle, keeper)
    }

    const REFUSED: &str = "passes=2 complete=false refused=1 read_failures=0";

    /// The keeper's refusal arrives after the owner's freeze request timed
    /// out: the reader still records it, and the run stays unproven.
    #[tokio::test(start_paused = true)]
    async fn a_refusal_answered_after_its_request_timed_out_leaves_the_run_unproven() {
        let (handle, _keeper) = silent_link();
        assert_eq!(handle.freeze().await, None);
        assert!(!handle.unproven());
        handle.state.tree_answer(1, 1, REFUSED.into());
        assert!(handle.unproven());
        assert_eq!(
            handle.tree_log(),
            vec![
                "freeze: no answer".to_string(),
                format!("freeze: late live=1 {REFUSED}")
            ]
        );
    }

    /// The same for a request whose future was dropped before the answer.
    #[tokio::test(start_paused = true)]
    async fn a_refusal_answered_after_its_request_was_cancelled_leaves_the_run_unproven() {
        let (handle, _keeper) = silent_link();
        assert!(
            tokio::time::timeout(Duration::from_millis(1), handle.census())
                .await
                .is_err()
        );
        handle.state.tree_answer(1, 1, REFUSED.into());
        assert!(handle.unproven());
    }

    /// A late answer is judged by the verb of its request: a census of a
    /// tree that keeps changing claimed nothing, and a freeze that did not
    /// complete did.
    #[tokio::test(start_paused = true)]
    async fn a_late_answer_is_judged_by_the_verb_of_its_request() {
        let changing = "passes=64 complete=false refused=0 read_failures=0";
        let (handle, _keeper) = silent_link();
        assert_eq!(handle.census().await, None);
        handle.state.tree_answer(1, 1, changing.into());
        assert!(!handle.unproven());
        assert_eq!(handle.freeze().await, None);
        handle.state.tree_answer(2, 1, changing.into());
        assert!(handle.unproven());
    }

    /// What the keeper carried in `reaped` makes the run unproven before the
    /// reap receives the answer.
    #[test]
    fn a_reap_answer_that_carries_an_unproven_answer_leaves_the_run_unproven() {
        let (handle, _keeper) = silent_link();
        handle.state.reaped(Reaped {
            live: 0,
            proven: true,
            unproven: true,
        });
        assert!(handle.unproven());
    }

    #[test]
    fn which_answers_are_unproven() {
        assert!(reply_unproven(
            "census",
            "passes=3 complete=true refused=0 read_failures=1"
        ));
        assert!(reply_unproven(
            "freeze",
            "passes=64 complete=false refused=0 read_failures=0"
        ));
        assert!(reply_unproven(
            "freeze",
            "passes=2 complete=false refused=1 read_failures=0"
        ));
        // A census of a tree that keeps changing never claimed zero.
        assert!(!reply_unproven(
            "census",
            "passes=64 complete=false refused=0 read_failures=0"
        ));
        assert!(!reply_unproven(
            "freeze",
            "passes=1 complete=true refused=0 read_failures=0"
        ));
        assert!(!reply_unproven("census", ""));
    }
}
