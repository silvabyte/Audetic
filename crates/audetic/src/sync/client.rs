use async_trait::async_trait;
use audetic_core::sync::{HubCandidate, HubConnection, HubId, RecordId};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use http::header::{ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RETRY_AFTER};
use http::{HeaderMap, HeaderName, HeaderValue};
use reqwest::Url;
use thiserror::Error;

use std::collections::BTreeMap;
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use super::protocol::{
    hub_blob_path, hub_payload_path, DictationPage, HubApiError, HubInfo, MeetingPage,
    MeetingTitlePatch, RecordKind, SharedMeeting, SnapshotBatch, SnapshotBatchResponse,
    HUB_API_MOUNT_PATH, HUB_DICTATIONS_PATH, HUB_ID_HEADER, HUB_INFO_PATH, HUB_MEETINGS_PATH,
    HUB_SNAPSHOTS_PATH, MAX_BLOB_BYTES, PROTOCOL_VERSION, PROTOCOL_VERSION_HEADER,
    TAILSCALE_HTTPS_PORT,
};
use super::transport::{
    BlobUpload, DiscoveryFailure, DiscoveryOutcome, HubProbe, HubTransferError, PayloadBody,
    PayloadContentRange, PayloadMetadata, RemoteDictationLibrary, RemoteLibraryMutations,
    RemoteMeetingLibrary, RemotePayloadSource, ReplicationTransport, StreamingPayloadResponse,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransportResponse {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

pub struct StreamingTransportResponse {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: super::transport::PayloadBody,
}

#[async_trait]
pub trait HubTransport: Clone + Send + Sync + 'static {
    async fn get(
        &self,
        url: Url,
        headers: BTreeMap<String, String>,
    ) -> Result<TransportResponse, String>;

    async fn post(
        &self,
        _url: Url,
        _headers: BTreeMap<String, String>,
        _body: Vec<u8>,
    ) -> Result<TransportResponse, String> {
        Err("POST is not implemented by this transport".to_owned())
    }

    async fn patch(
        &self,
        _url: Url,
        _headers: BTreeMap<String, String>,
        _body: Vec<u8>,
    ) -> Result<TransportResponse, String> {
        Err("PATCH is not implemented by this transport".into())
    }
    async fn delete(
        &self,
        _url: Url,
        _headers: BTreeMap<String, String>,
    ) -> Result<TransportResponse, String> {
        Err("DELETE is not implemented by this transport".into())
    }

    async fn head(
        &self,
        _url: Url,
        _headers: BTreeMap<String, String>,
    ) -> Result<TransportResponse, String> {
        Err("HEAD is not implemented by this transport".into())
    }

    async fn put_file(
        &self,
        _url: Url,
        _headers: BTreeMap<String, String>,
        _path: &Path,
        _byte_size: u64,
        _media_type: &str,
    ) -> Result<TransportResponse, String> {
        Err("streaming PUT is not implemented by this transport".into())
    }

    async fn get_stream(
        &self,
        url: Url,
        headers: BTreeMap<String, String>,
    ) -> Result<StreamingTransportResponse, String>;
}

#[derive(Clone, Debug)]
pub struct ReqwestHubTransport {
    client: reqwest::Client,
}

impl ReqwestHubTransport {
    pub fn new() -> Result<Self, reqwest::Error> {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(10 * 60))
            .build()
            .map(|client| Self { client })
    }
}

#[async_trait]
impl HubTransport for ReqwestHubTransport {
    async fn get(
        &self,
        url: Url,
        headers: BTreeMap<String, String>,
    ) -> Result<TransportResponse, String> {
        let mut request = self.client.get(url);
        for (name, value) in headers {
            request = request.header(name, value);
        }
        let response = request.send().await.map_err(|error| error.to_string())?;
        let status = response.status().as_u16();
        let headers = response_headers_from_reqwest(response.headers());
        let body = response
            .bytes()
            .await
            .map_err(|error| error.to_string())?
            .to_vec();
        Ok(TransportResponse {
            status,
            headers,
            body,
        })
    }

    async fn post(
        &self,
        url: Url,
        headers: BTreeMap<String, String>,
        body: Vec<u8>,
    ) -> Result<TransportResponse, String> {
        let mut request = self.client.post(url).body(body);
        for (name, value) in headers {
            request = request.header(name, value);
        }
        let response = request
            .header("content-type", "application/json")
            .send()
            .await
            .map_err(|error| error.to_string())?;
        response_from_reqwest(response).await
    }

    async fn patch(
        &self,
        url: Url,
        headers: BTreeMap<String, String>,
        body: Vec<u8>,
    ) -> Result<TransportResponse, String> {
        let mut request = self
            .client
            .patch(url)
            .body(body)
            .header("content-type", "application/json");
        for (name, value) in headers {
            request = request.header(name, value);
        }
        response_from_reqwest(request.send().await.map_err(|error| error.to_string())?).await
    }
    async fn delete(
        &self,
        url: Url,
        headers: BTreeMap<String, String>,
    ) -> Result<TransportResponse, String> {
        let mut request = self.client.delete(url);
        for (name, value) in headers {
            request = request.header(name, value);
        }
        response_from_reqwest(request.send().await.map_err(|error| error.to_string())?).await
    }

    async fn head(
        &self,
        url: Url,
        headers: BTreeMap<String, String>,
    ) -> Result<TransportResponse, String> {
        let mut request = self.client.head(url);
        for (name, value) in headers {
            request = request.header(name, value);
        }
        response_from_reqwest(request.send().await.map_err(|error| error.to_string())?).await
    }

    async fn put_file(
        &self,
        url: Url,
        headers: BTreeMap<String, String>,
        path: &Path,
        byte_size: u64,
        media_type: &str,
    ) -> Result<TransportResponse, String> {
        let file = tokio::fs::File::open(path)
            .await
            .map_err(|error| error.to_string())?;
        let stream = tokio_util::io::ReaderStream::new(file);
        let mut request = self
            .client
            .put(url)
            .header("content-type", media_type)
            .header("content-length", byte_size)
            .body(reqwest::Body::wrap_stream(stream));
        for (name, value) in headers {
            request = request.header(name, value);
        }
        response_from_reqwest(request.send().await.map_err(|error| error.to_string())?).await
    }

    async fn get_stream(
        &self,
        url: Url,
        headers: BTreeMap<String, String>,
    ) -> Result<StreamingTransportResponse, String> {
        let mut request = self.client.get(url);
        for (name, value) in headers {
            request = request.header(name, value);
        }
        let response = request.send().await.map_err(|error| error.to_string())?;
        let status = response.status().as_u16();
        let headers = response_headers_from_reqwest(response.headers());
        let body = response
            .bytes_stream()
            .map(|chunk| chunk.map_err(|error| HubTransferError::Transport(error.to_string())));
        Ok(StreamingTransportResponse {
            status,
            headers,
            body: Box::pin(body),
        })
    }
}

async fn response_from_reqwest(response: reqwest::Response) -> Result<TransportResponse, String> {
    let status = response.status().as_u16();
    let headers = response_headers_from_reqwest(response.headers());
    let body = response
        .bytes()
        .await
        .map_err(|error| error.to_string())?
        .to_vec();
    Ok(TransportResponse {
        status,
        headers,
        body,
    })
}

fn response_headers_from_reqwest(headers: &reqwest::header::HeaderMap) -> HeaderMap {
    let mut converted = HeaderMap::new();
    for name in headers.keys() {
        let name = HeaderName::from_bytes(name.as_str().as_bytes())
            .expect("reqwest only exposes valid header names");
        for value in headers.get_all(name.as_str()).iter() {
            let value = HeaderValue::from_bytes(value.as_bytes())
                .expect("reqwest only exposes valid header values");
            converted.append(name.clone(), value);
        }
    }
    converted
}

#[derive(Clone, Copy, Debug, Default)]
pub struct HandshakeExpectation<'a> {
    pub hub_id: Option<HubId>,
    pub owner_login: Option<&'a str>,
}

