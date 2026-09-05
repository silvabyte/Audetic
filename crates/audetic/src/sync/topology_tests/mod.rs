//! Deterministic multi-daemon Home Hub topology tests.

mod clock;
mod daemon;
mod tailnet;

use daemon::{HomeHubTopology, TestDaemon};
use tailnet::{FaultGate, OperationFault};

mod scenarios;
