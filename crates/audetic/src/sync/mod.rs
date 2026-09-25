//! Synchronization domain service and the isolated Hub transport server.

mod transport;

use anyhow::Result as AnyResult;
use audetic_core::config::{Config, SyncRole};
use audetic_core::sync::{
    DeviceAddResponse, DeviceListResponse, DeviceRevokeResponse, HubEnableResponse,
    LocalPairRequest, LocalPairResponse, LocalUnpairResponse, PairedHub, SyncStatusResponse,
    TransportPairRequest, TransportPairResponse, TransportStatusResponse, SYNC_PROTOCOL_VERSION,
};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use reqwest::{Client, StatusCode, Url};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::Mutex;
use uuid::Uuid;

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::db::sync::{AuthenticatedDevice, BindDeviceOutcome, SyncRepository};

pub use transport::{
    transport_bind_addr, TransportApiDoc, TransportServer, TRANSPORT_HOST, TRANSPORT_PAIR_PATH,
    TRANSPORT_PORT, TRANSPORT_STATUS_PATH,
};

const CREDENTIAL_PREFIX: &str = "audetic_sync_";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

pub type SyncResult<T> = Result<T, SyncError>;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SyncError {
    #[error("invalid synchronization request: {0}")]
    BadRequest(&'static str),
    #[error("operation requires the {required} role, but the daemon started as {active}")]
    RoleConflict {
        required: &'static str,
        active: &'static str,
    },
    #[error("this Client Node is already paired with a different Hub or credential")]
    PairingConflict,
    #[error("synchronization resource not found")]
    NotFound,
    #[error("unauthorized")]
    Unauthorized,
    #[error("the remote Sync Hub is unavailable or incompatible")]
    RemoteUnavailable,
    #[error("internal synchronization error")]
    Internal,
}

#[derive(Clone)]
pub struct SyncService {
    inner: Arc<SyncServiceInner>,
}

struct SyncServiceInner {
    active_role: SyncRole,
    node_id: String,
    db_path: PathBuf,
    config_path: PathBuf,
    client: Client,
    mutations: Mutex<()>,
}

impl SyncService {
    pub fn new(
        active_role: SyncRole,
        node_id: impl Into<String>,
        db_path: impl Into<PathBuf>,
        config_path: impl Into<PathBuf>,
    ) -> SyncResult<Self> {
        let node_id = node_id.into();
        Uuid::parse_str(&node_id)
            .map_err(|_| SyncError::BadRequest("the durable node ID must be a valid UUID"))?;
        let client = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| SyncError::Internal)?;

        Ok(Self {
            inner: Arc::new(SyncServiceInner {
                active_role,
                node_id,
                db_path: db_path.into(),
                config_path: config_path.into(),
                client,
                mutations: Mutex::new(()),
            }),
        })
    }

    pub fn active_role(&self) -> SyncRole {
        self.inner.active_role
    }

    pub fn node_id(&self) -> &str {
        &self.inner.node_id
    }

    pub fn db_path(&self) -> &Path {
        &self.inner.db_path
    }

    pub fn config_path(&self) -> &Path {
        &self.inner.config_path
    }

    pub async fn status(&self) -> SyncResult<SyncStatusResponse> {
        let _guard = self.inner.mutations.lock().await;
        let config_path = self.inner.config_path.clone();
        let config = blocking(move || Config::load_from(&config_path)).await?;
        let paired_hub = self.with_db(SyncRepository::paired_hub).await?;
        Ok(SyncStatusResponse {
            role: config.sync.role,
            node_id: self.inner.node_id.clone(),
            paired_hub,
        })
    }

    pub async fn enable_hub(&self) -> SyncResult<HubEnableResponse> {
        let _guard = self.inner.mutations.lock().await;
        if self.inner.active_role == SyncRole::Client {
            return Err(self.role_conflict("hub"));
        }
        if self.with_db(SyncRepository::paired_hub).await?.is_some() {
            return Err(SyncError::PairingConflict);
        }

        self.persist_role(SyncRole::Hub).await?;
        Ok(HubEnableResponse {
            role: SyncRole::Hub,
            restart_required: self.inner.active_role != SyncRole::Hub,
        })
    }

    pub async fn issue_device(&self, name: impl Into<String>) -> SyncResult<DeviceAddResponse> {
        self.require_role(SyncRole::Hub, "hub")?;
        let name = name.into();
        if name.trim().is_empty() {
            return Err(SyncError::BadRequest("device name must not be empty"));
        }

        let mut random = [0_u8; 32];
        getrandom::fill(&mut random).map_err(|_| SyncError::Internal)?;
        let credential = format!("{CREDENTIAL_PREFIX}{}", URL_SAFE_NO_PAD.encode(random));
        let hash = credential_hash(&credential);
        let device = self
            .with_db(move |conn| SyncRepository::issue_device(conn, &name, &hash))
            .await?;
        Ok(DeviceAddResponse { device, credential })
    }

    pub async fn list_devices(&self) -> SyncResult<DeviceListResponse> {
        self.require_role(SyncRole::Hub, "hub")?;
        Ok(DeviceListResponse {
            devices: self.with_db(SyncRepository::list_devices).await?,
        })
    }

    pub async fn revoke_device(
        &self,
        device_id: impl Into<String>,
    ) -> SyncResult<DeviceRevokeResponse> {
        self.require_role(SyncRole::Hub, "hub")?;
        let device_id = device_id.into();
        validate_uuid(&device_id, "device ID must be a valid UUID")?;
        let device = self
            .with_db(move |conn| SyncRepository::revoke_device(conn, &device_id))
            .await?
            .ok_or(SyncError::NotFound)?;
        Ok(DeviceRevokeResponse { device })
    }

    pub async fn pair(&self, request: LocalPairRequest) -> SyncResult<LocalPairResponse> {
        let _guard = self.inner.mutations.lock().await;
        if self.inner.active_role == SyncRole::Hub {
            return Err(self.role_conflict("standalone or client"));
        }
        if request.credential.trim().is_empty() {
            return Err(SyncError::BadRequest("credential must not be empty"));
        }

        let hub_url = normalize_hub_url(&request.hub_url)?;
        let canonical_hub_url = canonical_origin(&hub_url);
        let existing = self.with_db(SyncRepository::client_pairing).await?;
        if let Some(pairing) = &existing {
            if pairing.hub_url != canonical_hub_url
                || pairing.bearer_credential != request.credential
            {
                return Err(SyncError::PairingConflict);
            }
        } else if self.inner.active_role == SyncRole::Client {
            return Err(self.role_conflict("standalone"));
        }

        let endpoint = hub_url
            .join(TRANSPORT_PAIR_PATH)
            .map_err(|_| SyncError::Internal)?;
        let response = self
            .inner
            .client
            .post(endpoint)
            .bearer_auth(&request.credential)
            .json(&TransportPairRequest {
                client_node_id: self.inner.node_id.clone(),
            })
            .send()
            .await
            .map_err(|_| SyncError::RemoteUnavailable)?;
        match response.status() {
            StatusCode::UNAUTHORIZED => return Err(SyncError::Unauthorized),
            StatusCode::CONFLICT => return Err(SyncError::PairingConflict),
            status if !status.is_success() => return Err(SyncError::RemoteUnavailable),
            _ => {}
        }
        let remote = response
            .json::<TransportPairResponse>()
            .await
            .map_err(|_| SyncError::RemoteUnavailable)?;
        validate_remote_pairing(&remote)?;

        if let Some(pairing) = existing {
            if pairing.hub_node_id != remote.hub_node_id
                || pairing.device_id != remote.device_id
                || pairing.protocol_version != remote.protocol_version
            {
                return Err(SyncError::PairingConflict);
            }
            self.persist_role(SyncRole::Client).await?;
            return Ok(LocalPairResponse {
                role: SyncRole::Client,
                paired_hub: pairing.public_projection(),
                restart_required: self.inner.active_role != SyncRole::Client,
            });
        }

        let paired_hub = PairedHub {
            hub_url: canonical_hub_url,
            hub_node_id: remote.hub_node_id,
            device_id: remote.device_id,
            protocol_version: remote.protocol_version,
            paired_at: chrono::Utc::now().to_rfc3339(),
        };
        let saved = paired_hub.clone();
        let credential = request.credential;
        self.with_db(move |conn| SyncRepository::save_client_pairing(conn, &saved, &credential))
            .await?;
        self.persist_role(SyncRole::Client).await?;

        Ok(LocalPairResponse {
            role: SyncRole::Client,
            paired_hub,
            restart_required: self.inner.active_role != SyncRole::Client,
        })
    }

    pub async fn unpair(&self) -> SyncResult<LocalUnpairResponse> {
        let _guard = self.inner.mutations.lock().await;
        if self.inner.active_role == SyncRole::Hub {
            return Err(self.role_conflict("client or standalone"));
        }
        self.with_db(SyncRepository::delete_client_pairing).await?;
        self.persist_role(SyncRole::Standalone).await?;
        Ok(LocalUnpairResponse {
            role: SyncRole::Standalone,
            restart_required: self.inner.active_role != SyncRole::Standalone,
            hub_credential_revoked: false,
        })
    }

    pub(crate) async fn authenticate_transport(
        &self,
        credential: String,
    ) -> SyncResult<AuthenticatedDevice> {
        self.require_role(SyncRole::Hub, "hub")?;
        let hash = credential_hash(&credential);
        self.with_db(move |conn| SyncRepository::authenticate_device(conn, &hash))
            .await?
            .ok_or(SyncError::Unauthorized)
    }

    pub(crate) async fn bind_transport(
        &self,
        authenticated: AuthenticatedDevice,
        request: TransportPairRequest,
    ) -> SyncResult<TransportPairResponse> {
        self.require_role(SyncRole::Hub, "hub")?;
        validate_uuid(
            &request.client_node_id,
            "client node ID must be a valid UUID",
        )?;
        let device_id = authenticated.device_id;
        let client_node_id = request.client_node_id;
        let outcome = self
            .with_db(move |conn| SyncRepository::bind_device(conn, &device_id, &client_node_id))
            .await?;
        let device = match outcome {
            BindDeviceOutcome::Bound(device) | BindDeviceOutcome::AlreadyBound(device) => device,
            BindDeviceOutcome::Conflict => return Err(SyncError::PairingConflict),
            BindDeviceOutcome::NotFound | BindDeviceOutcome::Revoked => {
                return Err(SyncError::Unauthorized)
            }
        };
        Ok(TransportPairResponse {
            protocol_version: SYNC_PROTOCOL_VERSION,
            hub_node_id: self.inner.node_id.clone(),
            device_id: device.device_id,
        })
    }

    pub(crate) fn transport_status(
        &self,
        authenticated: AuthenticatedDevice,
    ) -> SyncResult<TransportStatusResponse> {
        self.require_role(SyncRole::Hub, "hub")?;
        let client_node_id = authenticated
            .client_node_id
            .ok_or(SyncError::PairingConflict)?;
        Ok(TransportStatusResponse {
            protocol_version: SYNC_PROTOCOL_VERSION,
            hub_node_id: self.inner.node_id.clone(),
            device_id: authenticated.device_id,
            client_node_id,
        })
    }

    async fn persist_role(&self, role: SyncRole) -> SyncResult<()> {
        let config_path = self.inner.config_path.clone();
        blocking(move || Config::update_at(&config_path, |config| config.sync.role = role)).await
    }

    async fn with_db<T, F>(&self, operation: F) -> SyncResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> AnyResult<T> + Send + 'static,
    {
        let db_path = self.inner.db_path.clone();
        blocking(move || {
            let conn = crate::db::open_db_at(&db_path)?;
            operation(&conn)
        })
        .await
    }

    fn require_role(&self, role: SyncRole, required: &'static str) -> SyncResult<()> {
        if self.inner.active_role == role {
            Ok(())
        } else {
            Err(self.role_conflict(required))
        }
    }

    fn role_conflict(&self, required: &'static str) -> SyncError {
        SyncError::RoleConflict {
            required,
            active: self.inner.active_role.as_str(),
        }
    }
}