#[derive(Debug, Error)]
pub enum HubClientError {
    #[error("invalid Home Hub base URL: {0}")]
    InvalidBaseUrl(String),
    #[error("Home Hub URLs must use HTTPS")]
    InsecureBaseUrl,
    #[error("Home Hub URL must not contain credentials, query parameters, or a fragment")]
    UnsafeBaseUrl,
    #[error("Home Hub URL path must be / or {HUB_API_MOUNT_PATH}")]
    UnexpectedBasePath,
    #[error("Home Hub URL must use dedicated HTTPS port {TAILSCALE_HTTPS_PORT}")]
    UnexpectedPort,
    #[error("Home Hub transport failed: {0}")]
    Transport(String),
    #[error("Home Hub returned HTTP {status}: {message}")]
    Http {
        status: u16,
        message: String,
        retry_after: Option<String>,
    },
    #[error("Home Hub returned malformed discovery JSON: {0}")]
    InvalidInfo(#[from] serde_json::Error),
    #[error("Home Hub response omitted {HUB_ID_HEADER}")]
    MissingHubIdHeader,
    #[error("Home Hub response repeated {HUB_ID_HEADER}")]
    DuplicateHubIdHeader,
    #[error("Home Hub returned an invalid Hub ID in {0}")]
    InvalidHubId(&'static str),
    #[error("Home Hub response body and header identify different hubs")]
    InconsistentHubId,
    #[error("Home Hub returned invalid payload metadata: {0}")]
    InvalidPayloadMetadata(String),
    #[error("expected Home Hub {expected}, but reached {actual}")]
    WrongHubId { expected: HubId, actual: HubId },
    #[error("expected Tailscale owner {expected:?}, but hub belongs to {actual:?}")]
    WrongOwner { expected: String, actual: String },
    #[error("Home Hub protocol range {minimum}..={current} is incompatible with protocol {PROTOCOL_VERSION}")]
    IncompatibleProtocol { minimum: u16, current: u16 },
}

impl From<HubClientError> for HubTransferError {
    fn from(error: HubClientError) -> Self {
        match error {
            HubClientError::Transport(message) => Self::Transport(message),
            HubClientError::Http {
                status,
                message,
                retry_after,
            } => Self::Http {
                status,
                message,
                retry_after,
            },
            error => Self::NeedsAttention(error.to_string()),
        }
    }
}

#[derive(Clone)]
pub struct HubClient<T = ReqwestHubTransport> {
    base_url: Url,
    transport: T,
}

impl HubClient<ReqwestHubTransport> {
    pub fn new(base_url: &str) -> Result<Self, HubClientError> {
        let transport = ReqwestHubTransport::new()
            .map_err(|error| HubClientError::Transport(error.to_string()))?;
        Self::with_transport(base_url, transport)
    }
}

impl<T: HubTransport> HubClient<T> {
    pub fn with_transport(base_url: &str, transport: T) -> Result<Self, HubClientError> {
        Ok(Self {
            base_url: canonicalize_base_url(base_url)?,
            transport,
        })
    }

    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    fn verify_target(&self, hub: &HubConnection) -> Result<(), HubTransferError> {
        let target = canonicalize_base_url(&hub.base_url).map_err(HubTransferError::from)?;
        if target == self.base_url {
            Ok(())
        } else {
            Err(HubTransferError::NeedsAttention(
                "Home Hub client target does not match the requested connection".to_owned(),
            ))
        }
    }

    pub async fn handshake(
        &self,
        expectation: HandshakeExpectation<'_>,
    ) -> Result<HubCandidate, HubClientError> {
        let url = self
            .base_url
            .join(HUB_INFO_PATH)
            .map_err(|error| HubClientError::InvalidBaseUrl(error.to_string()))?;
        let mut headers = BTreeMap::from([(
            PROTOCOL_VERSION_HEADER.to_owned(),
            PROTOCOL_VERSION.to_string(),
        )]);
        if let Some(hub_id) = expectation.hub_id {
            headers.insert(HUB_ID_HEADER.to_owned(), hub_id.to_string());
        }

        let response = self
            .transport
            .get(url, headers)
            .await
            .map_err(HubClientError::Transport)?;
        if let Some(expected) = expectation.hub_id {
            verify_expected_hub_id(&response.headers, expected)?;
        }
        if !(200..300).contains(&response.status) {
            return Err(http_error(response));
        }

        let header_hub_id = response_hub_id(&response.headers)?;
        let info: HubInfo = serde_json::from_slice(&response.body)?;
        let body_hub_id = info.hub_id;
        if header_hub_id != body_hub_id {
            return Err(HubClientError::InconsistentHubId);
        }
        if let Some(expected) = expectation.hub_id {
            if expected != body_hub_id {
                return Err(HubClientError::WrongHubId {
                    expected,
                    actual: body_hub_id,
                });
            }
        }
        if let Some(expected) = expectation.owner_login {
            if expected != info.owner_login {
                return Err(HubClientError::WrongOwner {
                    expected: expected.to_owned(),
                    actual: info.owner_login,
                });
            }
        }
        if !info.protocol.accepts(PROTOCOL_VERSION) {
            return Err(HubClientError::IncompatibleProtocol {
                minimum: info.protocol.minimum,
                current: info.protocol.current,
            });
        }

        Ok(HubCandidate {
            connection: HubConnection {
                base_url: self.base_url.to_string(),
                hub_id: body_hub_id,
                owner_login: info.owner_login,
            },
            device_name: info.device_name,
            protocol_version: u32::from(info.protocol.current),
        })
    }

    pub async fn upload_snapshots(
        &self,
        hub_id: HubId,
        batch: &SnapshotBatch,
    ) -> Result<SnapshotBatchResponse, HubClientError> {
        let url = self
            .base_url
            .join(HUB_SNAPSHOTS_PATH)
            .map_err(|error| HubClientError::InvalidBaseUrl(error.to_string()))?;
        let response = self
            .transport
            .post(
                url,
                protocol_headers(hub_id),
                serde_json::to_vec(batch).map_err(HubClientError::InvalidInfo)?,
            )
            .await
            .map_err(HubClientError::Transport)?;
        verify_hub_response(&response, hub_id)?;
        if !(200..300).contains(&response.status) {
            return Err(http_error(response));
        }
        serde_json::from_slice(&response.body).map_err(HubClientError::InvalidInfo)
    }

    pub async fn upload_blob(
        &self,
        hub_id: HubId,
        checksum: &str,
        path: &Path,
        byte_size: u64,
        media_type: &str,
    ) -> Result<(), HubClientError> {
        let url = self
            .base_url
            .join(&hub_blob_path(checksum))
            .map_err(|error| HubClientError::InvalidBaseUrl(error.to_string()))?;
        let response = self
            .transport
            .put_file(url, protocol_headers(hub_id), path, byte_size, media_type)
            .await
            .map_err(HubClientError::Transport)?;
        verify_hub_response(&response, hub_id)?;
        if !(200..300).contains(&response.status) {
            return Err(http_error(response));
        }
        Ok(())
    }

    pub async fn head_blob(&self, hub_id: HubId, checksum: &str) -> Result<bool, HubClientError> {
        let url = self
            .base_url
            .join(&hub_blob_path(checksum))
            .map_err(|error| HubClientError::InvalidBaseUrl(error.to_string()))?;
        let response = self
            .transport
            .head(url, protocol_headers(hub_id))
            .await
            .map_err(HubClientError::Transport)?;
        verify_hub_response(&response, hub_id)?;
        match response.status {
            200..=299 => Ok(true),
            404 => Ok(false),
            _ => Err(http_error(response)),
        }
    }

    pub async fn page_dictations(
        &self,
        hub_id: HubId,
        query: Option<&str>,
        from: Option<&str>,
        to: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<DictationPage, HubClientError> {
        let mut url = self
            .base_url
            .join(HUB_DICTATIONS_PATH)
            .map_err(|error| HubClientError::InvalidBaseUrl(error.to_string()))?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("limit", &limit.to_string());
            if let Some(value) = query {
                pairs.append_pair("q", value);
            }
            if let Some(value) = from {
                pairs.append_pair("from", value);
            }
            if let Some(value) = to {
                pairs.append_pair("to", value);
            }
            if let Some(value) = cursor {
                pairs.append_pair("cursor", value);
            }
        }
        let response = self
            .transport
            .get(url, protocol_headers(hub_id))
            .await
            .map_err(HubClientError::Transport)?;
        verify_hub_response(&response, hub_id)?;
        if !(200..300).contains(&response.status) {
            return Err(http_error(response));
        }
        serde_json::from_slice(&response.body).map_err(HubClientError::InvalidInfo)
    }

    pub async fn page_meetings(
        &self,
        hub_id: HubId,
        query: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<MeetingPage, HubClientError> {
        let mut url = self
            .base_url
            .join(HUB_MEETINGS_PATH)
            .map_err(|error| HubClientError::InvalidBaseUrl(error.to_string()))?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("limit", &limit.to_string());
            if let Some(value) = query {
                pairs.append_pair("q", value);
            }
            if let Some(value) = cursor {
                pairs.append_pair("cursor", value);
            }
        }
        let response = self
            .transport
            .get(url, protocol_headers(hub_id))
            .await
            .map_err(HubClientError::Transport)?;
        verify_hub_response(&response, hub_id)?;
        if !(200..300).contains(&response.status) {
            return Err(http_error(response));
        }
        serde_json::from_slice(&response.body).map_err(HubClientError::InvalidInfo)
    }

    pub async fn meeting(
        &self,
        hub_id: HubId,
        id: RecordId,
    ) -> Result<Option<SharedMeeting>, HubClientError> {
        let url = self
            .base_url
            .join(&format!("v1/meetings/{id}"))
            .map_err(|error| HubClientError::InvalidBaseUrl(error.to_string()))?;
        let response = self
            .transport
            .get(url, protocol_headers(hub_id))
            .await
            .map_err(HubClientError::Transport)?;
        verify_hub_response(&response, hub_id)?;
        if response.status == 404 {
            return Ok(None);
        }
        if !(200..300).contains(&response.status) {
            return Err(http_error(response));
        }
        serde_json::from_slice(&response.body)
            .map(Some)
            .map_err(HubClientError::InvalidInfo)
    }

    pub async fn update_meeting_title(
        &self,
        hub_id: HubId,
        id: RecordId,
        patch: &MeetingTitlePatch,
    ) -> Result<SharedMeeting, HubClientError> {
        let url = self
            .base_url
            .join(&format!("v1/meetings/{id}"))
            .map_err(|error| HubClientError::InvalidBaseUrl(error.to_string()))?;
        let response = self
            .transport
            .patch(
                url,
                protocol_headers(hub_id),
                serde_json::to_vec(patch).map_err(HubClientError::InvalidInfo)?,
            )
            .await
            .map_err(HubClientError::Transport)?;
        verify_hub_response(&response, hub_id)?;
        if !(200..300).contains(&response.status) {
            return Err(http_error(response));
        }
        serde_json::from_slice(&response.body).map_err(HubClientError::InvalidInfo)
    }

    pub async fn delete_record(
        &self,
        hub_id: HubId,
        id: RecordId,
        kind: RecordKind,
    ) -> Result<(), HubClientError> {
        let segment = match kind {
            RecordKind::Dictation => "dictations",
            RecordKind::Meeting => "meetings",
            RecordKind::Artifact => "artifacts",
        };
        let url = self
            .base_url
            .join(&format!("v1/{segment}/{id}"))
            .map_err(|error| HubClientError::InvalidBaseUrl(error.to_string()))?;
        let response = self
            .transport
            .delete(url, protocol_headers(hub_id))
            .await
            .map_err(HubClientError::Transport)?;
        verify_hub_response(&response, hub_id)?;
        if !(200..300).contains(&response.status) {
            return Err(http_error(response));
        }
        Ok(())
    }

    pub async fn stream_payload(
        &self,
        hub_id: HubId,
        kind: RecordKind,
        record_id: RecordId,
        range: Option<&str>,
    ) -> Result<StreamingPayloadResponse, HubClientError> {
        let url = self
            .base_url
            .join(&hub_payload_path(kind, record_id))
            .map_err(|error| HubClientError::InvalidBaseUrl(error.to_string()))?;
        let mut headers = protocol_headers(hub_id);
        if let Some(range) = range {
            headers.insert("range".to_owned(), range.to_owned());
        }
        let response = self
            .transport
            .get_stream(url, headers)
            .await
            .map_err(HubClientError::Transport)?;
        verify_streaming_hub_response(&response, hub_id)?;
        let metadata = payload_metadata(response.status, &response.headers, range)?;
        let expected_length = metadata
            .content_range
            .as_ref()
            .and_then(PayloadContentRange::byte_length)
            .or(metadata.content_length);
        let maximum_length = expected_length.unwrap_or(MAX_BLOB_BYTES);
        Ok(StreamingPayloadResponse {
            status: response.status,
            metadata,
            body: Box::pin(ValidatedPayloadBody::new(
                response.body,
                maximum_length,
                expected_length,
            )),
        })
    }
}

#[async_trait]
impl<T: HubTransport> HubProbe for HubClient<T> {
    async fn handshake(&self, hub: &HubConnection) -> Result<HubCandidate, HubTransferError> {
        self.verify_target(hub)?;
        HubClient::handshake(
            self,
            HandshakeExpectation {
                hub_id: Some(hub.hub_id),
                owner_login: Some(&hub.owner_login),
            },
        )
        .await
        .map_err(HubTransferError::from)
    }

    async fn discover(
        &self,
        candidates: Vec<String>,
        expected_owner_login: &str,
    ) -> DiscoveryOutcome {
        discover_hubs(self.transport.clone(), candidates, expected_owner_login).await
    }
}

#[async_trait]
impl<T: HubTransport> ReplicationTransport for HubClient<T> {
    async fn upload_snapshots(
        &self,
        hub: &HubConnection,
        batch: SnapshotBatch,
    ) -> Result<SnapshotBatchResponse, HubTransferError> {
        self.verify_target(hub)?;
        HubClient::upload_snapshots(self, hub.hub_id, &batch)
            .await
            .map_err(HubTransferError::from)
    }

    async fn upload_blob(
        &self,
        hub: &HubConnection,
        blob: BlobUpload,
    ) -> Result<(), HubTransferError> {
        self.verify_target(hub)?;
        HubClient::upload_blob(
            self,
            hub.hub_id,
            &blob.checksum,
            &blob.source_path,
            blob.byte_size,
            &blob.media_type,
        )
        .await
        .map_err(HubTransferError::from)
    }
}

#[async_trait]
impl<T: HubTransport> RemoteDictationLibrary for HubClient<T> {
    async fn page_dictations(
        &self,
        hub: &HubConnection,
        query: Option<&str>,
        from: Option<&str>,
        to: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<DictationPage, HubTransferError> {
        self.verify_target(hub)?;
        HubClient::page_dictations(self, hub.hub_id, query, from, to, cursor, limit)
            .await
            .map_err(HubTransferError::from)
    }
}

#[async_trait]
impl<T: HubTransport> RemoteMeetingLibrary for HubClient<T> {
    async fn page_meetings(
        &self,
        hub: &HubConnection,
        query: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<MeetingPage, HubTransferError> {
        self.verify_target(hub)?;
        HubClient::page_meetings(self, hub.hub_id, query, cursor, limit)
            .await
            .map_err(HubTransferError::from)
    }

    async fn meeting(
        &self,
        hub: &HubConnection,
        id: RecordId,
    ) -> Result<Option<SharedMeeting>, HubTransferError> {
        self.verify_target(hub)?;
        HubClient::meeting(self, hub.hub_id, id)
            .await
            .map_err(HubTransferError::from)
    }
}

#[async_trait]
impl<T: HubTransport> RemoteLibraryMutations for HubClient<T> {
    async fn update_meeting_title(
        &self,
        hub: &HubConnection,
        id: RecordId,
        patch: MeetingTitlePatch,
    ) -> Result<SharedMeeting, HubTransferError> {
        self.verify_target(hub)?;
        HubClient::update_meeting_title(self, hub.hub_id, id, &patch)
            .await
            .map_err(HubTransferError::from)
    }

    async fn delete_record(
        &self,
        hub: &HubConnection,
        id: RecordId,
        kind: RecordKind,
    ) -> Result<(), HubTransferError> {
        self.verify_target(hub)?;
        HubClient::delete_record(self, hub.hub_id, id, kind)
            .await
            .map_err(HubTransferError::from)
    }
}

#[async_trait]
impl<T: HubTransport> RemotePayloadSource for HubClient<T> {
    async fn stream_payload(
        &self,
        hub: &HubConnection,
        id: RecordId,
        kind: RecordKind,
        range: Option<&str>,
    ) -> Result<StreamingPayloadResponse, HubTransferError> {
        self.verify_target(hub)?;
        HubClient::stream_payload(self, hub.hub_id, kind, id, range)
            .await
            .map_err(HubTransferError::from)
    }
}

#[derive(Clone, Debug)]
pub struct NetworkHubAdapter {
    transport: Result<ReqwestHubTransport, String>,
}

impl Default for NetworkHubAdapter {
    fn default() -> Self {
        Self {
            transport: ReqwestHubTransport::new().map_err(|error| error.to_string()),
        }
    }
}

impl NetworkHubAdapter {
    fn client(
        &self,
        hub: &HubConnection,
    ) -> Result<HubClient<ReqwestHubTransport>, HubTransferError> {
        let transport = self
            .transport
            .as_ref()
            .map_err(|error| HubTransferError::Transport(error.clone()))?
            .clone();
        HubClient::with_transport(&hub.base_url, transport).map_err(HubTransferError::from)
    }
}

#[async_trait]
impl HubProbe for NetworkHubAdapter {
    async fn handshake(&self, hub: &HubConnection) -> Result<HubCandidate, HubTransferError> {
        HubProbe::handshake(&self.client(hub)?, hub).await
    }

    async fn discover(
        &self,
        candidates: Vec<String>,
        expected_owner_login: &str,
    ) -> DiscoveryOutcome {
        match &self.transport {
            Ok(transport) => {
                discover_hubs(transport.clone(), candidates, expected_owner_login).await
            }
            Err(error) => DiscoveryOutcome::None {
                failures: vec![DiscoveryFailure {
                    candidate: "Tailscale peers".to_owned(),
                    reason: error.clone(),
                }],
            },
        }
    }
}

#[async_trait]
impl ReplicationTransport for NetworkHubAdapter {
    async fn upload_snapshots(
        &self,
        hub: &HubConnection,
        batch: SnapshotBatch,
    ) -> Result<SnapshotBatchResponse, HubTransferError> {
        ReplicationTransport::upload_snapshots(&self.client(hub)?, hub, batch).await
    }

    async fn upload_blob(
        &self,
        hub: &HubConnection,
        blob: BlobUpload,
    ) -> Result<(), HubTransferError> {
        ReplicationTransport::upload_blob(&self.client(hub)?, hub, blob).await
    }
}

#[async_trait]
impl RemoteDictationLibrary for NetworkHubAdapter {
    async fn page_dictations(
        &self,
        hub: &HubConnection,
        query: Option<&str>,
        from: Option<&str>,
        to: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<DictationPage, HubTransferError> {
        RemoteDictationLibrary::page_dictations(
            &self.client(hub)?,
            hub,
            query,
            from,
            to,
            cursor,
            limit,
        )
        .await
    }
}

#[async_trait]
impl RemoteMeetingLibrary for NetworkHubAdapter {
    async fn page_meetings(
        &self,
        hub: &HubConnection,
        query: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<MeetingPage, HubTransferError> {
        RemoteMeetingLibrary::page_meetings(&self.client(hub)?, hub, query, cursor, limit).await
    }

    async fn meeting(
        &self,
        hub: &HubConnection,
        id: RecordId,
    ) -> Result<Option<SharedMeeting>, HubTransferError> {
        RemoteMeetingLibrary::meeting(&self.client(hub)?, hub, id).await
    }
}

#[async_trait]
impl RemoteLibraryMutations for NetworkHubAdapter {
    async fn update_meeting_title(
        &self,
        hub: &HubConnection,
        id: RecordId,
        patch: MeetingTitlePatch,
    ) -> Result<SharedMeeting, HubTransferError> {
        RemoteLibraryMutations::update_meeting_title(&self.client(hub)?, hub, id, patch).await
    }

    async fn delete_record(
        &self,
        hub: &HubConnection,
        id: RecordId,
        kind: RecordKind,
    ) -> Result<(), HubTransferError> {
        RemoteLibraryMutations::delete_record(&self.client(hub)?, hub, id, kind).await
    }
}

#[async_trait]
impl RemotePayloadSource for NetworkHubAdapter {
    async fn stream_payload(
        &self,
        hub: &HubConnection,
        id: RecordId,
        kind: RecordKind,
        range: Option<&str>,
    ) -> Result<StreamingPayloadResponse, HubTransferError> {
        RemotePayloadSource::stream_payload(&self.client(hub)?, hub, id, kind, range).await
    }
}

fn protocol_headers(hub_id: HubId) -> BTreeMap<String, String> {
    BTreeMap::from([
        (HUB_ID_HEADER.to_owned(), hub_id.to_string()),
        (
            PROTOCOL_VERSION_HEADER.to_owned(),
            PROTOCOL_VERSION.to_string(),
        ),
    ])
}

fn verify_hub_response(
    response: &TransportResponse,
    expected: HubId,
) -> Result<(), HubClientError> {
    verify_expected_hub_id(&response.headers, expected)
}

fn verify_streaming_hub_response(
    response: &StreamingTransportResponse,
    expected: HubId,
) -> Result<(), HubClientError> {
    verify_expected_hub_id(&response.headers, expected)
}

fn verify_expected_hub_id(headers: &HeaderMap, expected: HubId) -> Result<(), HubClientError> {
    let actual = response_hub_id(headers)?;
    if actual != expected {
        return Err(HubClientError::WrongHubId { expected, actual });
    }
    Ok(())
}

fn response_hub_id(headers: &HeaderMap) -> Result<HubId, HubClientError> {
    let mut values = headers.get_all(HUB_ID_HEADER).iter();
    let value = values.next().ok_or(HubClientError::MissingHubIdHeader)?;
    if values.next().is_some() {
        return Err(HubClientError::DuplicateHubIdHeader);
    }
    value
        .to_str()
        .map_err(|_| HubClientError::InvalidHubId(HUB_ID_HEADER))?
        .parse::<HubId>()
        .map_err(|_| HubClientError::InvalidHubId(HUB_ID_HEADER))
}

fn payload_metadata(
    status: u16,
    headers: &HeaderMap,
    requested_range: Option<&str>,
) -> Result<PayloadMetadata, HubClientError> {
    let content_type = unique_header(headers, &CONTENT_TYPE)?
        .map(|value| {
            let text = value
                .to_str()
                .map_err(|_| invalid_payload_metadata("Content-Type is not valid visible text"))?;
            if text.trim().is_empty() {
                return Err(invalid_payload_metadata("Content-Type is empty"));
            }
            Ok(value.clone())
        })
        .transpose()?;
    let content_length = unique_header(headers, &CONTENT_LENGTH)?
        .map(|value| {
            value
                .to_str()
                .map_err(|_| invalid_payload_metadata("Content-Length is not valid text"))?
                .parse::<u64>()
                .map_err(|_| invalid_payload_metadata("Content-Length is not an unsigned integer"))
        })
        .transpose()?;
    let content_range = unique_header(headers, &CONTENT_RANGE)?
        .map(parse_content_range)
        .transpose()?;
    let accept_ranges = unique_header(headers, &ACCEPT_RANGES)?
        .map(|value| {
            let text = value
                .to_str()
                .map_err(|_| invalid_payload_metadata("Accept-Ranges is not valid text"))?;
            if !text.eq_ignore_ascii_case("bytes") {
                return Err(invalid_payload_metadata(
                    "Accept-Ranges must contain the bytes range unit",
                ));
            }
            Ok(value.clone())
        })
        .transpose()?;

    if content_length.is_some_and(|length| length > MAX_BLOB_BYTES) {
        return Err(invalid_payload_metadata(
            "Content-Length is outside the supported payload size",
        ));
    }

    match status {
        200 => {
            if content_range.is_some() {
                return Err(invalid_payload_metadata(
                    "a 200 payload response must not declare Content-Range",
                ));
            }
            if content_length == Some(0) {
                return Err(invalid_payload_metadata(
                    "Content-Length is outside the supported payload size",
                ));
            }
        }
        206 => {
            let range = content_range.as_ref().ok_or_else(|| {
                invalid_payload_metadata("a 206 payload response requires Content-Range")
            })?;
            let PayloadContentRange::Bytes {
                start,
                end,
                complete_length,
            } = range
            else {
                return Err(invalid_payload_metadata(
                    "a 206 payload response requires a satisfied Content-Range",
                ));
            };
            if *complete_length == 0
                || *complete_length > MAX_BLOB_BYTES
                || start > end
                || end >= complete_length
            {
                return Err(invalid_payload_metadata(
                    "Content-Range is outside the supported payload size",
                ));
            }
            if content_length.is_some_and(|length| Some(length) != range.byte_length()) {
                return Err(invalid_payload_metadata(
                    "Content-Length does not match Content-Range",
                ));
            }
            let requested = requested_range
                .ok_or_else(|| invalid_payload_metadata("unsolicited partial payload response"))?;
            validate_response_range(requested, range)?;
        }
        416 => {
            parse_requested_range(requested_range.ok_or_else(|| {
                invalid_payload_metadata("unsolicited unsatisfied-range response")
            })?)?;
            match content_range
                .as_ref()
                .ok_or_else(|| invalid_payload_metadata("a 416 response requires Content-Range"))?
            {
                PayloadContentRange::Unsatisfied { complete_length }
                    if *complete_length > 0 && *complete_length <= MAX_BLOB_BYTES => {}
                PayloadContentRange::Unsatisfied { .. } => {
                    return Err(invalid_payload_metadata(
                        "unsatisfied Content-Range exceeds the supported payload size",
                    ));
                }
                PayloadContentRange::Bytes { .. } => {
                    return Err(invalid_payload_metadata(
                        "a 416 response cannot contain a satisfied Content-Range",
                    ));
                }
            }
        }
        _ => {
            if content_range.is_some() {
                return Err(invalid_payload_metadata(
                    "Content-Range is only valid for HTTP 206 or 416 payload responses",
                ));
            }
        }
    }

    Ok(PayloadMetadata {
        content_type,
        content_length,
        content_range,
        accept_ranges,
    })
}

fn unique_header<'a>(
    headers: &'a HeaderMap,
    name: &HeaderName,
) -> Result<Option<&'a HeaderValue>, HubClientError> {
    let mut values = headers.get_all(name).iter();
    let value = values.next();
    if values.next().is_some() {
        return Err(invalid_payload_metadata(&format!(
            "{} must not be repeated",
            name.as_str()
        )));
    }
    Ok(value)
}

fn parse_content_range(value: &HeaderValue) -> Result<PayloadContentRange, HubClientError> {
    let value = value
        .to_str()
        .map_err(|_| invalid_payload_metadata("Content-Range is not valid text"))?
        .trim();
    let (unit, value) = value
        .split_once(' ')
        .ok_or_else(|| invalid_payload_metadata("Content-Range is malformed"))?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return Err(invalid_payload_metadata(
            "Content-Range must use the bytes range unit",
        ));
    }
    let (bounds, complete_length) = value
        .split_once('/')
        .ok_or_else(|| invalid_payload_metadata("Content-Range is malformed"))?;
    let complete_length = complete_length
        .parse()
        .map_err(|_| invalid_payload_metadata("Content-Range length is malformed"))?;
    if bounds == "*" {
        return Ok(PayloadContentRange::Unsatisfied { complete_length });
    }
    let (start, end) = bounds
        .split_once('-')
        .ok_or_else(|| invalid_payload_metadata("Content-Range is malformed"))?;
    Ok(PayloadContentRange::Bytes {
        start: start
            .parse()
            .map_err(|_| invalid_payload_metadata("Content-Range start is malformed"))?,
        end: end
            .parse()
            .map_err(|_| invalid_payload_metadata("Content-Range end is malformed"))?,
        complete_length,
    })
}

enum RequestedRange {
    Closed { start: u64, end: u64 },
    From(u64),
    Suffix(u64),
}

fn validate_response_range(
    requested: &str,
    response: &PayloadContentRange,
) -> Result<(), HubClientError> {
    let PayloadContentRange::Bytes {
        start,
        end,
        complete_length,
    } = response
    else {
        return Err(invalid_payload_metadata(
            "requested range was not satisfied",
        ));
    };
    let requested = parse_requested_range(requested)?;
    let matches = match requested {
        RequestedRange::Closed {
            start: requested_start,
            end: requested_end,
        } => *start == requested_start && *end <= requested_end,
        RequestedRange::From(requested_start) => *start == requested_start,
        RequestedRange::Suffix(length) => {
            *end + 1 == *complete_length
                && response.byte_length() == Some(length.min(*complete_length))
        }
    };
    if matches {
        Ok(())
    } else {
        Err(invalid_payload_metadata(
            "Content-Range does not satisfy the requested byte range",
        ))
    }
}

fn parse_requested_range(value: &str) -> Result<RequestedRange, HubClientError> {
    let (unit, value) = value
        .trim()
        .split_once('=')
        .ok_or_else(|| invalid_payload_metadata("Range is malformed"))?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return Err(invalid_payload_metadata(
            "Range must use the bytes range unit",
        ));
    }
    if value.contains(',') {
        return Err(invalid_payload_metadata(
            "multipart byte ranges are not supported",
        ));
    }
    let (start, end) = value
        .split_once('-')
        .ok_or_else(|| invalid_payload_metadata("Range is malformed"))?;
    match (start.is_empty(), end.is_empty()) {
        (false, false) => {
            let start = start
                .parse::<u64>()
                .map_err(|_| invalid_payload_metadata("Range start is malformed"))?;
            let end = end
                .parse::<u64>()
                .map_err(|_| invalid_payload_metadata("Range end is malformed"))?;
            if start > end {
                return Err(invalid_payload_metadata("Range start exceeds its end"));
            }
            Ok(RequestedRange::Closed { start, end })
        }
        (false, true) => start
            .parse::<u64>()
            .map(RequestedRange::From)
            .map_err(|_| invalid_payload_metadata("Range start is malformed")),
        (true, false) => {
            let length = end
                .parse::<u64>()
                .map_err(|_| invalid_payload_metadata("Range suffix is malformed"))?;
            if length == 0 {
                return Err(invalid_payload_metadata("Range suffix must be non-zero"));
            }
            Ok(RequestedRange::Suffix(length))
        }
        (true, true) => Err(invalid_payload_metadata("Range is empty")),
    }
}

fn invalid_payload_metadata(message: &str) -> HubClientError {
    HubClientError::InvalidPayloadMetadata(message.to_owned())
}

struct ValidatedPayloadBody {
    inner: PayloadBody,
    maximum_length: u64,
    expected_length: Option<u64>,
    received: u64,
    finished: bool,
}

impl ValidatedPayloadBody {
    fn new(inner: PayloadBody, maximum_length: u64, expected_length: Option<u64>) -> Self {
        Self {
            inner,
            maximum_length,
            expected_length,
            received: 0,
            finished: false,
        }
    }
}

impl Stream for ValidatedPayloadBody {
    type Item = Result<Bytes, HubTransferError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.finished {
            return Poll::Ready(None);
        }
        match self.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                let received = self.received.saturating_add(chunk.len() as u64);
                if received > self.maximum_length {
                    self.finished = true;
                    self.inner = Box::pin(futures_util::stream::empty());
                    return Poll::Ready(Some(Err(HubTransferError::Transport(format!(
                        "Home Hub payload exceeded its allowed {maximum} bytes",
                        maximum = self.maximum_length
                    )))));
                }
                self.received = received;
                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(Some(Err(error))) => {
                self.finished = true;
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(None) => {
                self.finished = true;
                if self
                    .expected_length
                    .is_some_and(|expected| expected != self.received)
                {
                    Poll::Ready(Some(Err(HubTransferError::Transport(format!(
                        "Home Hub payload ended after {received} bytes; expected {expected}",
                        received = self.received,
                        expected = self.expected_length.expect("checked as present")
                    )))))
                } else {
                    Poll::Ready(None)
                }
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

pub fn canonicalize_base_url(input: &str) -> Result<Url, HubClientError> {
    let mut url =
        Url::parse(input).map_err(|error| HubClientError::InvalidBaseUrl(error.to_string()))?;
    if url.scheme() != "https" {
        return Err(HubClientError::InsecureBaseUrl);
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(HubClientError::UnsafeBaseUrl);
    }
    if url.host_str().is_none() {
        return Err(HubClientError::InvalidBaseUrl("missing host".to_owned()));
    }

    match url.path().trim_end_matches('/') {
        "" | "/audetic" => {}
        _ => return Err(HubClientError::UnexpectedBasePath),
    }
    if explicit_port(input).is_some_and(|port| port != TAILSCALE_HTTPS_PORT) {
        return Err(HubClientError::UnexpectedPort);
    }
    url.set_path(HUB_API_MOUNT_PATH);
    if url.port().is_none() {
        url.set_port(Some(TAILSCALE_HTTPS_PORT))
            .map_err(|_| HubClientError::InvalidBaseUrl("URL cannot contain a port".to_owned()))?;
    }
    Ok(url)
}

fn explicit_port(input: &str) -> Option<u16> {
    let authority = input.split_once("://")?.1.split('/').next()?;
    let host_and_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if host_and_port.starts_with('[') {
        let end = host_and_port.find(']')?;
        return host_and_port[end + 1..].strip_prefix(':')?.parse().ok();
    }
    host_and_port.rsplit_once(':')?.1.parse().ok()
}

pub async fn discover_hubs<T: HubTransport>(
    transport: T,
    candidates: impl IntoIterator<Item = String>,
    expected_owner_login: &str,
) -> DiscoveryOutcome {
    let mut compatible = Vec::new();
    let mut failures = Vec::new();
    for candidate in candidates {
        let client = match HubClient::with_transport(&candidate, transport.clone()) {
            Ok(client) => client,
            Err(error) => {
                failures.push(DiscoveryFailure {
                    candidate,
                    reason: error.to_string(),
                });
                continue;
            }
        };
        match client
            .handshake(HandshakeExpectation {
                hub_id: None,
                owner_login: Some(expected_owner_login),
            })
            .await
        {
            Ok(connection) => compatible.push(connection),
            Err(error) => failures.push(DiscoveryFailure {
                candidate,
                reason: error.to_string(),
            }),
        }
    }

    match compatible.len() {
        0 => DiscoveryOutcome::None { failures },
        1 => DiscoveryOutcome::One(compatible.remove(0)),
        _ => DiscoveryOutcome::Multiple(compatible),
    }
}

fn http_error(response: TransportResponse) -> HubClientError {
    let message = serde_json::from_slice::<HubApiError>(&response.body)
        .map(|error| error.message)
        .unwrap_or_else(|_| String::from_utf8_lossy(&response.body).into_owned());
    HubClientError::Http {
        status: response.status,
        message,
        retry_after: retry_after(&response.headers),
    }
}

fn retry_after(headers: &HeaderMap) -> Option<String> {
    let mut values = headers.get_all(RETRY_AFTER).iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    value.to_str().ok().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::protocol::{
        DictationPayload, DictationSnapshot, ProtocolRange, RecordingPayloadDescriptor, Snapshot,
        SnapshotBatchResponse, SnapshotDisposition, SnapshotResult, HUB_ID_HEADER,
    };
    use futures_util::TryStreamExt;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct CapturedRequest {
        method: &'static str,
        url: Url,
        headers: BTreeMap<String, String>,
        body: Vec<u8>,
    }

    #[derive(Clone, Default)]
    struct FakeTransport {
        requests: Arc<Mutex<Vec<CapturedRequest>>>,
        responses: Arc<Mutex<VecDeque<Result<TransportResponse, String>>>>,
    }

    impl FakeTransport {
        fn with_responses(responses: Vec<TransportResponse>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into_iter().map(Ok).collect())),
                ..Self::default()
            }
        }

        fn requests(&self) -> Vec<CapturedRequest> {
            self.requests.lock().unwrap().clone()
        }

        fn respond(
            &self,
            method: &'static str,
            url: Url,
            headers: BTreeMap<String, String>,
            body: Vec<u8>,
        ) -> Result<TransportResponse, String> {
            self.requests.lock().unwrap().push(CapturedRequest {
                method,
                url,
                headers,
                body,
            });
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err("no fake response".to_owned()))
        }
    }

    #[async_trait]
    impl HubTransport for FakeTransport {
        async fn get(
            &self,
            url: Url,
            headers: BTreeMap<String, String>,
        ) -> Result<TransportResponse, String> {
            self.respond("GET", url, headers, Vec::new())
        }

        async fn post(
            &self,
            url: Url,
            headers: BTreeMap<String, String>,
            body: Vec<u8>,
        ) -> Result<TransportResponse, String> {
            self.respond("POST", url, headers, body)
        }

        async fn patch(
            &self,
            url: Url,
            headers: BTreeMap<String, String>,
            body: Vec<u8>,
        ) -> Result<TransportResponse, String> {
            self.respond("PATCH", url, headers, body)
        }

        async fn delete(
            &self,
            url: Url,
            headers: BTreeMap<String, String>,
        ) -> Result<TransportResponse, String> {
            self.respond("DELETE", url, headers, Vec::new())
        }

        async fn put_file(
            &self,
            url: Url,
            headers: BTreeMap<String, String>,
            path: &Path,
            _byte_size: u64,
            _media_type: &str,
        ) -> Result<TransportResponse, String> {
            let body = std::fs::read(path).map_err(|error| error.to_string())?;
            self.respond("PUT", url, headers, body)
        }

        async fn get_stream(
            &self,
            url: Url,
            headers: BTreeMap<String, String>,
        ) -> Result<StreamingTransportResponse, String> {
            let response = self.respond("GET_STREAM", url, headers, Vec::new())?;
            Ok(StreamingTransportResponse {
                status: response.status,
                headers: response.headers,
                body: Box::pin(futures_util::stream::once(async move {
                    Ok(Bytes::from(response.body))
                })),
            })
        }
    }

    struct ObservedBody {
        polls: Arc<AtomicUsize>,
        dropped: Arc<AtomicBool>,
        sent: bool,
    }

    impl Stream for ObservedBody {
        type Item = Result<Bytes, HubTransferError>;

        fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            self.polls.fetch_add(1, Ordering::SeqCst);
            if self.sent {
                Poll::Pending
            } else {
                self.sent = true;
                Poll::Ready(Some(Ok(Bytes::from_static(b"data"))))
            }
        }
    }

