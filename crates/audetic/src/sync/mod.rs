//! HTTP-independent Home Hub synchronization domain.

pub mod client;
pub mod identity;
pub mod library;
pub mod library_reader;
pub mod outbox;
pub mod protocol;
pub mod server;
pub mod service;
pub mod tailscale;

pub use service::{HubAccess, SyncService, SyncServiceError};

use crate::db::sync_serve::SyncServeOwnership;

/// Whether persisted ownership names exactly the one mapping Audetic manages.
/// Both role demotion and uninstall must pass this check before asking the
/// Tailscale adapter to inspect/remove the live mapping.
pub(crate) fn is_exact_audetic_serve_ownership(ownership: &SyncServeOwnership) -> bool {
    ownership.https_port == protocol::TAILSCALE_HTTPS_PORT
        && ownership.mount_path == protocol::HUB_API_MOUNT_PATH.trim_end_matches('/')
        && ownership.proxy_url == protocol::HUB_LOOPBACK_BASE_URL
}
