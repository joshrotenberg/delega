//! Acknowledgements from the supervisor task for tests of parked waits.

use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

use super::{Supervisor, command::Command};

const ACK_DEADLINE: Duration = Duration::from_secs(15);

/// Receives one acknowledgement for each wait the actor registers.
pub struct WaitObserver(mpsc::UnboundedReceiver<RegisteredWait>);

impl WaitObserver {
    /// Wait for the next registration. The deadline only guards against a
    /// missing acknowledgement.
    pub async fn registered(&mut self) -> RegisteredWait {
        tokio::time::timeout(ACK_DEADLINE, self.0.recv())
            .await
            .expect("missing supervisor wait registration acknowledgement")
            .expect("supervisor wait observer closed before registration")
    }
}

/// A registered wait, with its own removal acknowledgement.
pub struct RegisteredWait(oneshot::Receiver<()>);

impl RegisteredWait {
    /// Wait until this registration leaves the actor's wait table.
    pub async fn removed(self) {
        tokio::time::timeout(ACK_DEADLINE, self.0)
            .await
            .expect("missing supervisor wait removal acknowledgement")
            .expect("supervisor wait removal acknowledgement was dropped");
    }
}

impl Supervisor {
    /// Observe registrations on this supervisor from this point onward.
    /// Install before starting the waits under test.
    #[doc(hidden)]
    pub fn observe_waits(&self) -> WaitObserver {
        let (tx, rx) = mpsc::unbounded_channel();
        self.shared
            .tx
            .send(Command::ObserveWaits(tx))
            .expect("the supervisor's task is running");
        WaitObserver(rx)
    }
}

/// Lives in the registered waiter, so every removal path sends exactly once.
pub(super) struct RemovalAck(Option<oneshot::Sender<()>>);

impl Drop for RemovalAck {
    fn drop(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(());
        }
    }
}

pub(super) fn acknowledge_registration(
    observer: &mpsc::UnboundedSender<RegisteredWait>,
) -> Option<RemovalAck> {
    if observer.is_closed() {
        return None;
    }
    let (tx, rx) = oneshot::channel();
    observer.send(RegisteredWait(rx)).ok()?;
    Some(RemovalAck(Some(tx)))
}