    impl Drop for ObservedBody {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    #[derive(Clone)]
    struct ObservedStreamingTransport {
        hub_id: HubId,
        polls: Arc<AtomicUsize>,
        dropped: Arc<AtomicBool>,
    }

    #[async_trait]
    impl HubTransport for ObservedStreamingTransport {
        async fn get(
            &self,
            _url: Url,
            _headers: BTreeMap<String, String>,
        ) -> Result<TransportResponse, String> {
            Err("buffered GET must not be used for payloads".to_owned())
        }

        async fn get_stream(
            &self,
            _url: Url,
            _headers: BTreeMap<String, String>,
        ) -> Result<StreamingTransportResponse, String> {
            let mut headers = hub_headers(self.hub_id);
            headers.insert(CONTENT_LENGTH, HeaderValue::from_static("4"));
            Ok(StreamingTransportResponse {
                status: 200,
                headers,
                body: Box::pin(ObservedBody {
                    polls: Arc::clone(&self.polls),
                    dropped: Arc::clone(&self.dropped),
                    sent: false,
                }),
            })
        }
    }

    use uuid::Uuid;

    fn hub_id() -> HubId {
        HubId::from_uuid(Uuid::new_v4())
    }

    fn info_response(hub_id: HubId, owner: &str) -> TransportResponse {
        TransportResponse {
            status: 200,
            headers: hub_headers(hub_id),
            body: serde_json::to_vec(&HubInfo {
                hub_id,
                owner_login: owner.to_owned(),
                device_name: Some("Home Hub".to_owned()),
                protocol: ProtocolRange::supported(),
                audetic_version: "0.1.26".to_owned(),
            })
            .unwrap(),
        }
    }

