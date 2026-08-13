use anyhow::Result;

use super::{ActiveDeviceWatcher, DeviceWatcherBackend, RawSwitchSink};

pub(super) fn backend() -> Box<dyn DeviceWatcherBackend> {
    Box::new(NoopBackend)
}

struct NoopBackend;

struct NoopActiveWatcher;

impl ActiveDeviceWatcher for NoopActiveWatcher {}

impl DeviceWatcherBackend for NoopBackend {
    fn start(self: Box<Self>, _sink: RawSwitchSink) -> Result<Box<dyn ActiveDeviceWatcher>> {
        Ok(Box::new(NoopActiveWatcher))
    }
}
