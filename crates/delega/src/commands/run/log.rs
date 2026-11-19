//! Everything `delega run` writes to standard error goes through here.
//!
//! Standard error can block: a caller that does not read it fills the pipe and
//! every write waits. Nothing that stops a worker may wait on that, so this
//! module is the only writer, and it never blocks its callers. Lines go into a
//! bounded queue with a non-blocking send; a thread of its own writes them. A
//! full queue drops the line and counts it. When the run is over, [`Log::finish`]
//! waits a bounded time for the queue to drain, then returns whether or not
//! the thread got through, so the result on standard output and the exit
//! status never wait on standard error.

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use delega_core::provider::{ProgressEvent, ProgressSink};

use super::output::{outcome_name, provider_name};

/// Most lines waiting to be written. A line that finds the queue full is
/// dropped and counted.
const QUEUE_LINES: usize = 1024;
/// How long [`Log::finish`] waits for the writer to drain the queue.
const FINISH_WAIT: Duration = Duration::from_secs(1);
/// How often the writer checks whether the run is over while the queue is
/// empty.
const IDLE_POLL: Duration = Duration::from_millis(20);

struct Inner {
    sender: SyncSender<String>,
    dropped: Arc<AtomicU64>,
    /// Chunks of provider output the provider layer reported as dropped before
    /// they became events.
    tap_dropped: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    done: Mutex<Option<Receiver<()>>>,
}

/// The queue of lines for standard error. Cheap to clone.
#[derive(Clone)]
pub struct Log(Arc<Inner>);

impl Log {
    /// Start the writer thread.
    pub fn start() -> Self {
        let (sender, receiver) = sync_channel::<String>(QUEUE_LINES);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let dropped = Arc::new(AtomicU64::new(0));
        let tap_dropped = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let counts = (dropped.clone(), tap_dropped.clone());
        let thread_stop = stop.clone();
        std::thread::spawn(move || {
            writer(&receiver, &counts.0, &counts.1, &thread_stop);
            let _ = done_tx.send(());
        });
        Self(Arc::new(Inner {
            sender,
            dropped,
            tap_dropped,
            stop,
            done: Mutex::new(Some(done_rx)),
        }))
    }

    /// Queue one line. Never waits.
    pub fn line(&self, text: impl Into<String>) {
        if self.0.sender.try_send(text.into()).is_err() {
            self.0.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The sink that turns provider progress events into `progress:` lines.
    pub fn progress_sink(&self) -> ProgressSink {
        let log = self.clone();
        Arc::new(move |event| match event {
            ProgressEvent::Dropped { chunks } => {
                log.0.tap_dropped.fetch_add(chunks, Ordering::Relaxed);
            }
            other => log.line(progress_line(&other)),
        })
    }

    /// Give the writer a bounded time to write what is queued. Returns
    /// whether it finished; a writer stuck on a blocked standard error is left
    /// to end with the process.
    pub fn finish(&self) -> bool {
        self.0.stop.store(true, Ordering::SeqCst);
        let done = self
            .0
            .done
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        done.is_some_and(|done| done.recv_timeout(FINISH_WAIT).is_ok())
    }
}

fn writer(
    receiver: &Receiver<String>,
    dropped: &AtomicU64,
    tap_dropped: &AtomicU64,
    stop: &AtomicBool,
) {
    let mut err = std::io::stderr().lock();
    loop {
        match receiver.recv_timeout(IDLE_POLL) {
            Ok(line) => {
                // A closed standard error is not worth failing the run for.
                let _ = writeln!(err, "{line}");
            }
            Err(RecvTimeoutError::Timeout) if !stop.load(Ordering::SeqCst) => {}
            Err(_) => break,
        }
    }
    // Said last, straight to standard error, so it cannot itself be dropped.
    let lost = dropped.load(Ordering::Relaxed) + tap_dropped.load(Ordering::Relaxed);
    if lost > 0 {
        let _ = writeln!(
            err,
            "progress: {lost} events or output chunks were dropped: standard error was not read \
             fast enough, or the provider wrote faster than progress could be reported"
        );
    }
}

/// One progress line. Message events carry a length, never text; a denied
/// request carries what the result lists as denied.
fn progress_line(event: &ProgressEvent) -> String {
    match event {
        ProgressEvent::Started { provider, program } => {
            format!("progress: started {} ({program})", provider_name(*provider))
        }
        ProgressEvent::Provider { kind, summary } if summary.is_empty() => {
            format!("progress: {kind}")
        }
        ProgressEvent::Provider { kind, summary } => format!("progress: {kind}: {summary}"),
        ProgressEvent::Denied { request } => format!("progress: denied: {request}"),
        ProgressEvent::Dropped { .. } => String::new(),
        ProgressEvent::Finished { outcome } => {
            format!("progress: finished: {}", outcome_name(*outcome))
        }
    }
}
