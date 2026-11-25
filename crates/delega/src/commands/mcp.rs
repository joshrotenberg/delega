//! `delega mcp`: serve the MCP tools over stdio.
//!
//! One connection, one supervisor. The server ends when standard input reaches
//! end of file, or on SIGINT, SIGTERM, SIGHUP or SIGQUIT. Either way it stops
//! accepting calls, stops every worker and everything they started, waits for
//! that (bounded by the supervisor's own shutdown bound) and only then lets the
//! runtime drop and exits.
//!
//! Standard output carries protocol messages and nothing else. Diagnostics go
//! to standard error through the queue and writer thread `delega run` uses
//! ([`super::run::log`]), which never waits on standard error and takes its
//! lock for one line at a time.
//!
//! Exit status: 0 after end of input, 130 after a signal (the status `delega
//! run` gives an interrupted run, for all four signals), 1 when the workers
//! could not be confirmed stopped or the supervisor itself had failed (after
//! either, and before 0 and 130) or reading or writing the transport failed
//! after end of input, and 2 when the server
//! could not start, including when an `--env` is one `delega run` would refuse
//! (the status `delega run` gives when nothing was started).
//!
//! The first signal starts the stop. A signal that arrives while it goes on is
//! acknowledged once on standard error and otherwise ignored, as in `delega
//! run`, so a second Ctrl-C cannot end the server while workers are still being
//! stopped. SIGKILL cannot be handled: each worker's keeper then stops that
//! worker's tree by itself, as for `delega run`.

use std::process::ExitCode;

use clap::Parser;
use delega_core::contract::applied::AppliedConfig;
use delega_core::contract::outcome::Outcome;
use delega_core::contract::partial::PartialRequest;
use delega_core::provider::ProviderOptions;
use delega_core::supervisor::{Supervisor, SupervisorConfig};
use delega_mcp::Ended;
use tokio::runtime::Builder;

use super::run::log::Log;
use super::run::signals::Signals;
use super::{EXIT_REFUSED, apply_env, guard_command, keeper_command, version};

/// What `delega run` exits with for an interrupted run, and so for a signal.
const EXIT_SIGNALLED: u8 = Outcome::Interrupted.exit_status();
/// The workers were not confirmed stopped, or the transport failed.
const EXIT_FAILED: u8 = 1;

/// Said once when a signal arrives while the workers are being stopped.
const STILL_STOPPING: &str =
    "delega mcp: still stopping the workers; waiting for cleanup to finish";

#[derive(Debug, Parser)]
pub struct Args {
    /// Give every worker an environment variable (repeatable). NAME passes this
    /// environment's value; NAME=VALUE sets it. Workers receive a fixed set
    /// only (user, shell, locale, proxy and certificate variables and the
    /// provider's own) plus what is given here. Prefer NAME for secrets: a
    /// value on the command line shows in the process table and shell history.
    /// A name that is not valid, that describes the calling session or that
    /// starts with DELEGA_ is refused at start-up, as for `delega run`, and no
    /// value is ever repeated
    #[arg(short = 'e', long = "env", value_name = "NAME[=VALUE]")]
    env: Vec<String>,
}

pub fn run(args: Args) -> ExitCode {
    // Recorded first, before anything else is read, so that `list` and the
    // identity resource can say when this executable is replaced or removed
    // later. A replacement between the start of the process and this line is
    // recorded as the file the server runs and is never reported. When the
    // file cannot be recorded the server never reports itself stale.
    let executable = delega_mcp::Executable::current();
    let log = Log::start();
    let code = go(&args, executable, &log);
    // Bounded: a blocked standard error cannot hold the exit.
    log.finish();
    code
}

