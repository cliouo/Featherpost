//! User-initiated graceful stop, installed only by the platform Agent host.
use cordis::Service;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Grace period between `stopManagedAgent` and the final stop signal. Kept well below the
/// 1s bound asserted in `api/agent_control_tests.rs`.
pub(crate) const STOP_DRAIN_WINDOW: std::time::Duration = std::time::Duration::from_millis(350);

#[derive(Default)]
pub(crate) struct ManagedAgentControl {
    requested: AtomicBool,
    stopped: tokio::sync::Notify,
}

impl Service for ManagedAgentControl {
    fn name(&self) -> &'static str {
        "agent.managed-control"
    }
}

impl ManagedAgentControl {
    pub(crate) fn request_stop(self: &Arc<Self>) {
        if self.requested.swap(true, Ordering::AcqRel) {
            return;
        }
        let control = self.clone();
        tokio::spawn(async move {
            // Drain window: the initiating local RPC gets its acknowledgement, and connected
            // frontends (already pushed `agent/stopping`) flush drafts and drop their Dock icon
            // before stream sessions are torn down.
            tokio::time::sleep(STOP_DRAIN_WINDOW).await;
            control.stopped.notify_one();
        });
    }

    pub(crate) async fn wait(&self) {
        self.stopped.notified().await;
    }
}
