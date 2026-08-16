//! Default-device notifications and shared Settled Switch policy.

use std::future::Future;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::mpsc;

const SETTLE_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub(crate) enum RawDeviceSwitch {
    Input,
    Output,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SettledSwitch {
    pub input_changed: bool,
    pub output_changed: bool,
}

pub(crate) type RawSwitchSink = Arc<dyn Fn(RawDeviceSwitch) + Send + Sync + 'static>;

pub(crate) trait ActiveDeviceWatcher: Send {}

pub(crate) trait DeviceWatcherBackend: Send {
    fn start(self: Box<Self>, sink: RawSwitchSink) -> Result<Box<dyn ActiveDeviceWatcher>>;
}

pub(crate) struct DeviceWatcher {
    active: Option<Box<dyn ActiveDeviceWatcher>>,
    debounce_task: tokio::task::JoinHandle<()>,
}

impl DeviceWatcher {
    pub(crate) fn start<F, Fut>(on_settled: F) -> Result<Self>
    where
        F: Fn(SettledSwitch) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        Self::start_with_backend(platform::backend(), on_settled)
    }

    #[cfg(test)]
    pub(crate) fn with_backend<F, Fut>(
        backend: Box<dyn DeviceWatcherBackend>,
        on_settled: F,
    ) -> Result<Self>
    where
        F: Fn(SettledSwitch) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        Self::start_with_backend(backend, on_settled)
    }

    fn start_with_backend<F, Fut>(
        backend: Box<dyn DeviceWatcherBackend>,
        on_settled: F,
    ) -> Result<Self>
    where
        F: Fn(SettledSwitch) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let pending = Arc::new(AtomicU8::new(0));
        let generation = Arc::new(AtomicU64::new(0));
        let (raw_tx, mut raw_rx) = mpsc::channel(1);
        let callback_pending = pending.clone();
        let callback_generation = generation.clone();
        let active = backend.start(Arc::new(move |event| {
            callback_generation.fetch_add(1, Ordering::SeqCst);
            callback_pending.fetch_or(event.bit(), Ordering::SeqCst);
            let _ = raw_tx.try_send(());
        }))?;
        let debounce_task = tokio::spawn(async move {
            while raw_rx.recv().await.is_some() {
                let mut observed_generation = generation.load(Ordering::SeqCst);
                loop {
                    match tokio::time::timeout(SETTLE_INTERVAL, raw_rx.recv()).await {
                        Ok(Some(())) => {
                            observed_generation = generation.load(Ordering::SeqCst);
                        }
                        Ok(None) => return,
                        Err(_) => break,
                    }
                }

                let bits = pending.swap(0, Ordering::SeqCst);
                if generation.load(Ordering::SeqCst) != observed_generation {
                    pending.fetch_or(bits, Ordering::SeqCst);
                    continue;
                }
                if bits != 0 {
                    on_settled(SettledSwitch::from_bits(bits)).await;
                }
            }
        });

        Ok(Self {
            active: Some(active),
            debounce_task,
        })
    }
}

impl RawDeviceSwitch {
    fn bit(self) -> u8 {
        match self {
            RawDeviceSwitch::Input => 1,
            RawDeviceSwitch::Output => 2,
        }
    }
}

impl SettledSwitch {
    fn from_bits(bits: u8) -> Self {
        Self {
            input_changed: bits & 1 != 0,
            output_changed: bits & 2 != 0,
        }
    }
}

impl Drop for DeviceWatcher {
    fn drop(&mut self) {
        // Stop native callbacks before cancelling their Rust receiver.
        drop(self.active.take());
        self.debounce_task.abort();
    }
}

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;

