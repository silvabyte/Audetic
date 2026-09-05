use async_trait::async_trait;
use tokio::sync::{oneshot, Notify};
use tokio_util::sync::CancellationToken;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::sync::clock::SyncClock;
use crate::sync::observer::{WorkerEvent, WorkerObserver};

use super::watchdog;

struct Sleeper {
    deadline: chrono::DateTime<chrono::Utc>,
    wake: oneshot::Sender<()>,
}

struct ClockState {
    now: chrono::DateTime<chrono::Utc>,
    sleepers: Vec<Sleeper>,
}

#[derive(Clone)]
pub(super) struct ManualClock {
    state: Arc<Mutex<ClockState>>,
    registered: Arc<Notify>,
}

impl ManualClock {
    pub(super) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(ClockState {
                now: chrono::DateTime::parse_from_rfc3339("2035-01-02T03:04:05Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
                sleepers: Vec::new(),
            })),
            registered: Arc::new(Notify::new()),
        }
    }

    pub(super) fn advance(&self, duration: Duration) {
        let delta = chrono::Duration::from_std(duration).unwrap();
        let mut state = self.state.lock().unwrap();
        state.now += delta;
        let now = state.now;
        let mut retained = Vec::new();
        for sleeper in state.sleepers.drain(..) {
            if sleeper.wake.is_closed() {
                continue;
            }
            if sleeper.deadline <= now {
                let _ = sleeper.wake.send(());
            } else {
                retained.push(sleeper);
            }
        }
        state.sleepers = retained;
    }

    pub(super) async fn wait_for_sleepers(&self, minimum: usize) {
        watchdog("waiting for manual-clock sleepers", async {
            loop {
                let notified = self.registered.notified();
                let ready = {
                    let mut state = self.state.lock().unwrap();
                    state.sleepers.retain(|sleeper| !sleeper.wake.is_closed());
                    state.sleepers.len() >= minimum
                };
                if ready {
                    return;
                }
                notified.await;
            }
        })
        .await;
    }
}

#[async_trait]
impl SyncClock for ManualClock {
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        self.state.lock().unwrap().now
    }

    async fn sleep(&self, duration: Duration, cancellation: &CancellationToken) {
        let (wake, wait) = oneshot::channel();
        {
            let mut state = self.state.lock().unwrap();
            let delta = chrono::Duration::from_std(duration).unwrap();
            let deadline = state.now + delta;
            state.sleepers.push(Sleeper { deadline, wake });
        }
        self.registered.notify_waiters();
        tokio::select! {
            _ = wait => {}
            _ = cancellation.cancelled() => {}
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct WorkerProbe {
    events: Arc<Mutex<Vec<WorkerEvent>>>,
    changed: Arc<Notify>,
}

impl WorkerProbe {
    pub(super) fn events(&self) -> Vec<WorkerEvent> {
        self.events.lock().unwrap().clone()
    }

    pub(super) fn successful_cycles(&self, role_epoch: u64) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    WorkerEvent::OutboxCycleSucceeded {
                        role_epoch: event_epoch
                    } if *event_epoch == role_epoch
                )
            })
            .count()
    }

    pub(super) async fn wait_for(&self, predicate: impl Fn(&[WorkerEvent]) -> bool) {
        watchdog("waiting for worker event", async {
            loop {
                let notified = self.changed.notified();
                if predicate(&self.events.lock().unwrap()) {
                    return;
                }
                notified.await;
            }
        })
        .await;
    }
}

impl WorkerObserver for WorkerProbe {
    fn observe(&self, event: WorkerEvent) {
        self.events.lock().unwrap().push(event);
        self.changed.notify_waiters();
    }
}