fn go(args: &Args, executable: Option<delega_mcp::Executable>, log: &Log) -> ExitCode {
    // The configuration file is read once, now: a file that cannot be used
    // refuses to start the server, as a bad `--env` does.
    let status = super::config::read();
    let file = status.applied.clone();
    let config = match status.into_config() {
        Ok(config) => config,
        Err(refusal) => {
            log.line(format!("delega mcp: refused: {refusal}"));
            return ExitCode::from(EXIT_REFUSED);
        }
    };
    // What `delega run` reads for provider options, read the same way: the
    // provider executable overrides (`DELEGA_CLAUDE_BIN`, `DELEGA_CODEX_BIN`)
    // by the provider layer from this process's environment, and the command
    // guard from this executable's own path.
    let mut options = ProviderOptions {
        system_paths: super::system_paths(),
        hook: guard_command(),
        keeper: keeper_command(),
        ..ProviderOptions::default()
    };
    config.apply_to_options(&mut options);
    apply_env(&mut options, &args.env);
    // The names are checked now, with the rules and messages `delega run` uses,
    // so a bad `--env` is a refusal to start and not a server whose every `start`
    // is refused. A name only one provider's adapter owns is still refused when
    // a worker for that provider starts. No value is ever written.
    if let Err(refusal) =
        delega_core::provider::env::check_caller_names(&options.env_pass, &options.env_set)
    {
        log.line(format!("delega mcp: refused: {refusal}"));
        return ExitCode::from(EXIT_REFUSED);
    }
    let supervisor_config = SupervisorConfig {
        limits: config.limits(),
        provider: options,
        ..SupervisorConfig::default()
    };
    // One thread for the supervisor's asynchronous input and output and
    // timers. The checks that admit a run, and building each run's plan when
    // it is dispatched, run on threads of their own within the preflight
    // bound. Reading a profile for `start` runs on a blocking thread before
    // admission, outside that bound. Starting the keeper and the provider's
    // probes and process still happens on this thread, so a filesystem that
    // stops answering there can still hold the server.
    let runtime = match Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(err) => {
            log.line(format!("delega mcp: cannot start the async runtime: {err}"));
            return ExitCode::from(EXIT_REFUSED);
        }
    };
    let code = runtime.block_on(serve(
        supervisor_config,
        config.request_defaults(),
        file,
        executable,
        log,
    ));
    // Every worker was stopped, or reported not to be, before this point. The
    // thread that reads standard input may still be blocked in `read`, and
    // dropping the runtime normally would wait for it.
    runtime.shutdown_background();
    code
}

async fn serve(
    config: SupervisorConfig,
    defaults: PartialRequest,
    file: Option<AppliedConfig>,
    executable: Option<delega_mcp::Executable>,
    log: &Log,
) -> ExitCode {
    let signals = match Signals::install() {
        Ok(signals) => signals,
        Err(err) => {
            log.line(format!(
                "delega mcp: cannot install the signal handlers that stop the workers: {err}"
            ));
            return ExitCode::from(EXIT_REFUSED);
        }
    };
    let supervisor = Supervisor::new(config);
    let router = match delega_mcp::router_with_executable(
        supervisor.clone(),
        &version::build_info(),
        defaults,
        file,
        executable,
    ) {
        Ok(router) => router,
        Err(err) => {
            log.line(format!("delega mcp: cannot build the server: {err}"));
            supervisor.shutdown().await;
            return ExitCode::from(EXIT_REFUSED);
        }
    };
    let (first, stop) = tokio::sync::oneshot::channel::<()>();
    let watcher = tokio::spawn(watch(signals, first, log.clone()));
    log.line(format!("delega mcp: {} ready", delega_core::VERSION));
    let served = delega_mcp::serve(router, &supervisor, async {
        let _ = stop.await;
    })
    .await;
    watcher.abort();

    // Workers that could not be confirmed stopped, or a supervisor that failed,
    // make the status 1 whatever ended the server. A failing transport does so
    // after end of input only: after a signal the status stays the signal's, a
    // client that stopped reading standard output being no failure of the workers.
    let mut workers_failed = false;
    if let Some(error) = &served.transport_error {
        log.line(format!("delega mcp: transport error: {error}"));
    }
    if let Some(why) = &served.shutdown.failed {
        log.line(format!("delega mcp: supervisor failed: {why}"));
        workers_failed = true;
    }
    if !served.shutdown.unsettled.is_empty() {
        log.line(format!(
            "delega mcp: cleanup not confirmed for {} run(s): {}",
            served.shutdown.unsettled.len(),
            served.shutdown.unsettled.join(", ")
        ));
        workers_failed = true;
    }
    for (run, cleanup) in &served.unconfirmed {
        if let delega_core::contract::result::Cleanup::Unconfirmed {
            pids,
            process_groups,
            reason,
        } = cleanup
        {
            log.line(format!(
                "delega mcp: cleanup not confirmed for run {run}: {reason} (pids {pids:?}, \
                 process groups {process_groups:?})"
            ));
            workers_failed = true;
        }
    }
    match served.ended {
        _ if workers_failed => ExitCode::from(EXIT_FAILED),
        Ended::Stopped => ExitCode::from(EXIT_SIGNALLED),
        Ended::EndOfInput if served.transport_error.is_some() => ExitCode::from(EXIT_FAILED),
        Ended::EndOfInput => ExitCode::SUCCESS,
    }
}

/// Wait for the first signal and tell the server, then swallow the rest,
/// saying so once. The handlers stay installed while the workers are stopped,
/// so no later signal takes its default action.
async fn watch(mut signals: Signals, first: tokio::sync::oneshot::Sender<()>, log: Log) {
    if signals.next().await.is_none() {
        // The handlers stopped delivering; stop anyway.
        let _ = first.send(());
        return;
    }
    log.line("delega mcp: signal received, stopping the workers");
    let _ = first.send(());
    let mut told = false;
    while signals.next().await.is_some() {
        if !told {
            told = true;
            log.line(STILL_STOPPING);
        }
    }
    // Handlers gone: nothing left to swallow.
    std::future::pending::<()>().await;
}
