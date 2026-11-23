//! Reliable line delivery for providers that keep standard input open.

use std::sync::Arc;

use tokio::sync::Notify;

/// The application protocol driven beside process I/O.
pub(crate) trait Dialogue: Send + Sync {
    /// Set the process deadline before reading or writing.
    fn started(&self, deadline: tokio::time::Instant);
    /// Consume a complete line. Return true to keep it in ordinary output.
    fn line(&self, line: &[u8]) -> bool;
    /// Pending responses are ready or the input pipe should close.
    fn changed(&self) -> &Notify;
    /// Take queued responses and whether to close the input pipe afterwards.
    fn responses(&self) -> (Vec<(usize, Vec<u8>)>, bool);
    fn written(&self, key: usize);
    fn end(&self, reason: End);
}

pub(crate) type DialogueHandle = Arc<dyn Dialogue>;

#[derive(Clone, Copy)]
pub(crate) enum End {
    Interrupted,
    TimedOut,
    ProviderClosed,
    ProtocolError,
}