    fn hub_response(hub_id: HubId, status: u16, body: Vec<u8>) -> TransportResponse {
        TransportResponse {
            status,
            headers: hub_headers(hub_id),
            body,
        }
    }

    fn hub_headers(hub_id: HubId) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            HUB_ID_HEADER,
            HeaderValue::from_str(&hub_id.to_string()).unwrap(),
        );
        headers
    }

    fn connection(hub_id: HubId) -> HubConnection {
        HubConnection {
            base_url: "https://hub.example.ts.net:8443/audetic/".to_owned(),
            hub_id,
            owner_login: "owner@example.com".to_owned(),
        }
    }

    #[test]
    fn canonical_base_url_preserves_the_serve_mount_path() {
        for input in [
            "https://hub.example.ts.net",
            "https://hub.example.ts.net/",
            "https://hub.example.ts.net/audetic",
            "https://hub.example.ts.net:8443/audetic/",
        ] {
            assert_eq!(
                canonicalize_base_url(input).unwrap().as_str(),
                "https://hub.example.ts.net:8443/audetic/",
                "{input}"
            );
        }
    }

    #[test]
    fn canonical_base_url_rejects_unsafe_or_unexpected_urls() {
        for input in [
            "http://hub.example.ts.net/audetic/",
            "https://user@hub.example.ts.net/audetic/",
            "https://hub.example.ts.net/audetic/?x=1",
            "https://hub.example.ts.net/audetic/#fragment",
            "https://hub.example.ts.net/other/",
            "https://hub.example.ts.net:443/audetic/",
        ] {
            assert!(canonicalize_base_url(input).is_err(), "{input}");
        }
    }

    #[tokio::test]
    async fn handshake_joins_a_relative_info_path_and_verifies_hub_owner_and_protocol() {
        let hub_id = hub_id();
        let transport =
            FakeTransport::with_responses(vec![info_response(hub_id, "Alice@Example.com")]);
        let client =
            HubClient::with_transport("https://hub.example.ts.net/audetic", transport.clone())
                .unwrap();

        let connection = client
            .handshake(HandshakeExpectation {
                hub_id: Some(hub_id),
                owner_login: Some("Alice@Example.com"),
            })
            .await
            .unwrap();

        assert_eq!(connection.connection.hub_id, hub_id);
        let requests = transport.requests();
        assert_eq!(
            requests[0].url.as_str(),
            "https://hub.example.ts.net:8443/audetic/v1/info"
        );
        assert_eq!(requests[0].headers[HUB_ID_HEADER], hub_id.to_string());
        assert_eq!(requests[0].headers[PROTOCOL_VERSION_HEADER], "1");
    }

    #[tokio::test]
    async fn handshake_rejects_response_hub_identity_drift() {
        let expected = hub_id();
        let actual = hub_id();
        let client = HubClient::with_transport(
            "https://hub.example.ts.net/audetic/",
            FakeTransport::with_responses(vec![info_response(actual, "owner@example.com")]),
        )
        .unwrap();

        assert!(matches!(
            client
                .handshake(HandshakeExpectation {
                    hub_id: Some(expected),
                    owner_login: Some("owner@example.com"),
                })
                .await,
            Err(HubClientError::WrongHubId { .. })
        ));
    }

    #[tokio::test]
    async fn expected_hub_identity_is_verified_even_on_error_responses() {
        let expected = hub_id();
        let actual = hub_id();
        let client = HubClient::with_transport(
            "https://hub.example.ts.net/audetic/",
            FakeTransport::with_responses(vec![hub_response(actual, 503, b"offline".to_vec())]),
        )
        .unwrap();

        assert!(matches!(
            client
                .handshake(HandshakeExpectation {
                    hub_id: Some(expected),
                    owner_login: Some("owner@example.com"),
                })
                .await,
            Err(HubClientError::WrongHubId {
                expected: expected_id,
                actual: actual_id,
            }) if expected_id == expected && actual_id == actual
        ));
    }

    #[tokio::test]
    async fn handshake_rejects_header_body_hub_disagreement_and_owner_normalization() {
        let body_hub = hub_id();
        let mut inconsistent = info_response(body_hub, "Alice@Example.com");
        inconsistent.headers.insert(
            HUB_ID_HEADER,
            HeaderValue::from_str(&hub_id().to_string()).unwrap(),
        );
        let client = HubClient::with_transport(
            "https://hub.example.ts.net/audetic/",
            FakeTransport::with_responses(vec![inconsistent]),
        )
        .unwrap();
        assert!(matches!(
            client.handshake(HandshakeExpectation::default()).await,
            Err(HubClientError::InconsistentHubId)
        ));

        let client = HubClient::with_transport(
            "https://hub.example.ts.net/audetic/",
            FakeTransport::with_responses(vec![info_response(body_hub, "Alice@Example.com")]),
        )
        .unwrap();
        assert!(matches!(
            client
                .handshake(HandshakeExpectation {
                    hub_id: None,
                    owner_login: Some("alice@example.com"),
                })
                .await,
            Err(HubClientError::WrongOwner { .. })
        ));
    }

    #[tokio::test]
    async fn discovery_selects_exactly_one_compatible_home_hub() {
        let hub_id = hub_id();
        let transport = FakeTransport::with_responses(vec![
            TransportResponse {
                status: 403,
                headers: HeaderMap::new(),
                body: b"wrong owner".to_vec(),
            },
            info_response(hub_id, "owner@example.com"),
        ]);

        let outcome = discover_hubs(
            transport,
            vec![
                "https://not-ours.example.ts.net/audetic/".to_owned(),
                "https://hub.example.ts.net/audetic/".to_owned(),
            ],
            "owner@example.com",
        )
        .await;

        assert!(matches!(
            outcome,
            DiscoveryOutcome::One(HubCandidate { connection: HubConnection { hub_id: id, .. }, .. }) if id == hub_id
        ));
    }

    #[tokio::test]
    async fn discovery_reports_multiple_compatible_hubs_instead_of_choosing() {
        let transport = FakeTransport::with_responses(vec![
            info_response(hub_id(), "owner@example.com"),
            info_response(hub_id(), "owner@example.com"),
        ]);

        let outcome = discover_hubs(
            transport,
            vec![
                "https://one.example.ts.net/audetic/".to_owned(),
                "https://two.example.ts.net/audetic/".to_owned(),
            ],
            "owner@example.com",
        )
        .await;

        assert!(matches!(outcome, DiscoveryOutcome::Multiple(hubs) if hubs.len() == 2));
    }

    #[tokio::test]
    async fn replication_capability_preserves_snapshot_then_blob_requests() {
        let hub_id = hub_id();
        let record_id = RecordId::new();
        let snapshot = Snapshot::Dictation(DictationSnapshot {
            kind: RecordKind::Dictation,
            schema_version: 1,
            record_id,
            origin_device_id: audetic_core::sync::DeviceId::new(),
            local_version: 1,
            created_at: "2026-09-04T10:00:00Z".to_owned(),
            updated_at: "2026-09-04T10:00:00Z".to_owned(),
            payload: DictationPayload {
                text: "portable".to_owned(),
                recording_payload: RecordingPayloadDescriptor::unavailable(),
            },
        });
        let response = SnapshotBatchResponse {
            results: vec![SnapshotResult {
                record_id,
                disposition: SnapshotDisposition::Accepted,
                authoritative_revision: Some(4),
                error_code: None,
                message: None,
            }],
        };
        let transport = FakeTransport::with_responses(vec![
            hub_response(hub_id, 200, serde_json::to_vec(&response).unwrap()),
            hub_response(hub_id, 201, Vec::new()),
        ]);
        let client =
            HubClient::with_transport(&connection(hub_id).base_url, transport.clone()).unwrap();
        let batch = SnapshotBatch {
            snapshots: vec![snapshot],
        };
        ReplicationTransport::upload_snapshots(&client, &connection(hub_id), batch)
            .await
            .unwrap();
        let temp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(temp.path(), b"blob bytes").unwrap();
        ReplicationTransport::upload_blob(
            &client,
            &connection(hub_id),
            BlobUpload {
                record_id,
                checksum: "a".repeat(64),
                source_path: temp.path().to_path_buf(),
                byte_size: 10,
                media_type: "audio/wav".to_owned(),
            },
        )
        .await
        .unwrap();

        let requests = transport.requests();
        assert_eq!(requests[0].method, "POST");
        assert!(requests[0].url.path().ends_with("/v1/snapshots"));
        assert!(!requests[0].body.is_empty());
        assert_eq!(requests[1].method, "PUT");
        assert!(requests[1]
            .url
            .path()
            .ends_with(&format!("/v1/blobs/{}", "a".repeat(64))));
        assert_eq!(requests[1].body, b"blob bytes");
    }

    #[tokio::test]
    async fn remote_library_capability_preserves_reads_and_mutations() {
        let hub_id = hub_id();
        let transport = FakeTransport::with_responses(vec![
            hub_response(
                hub_id,
                200,
                serde_json::to_vec(&DictationPage {
                    items: Vec::new(),
                    next_cursor: None,
                })
                .unwrap(),
            ),
            hub_response(hub_id, 204, Vec::new()),
        ]);
        let hub = connection(hub_id);
        let client = HubClient::with_transport(&hub.base_url, transport.clone()).unwrap();

        RemoteDictationLibrary::page_dictations(
            &client,
            &hub,
            Some("needle"),
            None,
            None,
            Some("cursor"),
            25,
        )
        .await
        .unwrap();
        RemoteLibraryMutations::delete_record(&client, &hub, RecordId::new(), RecordKind::Meeting)
            .await
            .unwrap();

        let requests = transport.requests();
        assert_eq!(requests[0].method, "GET");
        assert!(requests[0].url.as_str().contains("q=needle"));
        assert!(requests[0].url.as_str().contains("cursor=cursor"));
        assert_eq!(requests[1].method, "DELETE");
        assert!(requests[1].url.path().contains("/v1/meetings/"));
    }

    #[tokio::test]
    async fn payload_capability_forwards_range_and_returns_transport_neutral_stream() {
        let hub_id = hub_id();
        let mut response = hub_response(hub_id, 206, b"2345".to_vec());
        response
            .headers
            .insert(CONTENT_TYPE, HeaderValue::from_static("audio/wav"));
        response
            .headers
            .insert(CONTENT_LENGTH, HeaderValue::from_static("4"));
        response
            .headers
            .insert(CONTENT_RANGE, HeaderValue::from_static("bytes 2-5/10"));
        response
            .headers
            .insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        let transport = FakeTransport::with_responses(vec![response]);
        let hub = connection(hub_id);
        let client = HubClient::with_transport(&hub.base_url, transport.clone()).unwrap();

        let streamed = RemotePayloadSource::stream_payload(
            &client,
            &hub,
            RecordId::new(),
            RecordKind::Dictation,
            Some("bytes=2-5"),
        )
        .await
        .unwrap();
        assert_eq!(streamed.status, 206);
        assert_eq!(
            streamed.metadata.content_range,
            Some(PayloadContentRange::Bytes {
                start: 2,
                end: 5,
                complete_length: 10,
            })
        );
        let body = streamed
            .body
            .try_fold(Vec::new(), |mut body, chunk| async move {
                body.extend_from_slice(&chunk);
                Ok(body)
            })
            .await
            .unwrap();
        assert_eq!(body, b"2345");
        let requests = transport.requests();
        assert_eq!(requests[0].headers["range"], "bytes=2-5");
    }

    #[tokio::test]
    async fn payload_rejects_duplicate_or_malformed_hub_identity_before_forwarding() {
        for malformed in [false, true] {
            let hub_id = hub_id();
            let mut response = hub_response(hub_id, 200, b"payload".to_vec());
            response
                .headers
                .insert(CONTENT_LENGTH, HeaderValue::from_static("7"));
            if malformed {
                response.headers.insert(
                    HUB_ID_HEADER,
                    HeaderValue::from_bytes(&[0xff]).expect("opaque header value"),
                );
            } else {
                response.headers.append(
                    HUB_ID_HEADER,
                    HeaderValue::from_str(&hub_id.to_string()).unwrap(),
                );
            }
            let hub = connection(hub_id);
            let client = HubClient::with_transport(
                &hub.base_url,
                FakeTransport::with_responses(vec![response]),
            )
            .unwrap();

            let error = client
                .stream_payload(hub_id, RecordKind::Dictation, RecordId::new(), None)
                .await
                .err()
                .expect("invalid identity must reject the response");

            if malformed {
                assert!(matches!(error, HubClientError::InvalidHubId(_)));
            } else {
                assert!(matches!(error, HubClientError::DuplicateHubIdHeader));
            }
        }
    }

    #[tokio::test]
    async fn payload_metadata_must_match_the_requested_range_before_forwarding() {
        let hub_id = hub_id();
        let mut response = hub_response(hub_id, 206, b"2345".to_vec());
        response
            .headers
            .insert(CONTENT_LENGTH, HeaderValue::from_static("5"));
        response
            .headers
            .insert(CONTENT_RANGE, HeaderValue::from_static("bytes 2-5/10"));
        let hub = connection(hub_id);
        let client =
            HubClient::with_transport(&hub.base_url, FakeTransport::with_responses(vec![response]))
                .unwrap();

        assert!(matches!(
            client
                .stream_payload(
                    hub_id,
                    RecordKind::Dictation,
                    RecordId::new(),
                    Some("bytes=2-5")
                )
                .await,
            Err(HubClientError::InvalidPayloadMetadata(_))
        ));
    }

    #[tokio::test]
    async fn payload_stream_preserves_unsatisfied_range_response_metadata() {
        let hub_id = hub_id();
        let mut response = hub_response(hub_id, 416, b"range rejected".to_vec());
        response
            .headers
            .insert(CONTENT_TYPE, HeaderValue::from_static("text/plain"));
        response
            .headers
            .insert(CONTENT_LENGTH, HeaderValue::from_static("14"));
        response
            .headers
            .insert(CONTENT_RANGE, HeaderValue::from_static("bytes */10"));
        let hub = connection(hub_id);
        let client =
            HubClient::with_transport(&hub.base_url, FakeTransport::with_responses(vec![response]))
                .unwrap();

        let streamed = client
            .stream_payload(
                hub_id,
                RecordKind::Dictation,
                RecordId::new(),
                Some("bytes=20-30"),
            )
            .await
            .unwrap();

        assert_eq!(streamed.status, 416);
        assert_eq!(
            streamed.metadata.content_range,
            Some(PayloadContentRange::Unsatisfied {
                complete_length: 10
            })
        );
        assert_eq!(
            streamed.body.try_collect::<Vec<_>>().await.unwrap(),
            vec![Bytes::from_static(b"range rejected")]
        );
    }

    #[tokio::test]
    async fn payload_stream_enforces_declared_length_without_buffering() {
        let hub_id = hub_id();
        let mut response = hub_response(hub_id, 200, b"12345".to_vec());
        response
            .headers
            .insert(CONTENT_LENGTH, HeaderValue::from_static("4"));
        let hub = connection(hub_id);
        let client =
            HubClient::with_transport(&hub.base_url, FakeTransport::with_responses(vec![response]))
                .unwrap();

        let stream = client
            .stream_payload(hub_id, RecordKind::Dictation, RecordId::new(), None)
            .await
            .unwrap();
        let error = stream.body.try_collect::<Vec<_>>().await.unwrap_err();

        assert!(error.to_string().contains("allowed 4 bytes"));
    }

    #[tokio::test]
    async fn payload_stream_preserves_backpressure_and_drop_cancellation() {
        let hub_id = hub_id();
        let polls = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicBool::new(false));
        let client = HubClient::with_transport(
            &connection(hub_id).base_url,
            ObservedStreamingTransport {
                hub_id,
                polls: Arc::clone(&polls),
                dropped: Arc::clone(&dropped),
            },
        )
        .unwrap();

        let mut streamed = client
            .stream_payload(hub_id, RecordKind::Dictation, RecordId::new(), None)
            .await
            .unwrap();
        assert_eq!(polls.load(Ordering::SeqCst), 0);

        assert_eq!(
            streamed.body.next().await.unwrap().unwrap(),
            Bytes::from_static(b"data")
        );
        assert_eq!(polls.load(Ordering::SeqCst), 1);
        drop(streamed);
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn retry_after_and_rate_limit_status_survive_typed_error_conversion() {
        let hub_id = hub_id();
        let mut response = hub_response(
            hub_id,
            429,
            br#"{"error":true,"code":"busy","message":"try later"}"#.to_vec(),
        );
        response
            .headers
            .insert(RETRY_AFTER, HeaderValue::from_static("30"));
        let hub = connection(hub_id);
        let client =
            HubClient::with_transport(&hub.base_url, FakeTransport::with_responses(vec![response]))
                .unwrap();

        let error =
            RemoteDictationLibrary::page_dictations(&client, &hub, None, None, None, None, 25)
                .await
                .unwrap_err();

        assert!(matches!(
            &error,
            HubTransferError::Http {
                status: 429,
                retry_after: Some(value),
                ..
            } if value == "30"
        ));
        assert!(error.is_retryable());
    }

    #[tokio::test]
    async fn capability_errors_keep_http_status_for_retry_classification() {
        let hub_id = hub_id();
        let transport = FakeTransport::with_responses(vec![hub_response(
            hub_id,
            503,
            br#"{"error":true,"code":"unavailable","message":"try later"}"#.to_vec(),
        )]);
        let hub = connection(hub_id);
        let client = HubClient::with_transport(&hub.base_url, transport).unwrap();

        let error =
            RemoteDictationLibrary::page_dictations(&client, &hub, None, None, None, None, 25)
                .await
                .unwrap_err();

        assert!(matches!(&error, HubTransferError::Http { status: 503, .. }));
        assert!(error.is_retryable());
    }
}
