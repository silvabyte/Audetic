use async_trait::async_trait;
use audetic_core::sync::{HubCandidate, HubConnection, HubId, RecordId};
use reqwest::Url;
use thiserror::Error;

use std::collections::BTreeMap;
use std::time::Duration;

use super::protocol::{
    DictationPage, HubApiError, HubInfo, MeetingPage, MeetingTitlePatch, RecordKind, SharedMeeting,
    SnapshotBatch, SnapshotBatchResponse, HUB_API_MOUNT_PATH, HUB_DICTATIONS_PATH, HUB_ID_HEADER,
    HUB_INFO_PATH, HUB_MEETINGS_PATH, HUB_SNAPSHOTS_PATH, PROTOCOL_VERSION,
    PROTOCOL_VERSION_HEADER, TAILSCALE_HTTPS_PORT,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransportResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
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
}

#[derive(Clone, Debug)]
pub struct ReqwestHubTransport {
    client: reqwest::Client,
}

impl ReqwestHubTransport {
    pub fn new() -> Result<Self, reqwest::Error> {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(5))
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
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (name.as_str().to_owned(), value.to_owned()))
            })
            .collect();
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
}

async fn response_from_reqwest(response: reqwest::Response) -> Result<TransportResponse, String> {
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_owned(), value.to_owned()))
        })
        .collect();
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

#[derive(Clone, Copy, Debug, Default)]
pub struct HandshakeExpectation<'a> {
    pub hub_id: Option<HubId>,
    pub owner_login: Option<&'a str>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryFailure {
    pub candidate: String,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiscoveryOutcome {
    None { failures: Vec<DiscoveryFailure> },
    One(HubCandidate),
    Multiple(Vec<HubCandidate>),
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
    Http { status: u16, message: String },
    #[error("Home Hub returned malformed discovery JSON: {0}")]
    InvalidInfo(#[from] serde_json::Error),
    #[error("Home Hub response omitted {HUB_ID_HEADER}")]
    MissingHubIdHeader,
    #[error("Home Hub returned an invalid Hub ID in {0}")]
    InvalidHubId(&'static str),
    #[error("Home Hub response body and header identify different hubs")]
    InconsistentHubId,
    #[error("expected Home Hub {expected}, but reached {actual}")]
    WrongHubId { expected: HubId, actual: HubId },
    #[error("expected Tailscale owner {expected:?}, but hub belongs to {actual:?}")]
    WrongOwner { expected: String, actual: String },
    #[error("Home Hub protocol range {minimum}..={current} is incompatible with protocol {PROTOCOL_VERSION}")]
    IncompatibleProtocol { minimum: u16, current: u16 },
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
        if !(200..300).contains(&response.status) {
            return Err(http_error(response));
        }

        let header_hub_id = response
            .headers
            .get(HUB_ID_HEADER)
            .ok_or(HubClientError::MissingHubIdHeader)
            .and_then(|value| {
                value
                    .parse::<HubId>()
                    .map_err(|_| HubClientError::InvalidHubId(HUB_ID_HEADER))
            })?;
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
    let actual = response
        .headers
        .get(HUB_ID_HEADER)
        .ok_or(HubClientError::MissingHubIdHeader)?
        .parse::<HubId>()
        .map_err(|_| HubClientError::InvalidHubId(HUB_ID_HEADER))?;
    if actual != expected {
        return Err(HubClientError::WrongHubId { expected, actual });
    }
    Ok(())
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::protocol::{ProtocolRange, HUB_ID_HEADER};
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    type CapturedRequest = (Url, BTreeMap<String, String>);

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
    }

    #[async_trait]
    impl HubTransport for FakeTransport {
        async fn get(
            &self,
            url: Url,
            headers: BTreeMap<String, String>,
        ) -> Result<TransportResponse, String> {
            self.requests.lock().unwrap().push((url, headers));
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err("no fake response".to_owned()))
        }
    }

    use uuid::Uuid;

    fn hub_id() -> HubId {
        HubId::from_uuid(Uuid::new_v4())
    }

    fn info_response(hub_id: HubId, owner: &str) -> TransportResponse {
        TransportResponse {
            status: 200,
            headers: BTreeMap::from([(HUB_ID_HEADER.to_owned(), hub_id.to_string())]),
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
            requests[0].0.as_str(),
            "https://hub.example.ts.net:8443/audetic/v1/info"
        );
        assert_eq!(requests[0].1[HUB_ID_HEADER], hub_id.to_string());
        assert_eq!(requests[0].1[PROTOCOL_VERSION_HEADER], "1");
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
    async fn handshake_rejects_header_body_hub_disagreement_and_owner_normalization() {
        let body_hub = hub_id();
        let mut inconsistent = info_response(body_hub, "Alice@Example.com");
        inconsistent
            .headers
            .insert(HUB_ID_HEADER.to_owned(), hub_id().to_string());
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
                headers: BTreeMap::new(),
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
}
