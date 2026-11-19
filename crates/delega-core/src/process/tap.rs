//! Delivery of standard output chunks to a [`stdout_tap`](super::ProcessSpec).
//!
//! The owner loop must never wait for a tap, because the time limit, stop
//! requests and capture all run in that loop. Chunks therefore go to the tap
//! through a bounded queue, with `try_send`, to a thread of its own. A full
//! queue drops the chunk and counts it; a tap that panics is dropped and the
//! rest of the output is discarded without it. Capture does not depend on any
//! of this.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};

use tokio::sync::oneshot;

use super::limits::{TAP_DRAIN_WAIT, TAP_QUEUE_CHUNKS};
use super::owner::Shared;
use super::spec::OutputTap;

/// The queue to a tap's thread.
pub(super) struct TapWorker {
    sender: SyncSender<Vec<u8>>,
    dropped: Arc<AtomicU64>,
    panicked: Arc<AtomicU64>,
    done: oneshot::Receiver<()>,
}

impl TapWorker {
    pub(super) fn start(tap: OutputTap, counter: Option<Arc<AtomicU64>>) -> Self {
        let (sender, receiver) = sync_channel::<Vec<u8>>(TAP_QUEUE_CHUNKS);
        let (done_tx, done) = oneshot::channel();
        let panicked = Arc::new(AtomicU64::new(0));
        let thread_panicked = panicked.clone();
        std::thread::spawn(move || {
            let mut alive = true;
            for chunk in receiver {
                if alive && catch_unwind(AssertUnwindSafe(|| tap(&chunk))).is_err() {
                    alive = false;
                    thread_panicked.store(1, Ordering::SeqCst);
                }
            }
            let _ = done_tx.send(());
        });
        Self {
            sender,
            dropped: counter.unwrap_or_default(),
            panicked,
            done,
        }
    }

    /// Queue a chunk without waiting. A full queue drops it.
    pub(super) fn offer(&self, chunk: &[u8]) {
        if let Err(TrySendError::Full(_)) = self.sender.try_send(chunk.to_vec()) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Let the tap see what is queued, for at most [`TAP_DRAIN_WAIT`], and
    /// record in the report's warnings anything it missed. A tap that is still
    /// busy afterwards is left behind.
    pub(super) async fn finish(self, shared: &Shared) {
        let Self {
            sender,
            dropped,
            panicked,
            done,
        } = self;
        drop(sender);
        let drained = tokio::time::timeout(TAP_DRAIN_WAIT, done).await.is_ok();
        let dropped = dropped.load(Ordering::Relaxed);
        if dropped > 0 {
            shared.warn(format!(
                "{dropped} chunks of standard output were not delivered to the output tap because it was too slow"
            ));
        }
        if panicked.load(Ordering::SeqCst) > 0 {
            shared.warn("the output tap panicked and received no more output".to_string());
        }
        if !drained {
            shared.warn(format!(
                "the output tap had not finished {TAP_DRAIN_WAIT:?} after the run and was left behind"
            ));
        }
    }
}
