/// Stable lifecycle checkpoints for deterministic worker/runtime tests.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WorkerEvent {
    ListenerStarted,
    ListenerStopped,
    OutboxStarted { role_epoch: u64 },
    OutboxCycleStarted { role_epoch: u64 },
    OutboxCycleFinished { role_epoch: u64 },
    OutboxStopped { role_epoch: u64 },
}

pub(crate) trait WorkerObserver: Send + Sync {
    fn observe(&self, event: WorkerEvent);
}

#[derive(Debug, Default)]
pub(crate) struct NoopWorkerObserver;

impl WorkerObserver for NoopWorkerObserver {
    fn observe(&self, _event: WorkerEvent) {}
}
