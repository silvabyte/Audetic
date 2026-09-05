use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use std::time::Duration;

/// Time boundary used by durable sync workers.
///
/// Keeping both scheduling time and sleeping behind one adapter prevents tests
/// from advancing one notion of time while a worker remains blocked on another.
#[async_trait]
pub(crate) trait SyncClock: Send + Sync {
    fn now(&self) -> chrono::DateTime<chrono::Utc>;

    async fn sleep(&self, duration: Duration, cancellation: &CancellationToken);
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SystemSyncClock;

#[async_trait]
impl SyncClock for SystemSyncClock {
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now()
    }

    async fn sleep(&self, duration: Duration, cancellation: &CancellationToken) {
        tokio::select! {
            _ = tokio::time::sleep(duration) => {}
            _ = cancellation.cancelled() => {}
        }
    }
}
