//! Run several workers at once.
//!
//! A [`Supervisor`] owns the workers a caller starts. [`start`](Supervisor::start)
//! admits a request as a worker with its first run and returns at once;
//! [`follow_up`](Supervisor::follow_up) queues another assignment on the same
//! provider session; [`wait`](Supervisor::wait), [`read`](Supervisor::read) and
//! [`list`](Supervisor::list) report; [`interrupt`](Supervisor::interrupt) and
//! [`close`](Supervisor::close) stop. The seven operations are the MCP tools of
//! `docs/api.md`, one to one. `docs/supervisor.md` describes the states, the
//! limits and what is deferred.
//!
//! ```no_run
//! # async fn demo() {
//! use delega_core::contract::request::{Provider, Request};
//! use delega_core::supervisor::{Detail, Supervisor, SupervisorConfig, Target, WaitTargets};
//!
//! let supervisor = Supervisor::new(SupervisorConfig::default());
//! let started = supervisor
//!     .start(Request::new(Provider::Codex, "Summarize this repository"))
//!     .await
//!     .expect("the supervisor is running");
//! let waited = supervisor
//!     .wait(WaitTargets { runs: vec![started.run.clone()], workers: vec![] }, Some(120), None)
//!     .await
//!     .expect("wait");
//! let read = supervisor.read(Target::Run(started.run), Detail::Compact, None, None).await;
//! # let _ = (waited, read);
//! supervisor.shutdown().await;
//! # }
//! ```
//!
//! # How it is built
//!
//! One task owns all state and applies one command at a time. It never waits on
//! a provider: every run has a task of its own that starts the provider through
//! [`crate::provider`], waits for its one result and reports it back. Only
//! `wait`, `interrupt` and `close` (while it stops a running run) wait, and each
//! waits in the caller's task, bounded. A panic in a run's task ends that run as
//! `failed` and touches nothing else.
//!
//! # Platform
//!
//! Unix only, like the provider layer.

#![cfg(unix)]

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::contract::applied::Applied;
use crate::contract::request::Request;
use crate::contract::result::Cleanup;

mod activity;
mod actor;
mod bound;
mod command;
mod config;
mod core;
mod error;
mod ids;
pub mod limits;
mod preflight;
mod record;
mod results;
mod runner;
mod state;
mod views;

#[cfg(test)]
mod tests;

pub use activity::ActivityEntry;
pub use config::{Limits, SupervisorConfig};
pub use error::{Error, ErrorKind};
pub use state::{IllegalTransition, RunState, WorkerState};
pub use views::{
    CloseResponse, CompactResult, Detail, FollowUpResponse, InterruptResponse, LimitsView,
    ListResponse, ProgressView, ReadResponse, ResultView, RunBrief, RunDiagnostics, ShutdownReport,
    StartResponse, Target, TextWindow, WaitLimit, WaitResponse, WaitTargets, WorkerBrief,
    WorkerView,
};

use command::{Closing, Command, Stopping, WaitStart, Wake};

/// What every clone of a [`Supervisor`] shares. Dropping the last clone stops
/// the supervisor.
struct Shared {
    tx: UnboundedSender<Command>,
    config: SupervisorConfig,
    /// Cancelled to stop every run at once. Each run's stop token is a child
    /// of it, so `Drop` can signal every provider without the supervisor's
    /// task having to run.
    root: CancellationToken,
    /// Set if the supervisor's own task panicked.
    failed: Arc<Mutex<Option<String>>>,
    /// Runs the request checks of `start`.
    preflight: preflight::Preflight,
    /// Wall time for the bounds of `wait`, `interrupt` and `close`. The
    /// system's clock; only tests replace it.
    wall_clock: bound::WallClock,
    /// Test only: holds the next `wait` between its timeout and its view.
    #[cfg(test)]
    view_pause: Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>,
}

