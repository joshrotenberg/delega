//! Serving the router on standard input and output, and stopping.

use std::future::Future;
use std::time::Duration;

use delega_core::supervisor::{ShutdownReport, Supervisor};
use tower_mcp::{McpRouter, StdioTransport};

/// Most requests handled at once.
const MAX_CONCURRENT_REQUESTS: usize = 128;
/// Largest input frame. A prompt may be 4 MiB before JSON escaping, so the
/// frame limit leaves room for it and for the other fields.
const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;
/// How long the transport waits for calls still running once it has stopped
/// reading. The supervisor's shutdown releases them first, so this only bounds
/// a handler that does not return.
const DRAIN: Duration = Duration::from_secs(10);

/// Why the server stopped accepting calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// Standard input reached end of file, or could not be read.
    EndOfInput,
    /// The caller's stop signal arrived first.
    Stopped,
}

/// How a served connection ended.
#[derive(Debug)]
pub struct Served {
    /// What ended it.
    pub ended: Ended,
    /// What stopping the workers found. The report says whether every run
    /// stopped and whether the supervisor itself had failed.
    pub shutdown: ShutdownReport,
    /// The transport's error, if reading or writing failed. End of file is not
    /// an error.
    pub transport_error: Option<String>,
}

/// Serve `router` on standard input and output until standard input ends or
/// `stop` resolves, then stop every worker.
///
/// The order is fixed. The server stops reading, so no new call is accepted.
/// It then calls [`Supervisor::shutdown`] and waits for it, bounded by the
/// supervisor's own shutdown bound, while calls still running are released
/// because their runs end. Only then does it return, and the caller drops the
/// runtime. Standard output carries protocol messages and nothing else; this
/// function writes nothing to either stream.
pub async fn serve(
    router: McpRouter,
    supervisor: &Supervisor,
    stop: impl Future<Output = ()>,
) -> Served {
    let mut transport = StdioTransport::new(router)
        .max_concurrent_requests(MAX_CONCURRENT_REQUESTS)
        .max_frame_len(MAX_FRAME_BYTES)
        .drain_timeout(DRAIN);
    let handle = transport.handle();
    let run = transport.run();
    tokio::pin!(run);
    tokio::pin!(stop);
    let mut finished = None;
    let ended = tokio::select! {
        result = &mut run => {
            finished = Some(result);
            Ended::EndOfInput
        }
        () = handle.stopping() => Ended::EndOfInput,
        () = &mut stop => {
            let _ = handle.shutdown();
            Ended::Stopped
        }
    };
    // The transport keeps draining calls that are still running while the
    // workers are stopped, so both are driven together.
    let shutdown = supervisor.shutdown();
    tokio::pin!(shutdown);
    let report = loop {
        tokio::select! {
            report = &mut shutdown => break report,
            result = &mut run, if finished.is_none() => finished = Some(result),
        }
    };
    let result = match finished {
        Some(result) => result,
        None => run.await,
    };
    Served {
        ended,
        shutdown: report,
        transport_error: result.err().map(|error| error.to_string()),
    }
}
