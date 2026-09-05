//! Deterministic multi-daemon Home Hub topology tests.

use std::future::Future;
use std::time::Duration;

mod clock;
mod daemon;
mod process_contract;
mod tailnet;
mod tcp_contract;

use daemon::{HomeHubTopology, TestDaemon};
use tailnet::{FaultGate, OperationFault};

mod scenarios;

const WATCHDOG_TIMEOUT: Duration = Duration::from_secs(10);

async fn watchdog<T>(label: &str, future: impl Future<Output = T>) -> T {
    tokio::time::timeout(WATCHDOG_TIMEOUT, future)
        .await
        .unwrap_or_else(|_| panic!("topology watchdog timed out while {label}"))
}