#[cfg(not(target_os = "macos"))]
mod stub;
#[cfg(not(target_os = "macos"))]
use stub as platform;

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::*;
    use crate::app::DaemonCommand;
    use crate::audio::stream_event::{CaptureSource, StreamDeath, StreamGeneration};

    #[derive(Clone)]
    struct FakeBackendHandle {
        sink: Arc<Mutex<Option<RawSwitchSink>>>,
        drops: Arc<AtomicUsize>,
    }

    impl FakeBackendHandle {
        fn emit(&self, event: RawDeviceSwitch) {
            self.sink.lock().unwrap().as_ref().unwrap()(event);
        }
    }

    struct FakeBackend(FakeBackendHandle);

    struct FakeActiveWatcher {
        sink: Arc<Mutex<Option<RawSwitchSink>>>,
        drops: Arc<AtomicUsize>,
    }

    impl ActiveDeviceWatcher for FakeActiveWatcher {}

    impl Drop for FakeActiveWatcher {
        fn drop(&mut self) {
            self.sink.lock().unwrap().take();
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl DeviceWatcherBackend for FakeBackend {
        fn start(self: Box<Self>, sink: RawSwitchSink) -> Result<Box<dyn ActiveDeviceWatcher>> {
            *self.0.sink.lock().unwrap() = Some(sink);
            Ok(Box::new(FakeActiveWatcher {
                sink: self.0.sink.clone(),
                drops: self.0.drops.clone(),
            }))
        }
    }

    impl FakeBackendHandle {
        fn new() -> Self {
            Self::default()
        }
    }

    impl Default for FakeBackendHandle {
        fn default() -> Self {
            Self {
                sink: Arc::new(Mutex::new(None)),
                drops: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    fn start_fake_watcher(
        backend: FakeBackendHandle,
    ) -> (DeviceWatcher, mpsc::UnboundedReceiver<SettledSwitch>) {
        let (settled_tx, settled_rx) = mpsc::unbounded_channel();
        let watcher = DeviceWatcher::with_backend(Box::new(FakeBackend(backend)), move |event| {
            let settled_tx = settled_tx.clone();
            async move {
                let _ = settled_tx.send(event);
            }
        })
        .unwrap();
        (watcher, settled_rx)
    }

    async fn advance(duration: Duration) {
        tokio::time::advance(duration).await;
        tokio::task::yield_now().await;
    }

    #[tokio::test(start_paused = true)]
    async fn input_activity_emits_one_settled_switch_after_quiet_interval() {
        let backend = FakeBackendHandle::new();
        let (_watcher, mut settled) = start_fake_watcher(backend.clone());

        backend.emit(RawDeviceSwitch::Input);
        tokio::task::yield_now().await;
        advance(Duration::from_millis(499)).await;
        assert!(settled.try_recv().is_err());

        advance(Duration::from_millis(1)).await;
        assert_eq!(
            settled.try_recv().unwrap(),
            SettledSwitch {
                input_changed: true,
                output_changed: false,
            }
        );
        assert!(settled.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn repeated_activity_resets_one_shared_trailing_timer() {
        let backend = FakeBackendHandle::new();
        let (_watcher, mut settled) = start_fake_watcher(backend.clone());

        backend.emit(RawDeviceSwitch::Input);
        tokio::task::yield_now().await;
        advance(Duration::from_millis(400)).await;
        backend.emit(RawDeviceSwitch::Input);
        tokio::task::yield_now().await;
        advance(Duration::from_millis(499)).await;
        assert!(settled.try_recv().is_err());

        advance(Duration::from_millis(1)).await;
        assert_eq!(
            settled.try_recv().unwrap(),
            SettledSwitch {
                input_changed: true,
                output_changed: false,
            }
        );
        assert!(settled.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn interleaved_activity_emits_one_switch_with_both_directions() {
        let backend = FakeBackendHandle::new();
        let (_watcher, mut settled) = start_fake_watcher(backend.clone());

        backend.emit(RawDeviceSwitch::Input);
        tokio::task::yield_now().await;
        advance(Duration::from_millis(200)).await;
        backend.emit(RawDeviceSwitch::Output);
        tokio::task::yield_now().await;
        advance(SETTLE_INTERVAL).await;

        assert_eq!(
            settled.try_recv().unwrap(),
            SettledSwitch {
                input_changed: true,
                output_changed: true,
            }
        );
        assert!(settled.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn later_activity_burst_emits_a_second_settled_switch() {
        let backend = FakeBackendHandle::new();
        let (_watcher, mut settled) = start_fake_watcher(backend.clone());

        backend.emit(RawDeviceSwitch::Input);
        tokio::task::yield_now().await;
        advance(SETTLE_INTERVAL).await;
        assert!(settled.try_recv().unwrap().input_changed);

        backend.emit(RawDeviceSwitch::Output);
        tokio::task::yield_now().await;
        advance(SETTLE_INTERVAL).await;
        assert_eq!(
            settled.try_recv().unwrap(),
            SettledSwitch {
                input_changed: false,
                output_changed: true,
            }
        );
        assert!(settled.try_recv().is_err());
    }

    #[tokio::test]
    async fn dropping_watcher_stops_backend_before_returning() {
        let backend = FakeBackendHandle::new();
        let drops = backend.drops.clone();
        let (watcher, _settled) = start_fake_watcher(backend.clone());

        assert!(backend.sink.lock().unwrap().is_some());
        drop(watcher);

        assert!(backend.sink.lock().unwrap().is_none());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn stream_death_command_remains_immediate_while_switch_is_unsettled() {
        let backend = FakeBackendHandle::new();
        let (command_tx, mut command_rx) = mpsc::channel(10);
        let settled_tx = command_tx.clone();
        let _watcher =
            DeviceWatcher::with_backend(Box::new(FakeBackend(backend.clone())), move |settled| {
                let settled_tx = settled_tx.clone();
                async move {
                    let _ = settled_tx
                        .send(DaemonCommand::SettledDeviceSwitch(settled))
                        .await;
                }
            })
            .unwrap();
        let death = StreamDeath {
            source: CaptureSource::Dictation,
            generation: StreamGeneration(1),
        };

        backend.emit(RawDeviceSwitch::Input);
        tokio::task::yield_now().await;
        command_tx
            .send(DaemonCommand::CaptureStreamDied(death))
            .await
            .unwrap();

        assert!(matches!(
            command_rx.try_recv(),
            Ok(DaemonCommand::CaptureStreamDied(received)) if received == death
        ));
        assert!(command_rx.try_recv().is_err());

        advance(SETTLE_INTERVAL).await;
        assert!(matches!(
            command_rx.try_recv(),
            Ok(DaemonCommand::SettledDeviceSwitch(SettledSwitch {
                input_changed: true,
                output_changed: false,
            }))
        ));
    }

    #[cfg(not(target_os = "macos"))]
    #[tokio::test(start_paused = true)]
    async fn unsupported_platform_watcher_is_idle() {
        let (settled_tx, mut settled_rx) = mpsc::unbounded_channel();
        let watcher = DeviceWatcher::start(move |settled| {
            let settled_tx = settled_tx.clone();
            async move {
                let _ = settled_tx.send(settled);
            }
        })
        .unwrap();

        advance(Duration::from_secs(1)).await;
        assert!(settled_rx.try_recv().is_err());
        drop(watcher);
    }
}