pub fn normalize_hub_url(input: &str) -> SyncResult<Url> {
    let mut url = Url::parse(input).map_err(|_| SyncError::BadRequest("Hub URL is invalid"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(SyncError::BadRequest(
            "Hub URL must not contain user information",
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(SyncError::BadRequest(
            "Hub URL must not contain a query or fragment",
        ));
    }
    if url.path() != "/" && !url.path().is_empty() {
        return Err(SyncError::BadRequest("Hub URL must contain only an origin"));
    }
    let loopback = url.host_str().is_some_and(|host| {
        let unbracketed = host.trim_start_matches('[').trim_end_matches(']');
        unbracketed
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
            || host.eq_ignore_ascii_case("localhost")
            || host.ends_with(".localhost")
    });
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        _ => {
            return Err(SyncError::BadRequest(
                "Hub URL must use HTTPS unless it is loopback",
            ))
        }
    }
    url.set_path("/");
    Ok(url)
}

fn canonical_origin(url: &Url) -> String {
    url.as_str().trim_end_matches('/').to_string()
}

fn validate_remote_pairing(response: &TransportPairResponse) -> SyncResult<()> {
    if response.protocol_version != SYNC_PROTOCOL_VERSION {
        return Err(SyncError::RemoteUnavailable);
    }
    validate_uuid(&response.hub_node_id, "Hub node ID must be a valid UUID")
        .map_err(|_| SyncError::RemoteUnavailable)?;
    validate_uuid(&response.device_id, "device ID must be a valid UUID")
        .map_err(|_| SyncError::RemoteUnavailable)
}

fn validate_uuid(value: &str, error: &'static str) -> SyncResult<()> {
    Uuid::parse_str(value)
        .map(|_| ())
        .map_err(|_| SyncError::BadRequest(error))
}

fn credential_hash(credential: &str) -> [u8; 32] {
    Sha256::digest(credential.as_bytes()).into()
}

async fn blocking<T, F>(operation: F) -> SyncResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> AnyResult<T> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| SyncError::Internal)?
        .map_err(|_| SyncError::Internal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_url_validation_and_normalization_matrix() {
        for (input, expected) in [
            ("https://sync.example.com", "https://sync.example.com/"),
            ("https://sync.example.com/", "https://sync.example.com/"),
            ("http://localhost:3738", "http://localhost:3738/"),
            ("http://dev.localhost:3738/", "http://dev.localhost:3738/"),
            ("http://127.0.0.1:3738", "http://127.0.0.1:3738/"),
            ("http://127.0.0.2:3738", "http://127.0.0.2:3738/"),
            ("http://[::1]:3738", "http://[::1]:3738/"),
        ] {
            assert_eq!(
                normalize_hub_url(input).unwrap().as_str(),
                expected,
                "{input}"
            );
        }

        for input in [
            "http://sync.example.com",
            "ftp://sync.example.com",
            "https://sync.example.com/api",
            "https://user@sync.example.com",
            "https://sync.example.com?debug=true",
            "https://sync.example.com#fragment",
        ] {
            assert!(normalize_hub_url(input).is_err(), "{input}");
        }
    }

    #[test]
    fn errors_do_not_echo_credentials() {
        let credential = "audetic_sync_do-not-print-this";
        for error in [
            SyncError::Unauthorized,
            SyncError::PairingConflict,
            SyncError::RemoteUnavailable,
            SyncError::Internal,
        ] {
            assert!(!format!("{error}").contains(credential));
            assert!(!format!("{error:?}").contains(credential));
        }
    }
}
