//! `--progress`: one line per provider event on standard error.
//!
//! The provider layer calls the sink from the task that reads the worker's
//! output, so the sink only queues the event. A thread of its own writes the
//! lines, and a slow or blocked reader of standard error cannot hold the run or
//! its time limit. Standard output is never written here.

use std::io::Write;
use std::sync::Arc;
use std::sync::mpsc::{Sender, channel};
use std::thread::JoinHandle;

use delega_core::provider::{ProgressEvent, ProgressSink};

use super::output::{outcome_name, provider_name};

/// What the writer thread receives.
enum Message {
    Event(ProgressEvent),
    /// The run is over; write what is queued and end.
    Stop,
}

/// The writer thread and the way to feed it.
pub struct Printer {
    sender: Sender<Message>,
    thread: JoinHandle<()>,
}

impl Printer {
    /// Start the writer thread.
    pub fn start() -> Self {
        let (sender, receiver) = channel::<Message>();
        let thread = std::thread::spawn(move || {
            for message in receiver {
                let Message::Event(event) = message else {
                    break;
                };
                let mut err = std::io::stderr().lock();
                // A closed standard error is not worth failing the run for.
                let _ = writeln!(err, "{}", line(&event));
            }
        });
        Self { sender, thread }
    }

    /// The sink to give the provider layer.
    pub fn sink(&self) -> ProgressSink {
        let sender = self.sender.clone();
        Arc::new(move |event| {
            let _ = sender.send(Message::Event(event));
        })
    }

    /// Write every queued line and stop the thread. Events sent after this
    /// call are dropped.
    pub fn finish(self) {
        let _ = self.sender.send(Message::Stop);
        let _ = self.thread.join();
    }
}

/// One progress line. It holds the provider's event kind and a short summary,
/// never a command or an environment value.
fn line(event: &ProgressEvent) -> String {
    match event {
        ProgressEvent::Started { provider, program } => {
            format!("progress: started {} ({program})", provider_name(*provider))
        }
        ProgressEvent::Provider { kind, summary } if summary.is_empty() => {
            format!("progress: {kind}")
        }
        ProgressEvent::Provider { kind, summary } => format!("progress: {kind}: {summary}"),
        ProgressEvent::Denied { request } => format!("progress: denied: {request}"),
        ProgressEvent::Finished { outcome } => {
            format!("progress: finished: {}", outcome_name(*outcome))
        }
    }
}
