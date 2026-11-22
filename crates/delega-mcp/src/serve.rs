//! Serving the router on standard input and output, and stopping.

use std::future::Future;
use std::time::Duration;

use delega_core::contract::result::Cleanup;
use delega_core::supervisor::{ShutdownReport, Supervisor};
use tower_mcp::{McpRouter, StdioTransport};

/// Most requests handled at once.
const MAX_CONCURRENT_REQUESTS: usize = 128;
// A `wait` can hold a request for the whole wait ceiling. The supervisor lets
// at most `max_waits_in_flight` of them block, and the configuration file can
// lower that and cannot raise it, so at least half of the requests stay free
// for `interrupt`, `close`, `start` and the reads. A build that changes either
// number so that this no longer holds does not compile.
const _: () = assert!(
    delega_core::supervisor::limits::MAX_WAITS_IN_FLIGHT <= MAX_CONCURRENT_REQUESTS / 2,
    "the default limit on waits in flight must leave half of the request slots to other calls"
);
const _: () = assert!(
    delega_core::config::MAX_WAITS_IN_FLIGHT_BOUND
        <= delega_core::supervisor::limits::MAX_WAITS_IN_FLIGHT,
    "the configuration file must not raise the limit on waits in flight"
);
/// Largest input frame. A prompt may be 4 MiB before JSON escaping, so the
/// frame limit leaves room for it and for the other fields.
const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;
/// How long the transport waits for calls still running once it has stopped
/// reading. The supervisor's shutdown releases them first, so this only bounds
/// a handler that does not return.
const DRAIN: Duration = Duration::from_secs(10);
/// How long past the drain the server waits for the transport once the workers
/// are stopped. The transport writes each response inside its read loop, so a
/// client that stopped reading standard output (the pipe full, the host frozen)
/// blocks that write, and the loop never sees the stop; the drain limit only
/// covers the phase after the loop. The workers are gone by then, so the server
/// gives up on the transport and returns.
const TRANSPORT_GRACE: Duration = Duration::from_secs(5);

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
    /// Every run on the connection whose cleanup was not confirmed, with what
    /// the result says could not be confirmed. A run counts when it settled, so
    /// it stays listed after a later run on its worker replaced it as the latest
    /// and after retention dropped its result. Read after the workers were
    /// stopped: a run stopped by the shutdown that the process layer could not
    /// confirm gone is listed here, though the shutdown itself settled.
    pub unconfirmed: Vec<(String, Cleanup)>,
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
/// because their runs end, and waits for the transport at most the drain limit
/// plus a short grace. Only then does it return, and the caller drops the
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
    let transport_error = match finished {
        Some(result) => result.err().map(|error| error.to_string()),
        None => match tokio::time::timeout(DRAIN + TRANSPORT_GRACE, &mut run).await {
            Ok(result) => result.err().map(|error| error.to_string()),
            Err(_) => Some(
                "the transport did not finish after the workers were stopped; a client that \
                 does not read standard output can hold it, and it was given up on"
                    .to_string(),
            ),
        },
    };
    Served {
        ended,
        shutdown: report,
        unconfirmed: supervisor.unconfirmed().await.unwrap_or_default(),
        transport_error,
    }
}