impl Shared {
    fn failure(&self) -> Option<String> {
        self.failed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for Shared {
    /// Signals every provider to stop, asks the supervisor's task to close
    /// every worker, and returns. It never waits, and it needs no runtime:
    /// cancelling a token and sending on an unbounded channel work from any
    /// thread, inside any task, during a panic, and after the runtime is gone.
    ///
    /// Cancelling the root token signals every run's stop token at once, without
    /// the supervisor's task having to run. Each run's task then interrupts its
    /// provider, and if the runtime is dropped first, the process layer's own
    /// drop path kills each child's process group and its tracked descendants
    /// without waiting for a process listing. The processes are therefore gone
    /// shortly after, not by the time this returns. Only
    /// [`Supervisor::shutdown`] waits for cleanup and reports what could not be
    /// confirmed; call it before the last clone is dropped when that matters.
    fn drop(&mut self) {
        self.root.cancel();
        self.preflight.close();
        let _ = self.tx.send(Command::Quit);
    }
}

/// A set of workers and the runs given to them. Cheap to clone; every clone is
/// the same supervisor.
///
/// Must be created inside a Tokio runtime. Dropping the last clone signals every
/// provider to stop and returns at once; it never waits and never panics. The
/// processes are gone shortly after. Only [`shutdown`](Self::shutdown) waits for
/// cleanup and reports what could not be confirmed, so call it before the last
/// clone is dropped, and before the runtime is.
#[derive(Clone)]
pub struct Supervisor {
    shared: Arc<Shared>,
}

impl Supervisor {
    /// A supervisor with the given limits and provider defaults.
    ///
    /// # Panics
    ///
    /// Panics if called outside a Tokio runtime.
    pub fn new(config: SupervisorConfig) -> Self {
        Self::with_hooks(
            config,
            runner::provider_launch(),
            preflight::Preflight::provider(),
        )
    }

    /// Test only: a supervisor whose `wait`, `interrupt` and `close` read wall
    /// time from `wall_clock`, so a test can move wall time without moving
    /// Tokio's.
    #[cfg(test)]
    pub(crate) fn with_wall_clock(config: SupervisorConfig, wall_clock: bound::WallClock) -> Self {
        let mut supervisor = Self::new(config);
        Arc::get_mut(&mut supervisor.shared)
            .expect("a new supervisor has one clone")
            .wall_clock = wall_clock;
        supervisor
    }

    #[cfg(test)]
    pub(crate) fn with_launcher(config: SupervisorConfig, launch: runner::Launch) -> Self {
        Self::with_hooks(config, launch, preflight::Preflight::provider())
    }

    fn with_hooks(
        config: SupervisorConfig,
        launch: runner::Launch,
        preflight: preflight::Preflight,
    ) -> Self {
        let (tx, rx) = unbounded_channel();
        let cursor = Arc::new(AtomicU64::new(0));
        let root = CancellationToken::new();
        let failed = Arc::new(Mutex::new(None));
        let core = core::Core::new(config.clone(), tx.clone(), launch, cursor, root.clone());
        let task = tokio::spawn(actor::run(core, rx, config.shutdown_bound));
        // If the task panics, `Core`'s drop has already stopped every run;
        // this records that it happened so no call can mistake it for a clean
        // stop.
        let watch_failed = failed.clone();
        tokio::spawn(async move {
            if let Err(error) = task.await {
                let how = if error.is_panic() {
                    "panicked"
                } else {
                    "was cancelled"
                };
                *watch_failed.lock().unwrap_or_else(PoisonError::into_inner) = Some(format!(
                    "the supervisor's task {how}; every provider was sent a stop, and nothing \
                     confirmed that any of them ended"
                ));
            }
        });
        Self {
            shared: Arc::new(Shared {
                tx,
                config,
                root,
                failed,
                preflight,
                wall_clock: bound::system_clock(),
                #[cfg(test)]
                view_pause: Mutex::new(None),
            }),
        }
    }

    /// The error for a call the supervisor's task cannot answer.
    fn gone(&self) -> Error {
        match self.shared.failure() {
            Some(why) => Error::Failed(why),
            None => Error::Stopped,
        }
    }

    async fn call<T>(&self, make: impl FnOnce(oneshot::Sender<T>) -> Command) -> Result<T, Error> {
        let (tx, rx) = oneshot::channel();
        self.shared.tx.send(make(tx)).map_err(|_| self.gone())?;
        rx.await.map_err(|_| self.gone())
    }

    /// Admit `request` as a new worker with its first run and return at once.
    ///
    /// The response says whether the run is `queued` (a running slot is not
    /// free) or `running`. A request that cannot go ahead (invalid, a limit
    /// reached, a name in use, a provider that cannot start) is still a run: the
    /// response is `finished` with outcome `refused`, no worker is created, and
    /// [`read`](Self::read) returns the result.
    pub async fn start(&self, request: Request) -> Result<StartResponse, Error> {
        self.start_checked(request, false, Applied::default()).await
    }

    /// [`start`](Self::start) for a caller that built the request from layers
    /// and knows which one set each field. `origin` carries the parts of
    /// `applied` only the caller knows (`config` and `sources`); the run's
    /// results, refusals included, report them, and a follow-up on the worker
    /// reports the same ones.
    pub async fn start_with_origin(
        &self,
        request: Request,
        origin: Applied,
    ) -> Result<StartResponse, Error> {
        self.start_checked(request, false, origin).await
    }

    /// [`start`](Self::start) for a caller whose own working directory says
    /// nothing about where the worker belongs, such as an MCP server a host
    /// launches in a directory of its own choosing.
    ///
    /// A request with no `cwd`, or a relative one, is refused as a finished run
    /// with kind `cwd_required`, and no provider check runs. A request that
    /// fails validation is still refused as `invalid_request`, and a name in use
    /// or the worker limit is still refused first; the check falls before the
    /// provider's own checks. [`follow_up`](Self::follow_up) needs no such
    /// variant: it reuses the worker's first `cwd`.
    pub async fn start_requiring_cwd(&self, request: Request) -> Result<StartResponse, Error> {
        self.start_checked(request, true, Applied::default()).await
    }

    /// [`start_requiring_cwd`](Self::start_requiring_cwd) with the `origin` of
    /// [`start_with_origin`](Self::start_with_origin).
    pub async fn start_requiring_cwd_with_origin(
        &self,
        request: Request,
        origin: Applied,
    ) -> Result<StartResponse, Error> {
        self.start_checked(request, true, origin).await
    }

    async fn start_checked(
        &self,
        request: Request,
        require_cwd: bool,
        origin: Applied,
    ) -> Result<StartResponse, Error> {
        let preflight = match require_cwd.then(|| cwd_required(&request)).flatten() {
            Some(refusal) => Some(refusal),
            None => self.preflight(&request).await,
        };
        // A shutdown that began while this start was being checked released it.
        // Answer here: a `Start` sent now could reach the supervisor's task
        // before the `Shutdown` that `shutdown` sends after closing the checks,
        // and the run would be started only to be stopped. A start that read
        // "not closed" just before the close may still be admitted; it finished
        // its check before the shutdown began, and the shutdown stops it.
        if self.shared.preflight.is_closed() {
            return Err(match self.shared.failure() {
                Some(why) => Error::Failed(why),
                None => Error::ShutDown,
            });
        }
        self.call(|reply| Command::Start {
            request: Box::new(request),
            preflight,
            origin: Box::new(origin),
            reply,
        })
        .await?
    }

    /// The filesystem and schema work of admitting a request, done off the
    /// supervisor's task and bounded, so a hung filesystem can delay only this
    /// `start`, never `wait` or `interrupt`. See [`preflight`].
    async fn preflight(&self, request: &Request) -> Option<(String, String)> {
        self.shared
            .preflight
            .run(request, &self.shared.config.provider)
            .await
    }

    /// Queue another assignment on `worker`, to continue its provider session
    /// with the same permissions and settings as its first request.
    ///
    /// Refused as a finished run, not an error, when the worker is closed, its
    /// queue is full, it was started ephemeral, or the prompt is not valid. A
    /// worker whose earlier runs reported no session refuses the run when its
    /// turn comes. `timeout_secs` replaces the worker's time limit for this run.
    pub async fn follow_up(
        &self,
        worker: &str,
        prompt: impl Into<String>,
        timeout_secs: Option<u64>,
    ) -> Result<FollowUpResponse, Error> {
        self.call(|reply| Command::FollowUp {
            worker: worker.to_string(),
            prompt: prompt.into(),
            timeout_secs,
            reply,
        })
        .await?
    }

    /// Wait until any named run finishes, or any named worker's current run
    /// finishes, or the time passes. A timeout is a response with `timed_out`,
    /// not an error. The time is counted in the time the machine was awake and
    /// checked against wall time about once a second, so after the machine
    /// slept past the limit the wait ends shortly after it wakes.
    ///
    /// `timeout_secs` defaults to the configured wait default
    /// ([`limits::DEFAULT_WAIT`] unless changed) and is capped at the wait
    /// ceiling. With `after_cursor`, returns at once if anything named
    /// finished after it. Pass the response's `cursor` back as `after_cursor`.
    /// Unknown ids are listed in `unknown`; the rest are still waited for.
    ///
    /// At most `Limits::max_waits_in_flight` waits block at once
    /// ([`limits::MAX_WAITS_IN_FLIGHT`] unless changed). A wait that would
    /// block beyond that returns at once with the current states,
    /// `timeout_secs: 0` and `limit_reached`; a wait that has something to
    /// report is answered whatever the count. A wait stops counting when it
    /// returns and when its future is dropped.
    pub async fn wait(
        &self,
        targets: WaitTargets,
        timeout_secs: Option<u64>,
        after_cursor: Option<u64>,
    ) -> Result<WaitResponse, Error> {
        let ceiling = self.shared.config.limits.wait_ceiling;
        let asked =
            timeout_secs.map_or(self.shared.config.limits.wait_default, Duration::from_secs);
        let applied = asked.min(ceiling);
        let start = self
            .call(|reply| Command::Wait {
                targets: targets.clone(),
                after: after_cursor,
                reply,
            })
            .await??;
        // The receiver outlives the timeout: dropping it would close the
        // registration, and a command handled before the view (a shutdown, or any
        // command that prunes closed registrations) would then make the view read
        // a registration that is gone as one that fired.
        let mut pending = None;
        let (waiter, timed_out, released) = match start {
            WaitStart::Ready => (None, false, false),
            WaitStart::Released => (None, false, true),
            WaitStart::LimitReached(view) => return Ok(*view),
            WaitStart::Pending { id, mut rx } => {
                let woke = bound::within(applied, &self.shared.wall_clock, &mut rx).await;
                match woke {
                    Some(woke) => (None, false, matches!(woke, Ok(Wake::Shutdown))),
                    None => {
                        pending = Some(rx);
                        (Some(id), true, false)
                    }
                }
            }
        };
        #[cfg(test)]
        self.pause_before_view().await;
        let mut response = self
            .call(|reply| Command::WaitView {
                targets,
                after: after_cursor,
                waiter,
                timed_out,
                released,
                timeout_secs: applied.as_secs(),
                reply,
            })
            .await??;
        // Shutdown may have fired the registration after the timeout and before
        // the view; the view then finds nothing registered and cannot say why.
        if let Some(mut rx) = pending {
            response.released_by_shutdown |= matches!(rx.try_recv(), Ok(Wake::Shutdown));
        }
        Ok(response)
    }

    /// Test only: hold the next `wait` after its timeout has elapsed and before
    /// it asks for the view. Returns a receiver that fires when the wait is held
    /// and a sender that lets it go on.
    #[cfg(test)]
    pub(crate) fn pause_next_wait_before_view(
        &self,
    ) -> (oneshot::Receiver<()>, oneshot::Sender<()>) {
        let (reached_tx, reached_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        *self
            .shared
            .view_pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some((reached_tx, release_rx));
        (reached_rx, release_tx)
    }

    #[cfg(test)]
    async fn pause_before_view(&self) {
        let held = self
            .shared
            .view_pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some((reached, release)) = held {
            let _ = reached.send(());
            let _ = release.await;
        }
    }

    /// The result if the run has finished; otherwise its state, the partial text
    /// and recent activity. Compact by default; [`Detail::Full`] returns every
    /// field of the result, the whole activity history and the supervisor's
    /// diagnostics. For a worker, reads its current run, or its latest.
    ///
    /// `after_cursor` limits the activity returned to entries after it.
    ///
    /// A compact read returns the first [`limits::COMPACT_RESULT_TEXT_CHARS`]
    /// characters of the answer and leaves out a structured answer larger than
    /// [`limits::COMPACT_STRUCTURED_BYTES`], saying so. `text_offset`, a number
    /// of characters, pages through a long answer: the result's text is the
    /// window of that many characters starting there, and `text_window` gives
    /// the offset, the window's length, the total, and whether more follows.
    /// Read again with `offset + length` for the next window. It works with
    /// either detail; a full read without an offset returns everything.
    pub async fn read(
        &self,
        target: Target,
        detail: Detail,
        after_cursor: Option<u64>,
        text_offset: Option<usize>,
    ) -> Result<ReadResponse, Error> {
        self.call(|reply| Command::Read {
            target,
            detail,
            after: after_cursor,
            text_offset,
            reply,
        })
        .await?
    }

    /// Every worker with its state, current run, queue length and session id,
    /// and the limits with what is in use.
    pub async fn list(&self) -> Result<ListResponse, Error> {
        self.call(|reply| Command::List { reply }).await?
    }

    /// Stop a run and return its `interrupted` result once it has stopped and
    /// cleanup was attempted, bounded by the process layer and by the
    /// supervisor's interrupt bound. That bound is counted in the time the
    /// machine was awake and checked against wall time, as a `wait` is.
    ///
    /// A queued run is removed and finishes `interrupted` without ever starting.
    /// A finished run is returned unchanged. For a worker, acts on its current
    /// run. Runs queued behind a stopped run go on.
    pub async fn interrupt(&self, target: Target) -> Result<InterruptResponse, Error> {
        let stopping = self
            .call(|reply| Command::Interrupt { target, reply })
            .await??;
        match stopping {
            Stopping::Done(response) => Ok(*response),
            Stopping::Pending { base, run, rx } => {
                let mut response = *base;
                let waited = self.stopped_in_time(rx).await;
                if waited && let Some(view) = self.settled(run).await? {
                    response.state = Some(RunState::Finished);
                    response.settled = true;
                    response.result = Some(view);
                }
                Ok(response)
            }
        }
    }

    /// Close a worker: stop its running run, discard its queued runs (each
    /// finishes `interrupted`, saying it was discarded by close), and free its
    /// slot. Finished results stay readable for the retention bound. Closing a
    /// closed worker answers the same way. Waits for the running run as
    /// [`interrupt`](Self::interrupt) does, with the same bound.
    pub async fn close(&self, worker: &str) -> Result<CloseResponse, Error> {
        let closing = self
            .call(|reply| Command::Close {
                worker: worker.to_string(),
                reply,
            })
            .await??;
        match closing {
            Closing::Done(response) => Ok(*response),
            Closing::Pending { base, run, rx } => {
                let mut response = *base;
                let waited = self.stopped_in_time(rx).await;
                if waited && let Some(view) = self.settled(run).await? {
                    response.settled = true;
                    response.result = Some(view);
                }
                Ok(response)
            }
        }
    }

    /// Whether the run a stop is waiting on reported within the interrupt
    /// bound, counted in awake time and checked against wall time.
    async fn stopped_in_time(&self, rx: oneshot::Receiver<()>) -> bool {
        let bound = self.shared.config.interrupt_bound;
        bound::within(bound, &self.shared.wall_clock, rx)
            .await
            .is_some()
    }

    async fn settled(&self, run: String) -> Result<Option<ResultView>, Error> {
        self.call(|reply| Command::Settled { run, reply }).await
    }

    /// Stop every worker and refuse new work. Returns when every run has
    /// stopped, or when the shutdown bound passes, naming the runs that had
    /// not. Reads and `list` keep working afterwards. If the supervisor's own
    /// task had panicked, the report says so in `failed` instead of looking
    /// clean.
    pub async fn shutdown(&self) -> ShutdownReport {
        // A start still waiting on a request check is released with `ShutDown`.
        self.shared.preflight.close();
        let started = self.call(|reply| Command::Shutdown { reply }).await;
        let Ok(started) = started else {
            return ShutdownReport {
                workers_closed: 0,
                runs_stopped: 0,
                runs_discarded: 0,
                unsettled: Vec::new(),
                failed: self.shared.failure(),
            };
        };
        if let Some(rx) = started.rx {
            let _ = tokio::time::timeout(self.shared.config.shutdown_bound, rx).await;
        }
        let unsettled = self
            .call(|reply| Command::Unsettled { reply })
            .await
            .unwrap_or_default();
        ShutdownReport {
            workers_closed: started.workers_closed,
            runs_stopped: started.runs_stopped,
            runs_discarded: started.runs_discarded,
            unsettled,
            failed: self.shared.failure(),
        }
    }

    /// Every run on this supervisor that settled with cleanup not confirmed,
    /// oldest first, with what the result says could not be confirmed.
    ///
    /// Recorded when each run settles, so a run counts after a later run on the
    /// same worker replaced it as the latest, and after retention dropped its
    /// result. At most the 256 most recent are kept. Still answers after a
    /// shutdown.
    pub async fn unconfirmed(&self) -> Result<Vec<(String, Cleanup)>, Error> {
        self.call(|reply| Command::Unconfirmed { reply }).await
    }

    /// Make the supervisor's task panic. Test only.
    #[cfg(test)]
    pub(crate) fn inject_panic(&self) {
        let _ = self.shared.tx.send(Command::PanicForTest);
    }
}

/// The refusal for a request whose `cwd` is missing or relative. A request that
/// fails validation is left to the supervisor, which refuses it as invalid.
fn cwd_required(request: &Request) -> Option<(String, String)> {
    if request.validate().is_err() {
        return None;
    }
    let message = match &request.cwd {
        None => {
            "cwd is required: give an absolute working directory. The MCP server's own \
                 working directory is chosen by the host (on Codex it is the plugin root), \
                 so it is not used as a default"
        }
        Some(cwd) if !cwd.is_absolute() => {
            "cwd must be an absolute path. The MCP server's own working directory is chosen \
             by the host (on Codex it is the plugin root), so a relative path has no fixed \
             meaning"
        }
        Some(_) => return None,
    };
    Some(("cwd_required".to_string(), message.to_string()))
}
