//! HTTP-independent Home Hub synchronization domain.

pub mod client;
pub mod identity;
pub mod library;
pub mod outbox;
pub mod payload;
pub mod protocol;
pub mod runtime;
pub mod serve;
pub mod server;
pub mod service;
pub mod shared_library;
pub mod state;
pub mod tailscale;
pub mod transition;
pub mod transport;

pub use service::{SyncService, SyncServiceError};
