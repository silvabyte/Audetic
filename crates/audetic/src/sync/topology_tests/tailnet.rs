use async_trait::async_trait;
use axum::body::Body;
use axum::http::{HeaderValue, Method, Request, Response};
use axum::Router;
use futures_util::{StreamExt, TryStreamExt};
use reqwest::Url;
use semver::Version;
use tokio::sync::Notify;
use tower::ServiceExt;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::sync::client::{HubTransport, StreamingTransportResponse, TransportResponse};
use crate::sync::identity::TAILSCALE_USER_LOGIN_HEADER;
use crate::sync::protocol::{ServeSpec, PROTOCOL_VERSION_HEADER, TAILSCALE_FUNNEL_REQUEST_HEADER};
use crate::sync::runtime::{HubListenerFuture, HubRuntimeLauncher, RuntimeError};
use crate::sync::server::HubServer;
use crate::sync::tailscale::{
    MappingState, ServeAssessment, TailscaleControl, TailscaleError, TailscalePeer, TailscaleStatus,
};
use crate::sync::transport::HubTransferError;

#[derive(Clone)]
pub(super) struct FaultGate {
    entered: Arc<Notify>,
    is_entered: Arc<AtomicBool>,
    release: Arc<Notify>,
    is_released: Arc<AtomicBool>,
}

impl FaultGate {
    pub(super) fn new() -> Self {
        Self {
            entered: Arc::new(Notify::new()),
            is_entered: Arc::new(AtomicBool::new(false)),
            release: Arc::new(Notify::new()),
            is_released: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(super) async fn wait_entered(&self) {
        while !self.is_entered.load(Ordering::Acquire) {
            self.entered.notified().await;
        }
    }

    pub(super) fn release(&self) {
        self.is_released.store(true, Ordering::Release);
        self.release.notify_waiters();
    }
}

#[derive(Clone)]
pub(super) enum OperationFault {
    FailBeforeDispatch(&'static str),
    LoseResponseAfterDispatch(&'static str),
    HoldBeforeDispatch(FaultGate),
    TruncateRequestBody(usize),
    CorruptRequestBody,
    TruncateResponseBody(usize),
    CorruptResponseBody,
    OverrideIdentity(Option<String>),
    OverrideProtocol(Option<String>),
    FunnelRequest,
}

struct ScriptedFault {
    source: String,
    target: String,
    method: Method,
    path: String,
    fault: OperationFault,
}

#[derive(Clone)]
struct NodeState {
    dns_name: String,
    owner_login: String,
    tagged: bool,
    online: bool,
    backend_state: String,
    mapping: MappingState,
    funnel: bool,
    router: Option<Router>,
}

#[derive(Default)]
struct TailnetState {
    nodes: BTreeMap<String, NodeState>,
    partitions: BTreeSet<(String, String)>,
    faults: VecDeque<ScriptedFault>,
    dispatched: Vec<(String, String, Method, String)>,
}

#[derive(Clone, Default)]
pub(super) struct FakeTailnet {
    state: Arc<Mutex<TailnetState>>,
}

impl FakeTailnet {
    pub(super) fn add_node(&self, name: &str, owner_login: &str) {
        self.state.lock().unwrap().nodes.insert(
            name.to_owned(),
            NodeState {
                dns_name: format!("{name}.audetic.test.ts.net"),
                owner_login: owner_login.to_owned(),
                tagged: false,
                online: true,
                backend_state: "Running".into(),
                mapping: MappingState::Vacant,
                funnel: false,
                router: None,
            },
        );
    }

    pub(super) fn base_url(&self, name: &str) -> String {
        ServeSpec::audetic().base_url(&self.node(name).dns_name)
    }

    pub(super) fn transport(&self, source: &str) -> TailnetTransport {
        TailnetTransport {
            tailnet: self.clone(),
            source: source.to_owned(),
        }
    }

    pub(super) fn tailscale(&self, node: &str) -> Arc<dyn TailscaleControl> {
        Arc::new(FakeTailscale {
            tailnet: self.clone(),
            node: node.to_owned(),
        })
    }

    pub(super) fn launcher(&self, node: &str) -> Arc<dyn HubRuntimeLauncher> {
        Arc::new(TailnetHubLauncher {
            tailnet: self.clone(),
            node: node.to_owned(),
        })
    }

    pub(super) fn set_tagged(&self, node: &str, tagged: bool) {
        self.node_mut(node, |state| state.tagged = tagged);
    }

    pub(super) fn set_online(&self, node: &str, online: bool) {
        self.node_mut(node, |state| state.online = online);
    }

    pub(super) fn set_funnel(&self, node: &str, enabled: bool) {
        self.node_mut(node, |state| state.funnel = enabled);
    }

    pub(super) fn set_mapping(&self, node: &str, mapping: MappingState) {
        self.node_mut(node, |state| state.mapping = mapping);
    }

    pub(super) fn partition(&self, source: &str, target: &str) {
        self.state
            .lock()
            .unwrap()
            .partitions
            .insert((source.to_owned(), target.to_owned()));
    }

    pub(super) fn heal(&self, source: &str, target: &str) {
        self.state
            .lock()
            .unwrap()
            .partitions
            .remove(&(source.to_owned(), target.to_owned()));
    }

    pub(super) fn fault(
        &self,
        source: &str,
        target: &str,
        method: Method,
        path: &str,
        fault: OperationFault,
    ) {
        self.state.lock().unwrap().faults.push_back(ScriptedFault {
            source: source.to_owned(),
            target: target.to_owned(),
            method,
            path: path.to_owned(),
            fault,
        });
    }

    pub(super) fn published_router_count(&self, node: &str) -> usize {
        usize::from(self.node(node).router.is_some())
    }

    pub(super) fn dispatch_count(&self, method: Method, path: &str) -> usize {
        self.state
            .lock()
            .unwrap()
            .dispatched
            .iter()
            .filter(|(_, _, actual_method, actual_path)| {
                *actual_method == method && actual_path == path
            })
            .count()
    }

    fn node(&self, name: &str) -> NodeState {
        self.state.lock().unwrap().nodes[name].clone()
    }

    fn node_mut(&self, name: &str, update: impl FnOnce(&mut NodeState)) {
        update(self.state.lock().unwrap().nodes.get_mut(name).unwrap());
    }

    fn target_for_host(&self, host: &str) -> Option<String> {
        self.state
            .lock()
            .unwrap()
            .nodes
            .iter()
            .find_map(|(name, node)| (node.dns_name == host).then(|| name.clone()))
    }

    fn take_fault(
        &self,
        source: &str,
        target: &str,
        method: &Method,
        path: &str,
    ) -> Option<OperationFault> {
        let mut state = self.state.lock().unwrap();
        let index = state.faults.iter().position(|fault| {
            fault.source == source
                && fault.target == target
                && fault.method == *method
                && fault.path == path
        })?;
        state.faults.remove(index).map(|fault| fault.fault)
    }

    async fn dispatch(
        &self,
        source: &str,
        method: Method,
        url: Url,
        headers: BTreeMap<String, String>,
        mut body: Body,
    ) -> Result<Response<Body>, String> {
        let host = url.host_str().ok_or("request URL has no host")?;
        let target = self
            .target_for_host(host)
            .ok_or_else(|| format!("tailnet DNS has no node {host}"))?;
        let path = url.path().to_owned();
        let partitioned = self
            .state
            .lock()
            .unwrap()
            .partitions
            .contains(&(source.to_owned(), target.clone()));
        if partitioned {
            return Err(format!("directional partition {source}->{target}"));
        }
        let fault = self.take_fault(source, &target, &method, &path);
        match &fault {
            Some(OperationFault::FailBeforeDispatch(message)) => return Err((*message).into()),
            Some(OperationFault::HoldBeforeDispatch(gate)) => {
                gate.is_entered.store(true, Ordering::Release);
                gate.entered.notify_waiters();
                while !gate.is_released.load(Ordering::Acquire) {
                    gate.release.notified().await;
                }
            }
            _ => {}
        }

        if let Some(OperationFault::TruncateRequestBody(limit)) = fault.as_ref() {
            let limit = *limit;
            body = Body::from_stream(body.into_data_stream().scan(0usize, move |seen, item| {
                let output = item.ok().and_then(|chunk| {
                    let remaining = limit.saturating_sub(*seen);
                    *seen += chunk.len().min(remaining);
                    (remaining > 0)
                        .then(|| Ok::<_, std::io::Error>(chunk.slice(..chunk.len().min(remaining))))
                });
                std::future::ready(output)
            }));
        } else if matches!(fault, Some(OperationFault::CorruptRequestBody)) {
            body = corrupt_body(body);
        }

        let target_state = self.node(&target);
        if !target_state.online {
            return Err(format!("target {target} is offline"));
        }
        if target_state.mapping != MappingState::OwnedByAudetic {
            return Err(format!("target {target} has no Audetic Serve mapping"));
        }
        let router = target_state
            .router
            .ok_or_else(|| format!("target {target} has no published Home Hub router"))?;
        let route = path
            .strip_prefix(ServeSpec::audetic().cli_mount_path())
            .ok_or("request missed the Audetic Serve mount")?;
        let uri = match url.query() {
            Some(query) => format!("{route}?{query}"),
            None => route.to_owned(),
        };
        let mut request = Request::builder().method(method.clone()).uri(uri);
        for (name, value) in headers {
            if name.eq_ignore_ascii_case(TAILSCALE_USER_LOGIN_HEADER) {
                continue;
            }
            request = request.header(name, value);
        }
        let identity = match &fault {
            Some(OperationFault::OverrideIdentity(identity)) => identity.clone(),
            _ => Some(self.node(source).owner_login),
        };
        if let Some(identity) = identity {
            request = request.header(TAILSCALE_USER_LOGIN_HEADER, identity);
        }
        if matches!(fault, Some(OperationFault::FunnelRequest)) {
            request = request.header(TAILSCALE_FUNNEL_REQUEST_HEADER, "true");
        }
        let mut request = request.body(body).map_err(|error| error.to_string())?;
        if let Some(OperationFault::OverrideProtocol(version)) = &fault {
            request.headers_mut().remove(PROTOCOL_VERSION_HEADER);
            if let Some(version) = version {
                request.headers_mut().insert(
                    PROTOCOL_VERSION_HEADER,
                    HeaderValue::from_str(version).map_err(|error| error.to_string())?,
                );
            }
        }
        self.state
            .lock()
            .unwrap()
            .dispatched
            .push((source.to_owned(), target, method, path));
        let response = router
            .oneshot(request)
            .await
            .map_err(|error| error.to_string())?;
        if let Some(OperationFault::LoseResponseAfterDispatch(message)) = fault {
            return Err(message.into());
        }
        Ok(match fault {
            Some(OperationFault::TruncateResponseBody(limit)) => {
                let (parts, body) = response.into_parts();
                let body =
                    Body::from_stream(body.into_data_stream().scan(0usize, move |seen, item| {
                        let output = item.ok().and_then(|chunk| {
                            let remaining = limit.saturating_sub(*seen);
                            *seen += chunk.len().min(remaining);
                            (remaining > 0).then(|| {
                                Ok::<_, std::io::Error>(chunk.slice(..chunk.len().min(remaining)))
                            })
                        });
                        std::future::ready(output)
                    }));
                Response::from_parts(parts, body)
            }
            Some(OperationFault::CorruptResponseBody) => {
                let (parts, body) = response.into_parts();
                Response::from_parts(parts, corrupt_body(body))
            }
            _ => response,
        })
    }
}

fn corrupt_body(body: Body) -> Body {
    Body::from_stream(body.into_data_stream().scan(false, |changed, item| {
        let output = item.map(|chunk| {
            if *changed || chunk.is_empty() {
                return chunk;
            }
            *changed = true;
            let mut bytes = chunk.to_vec();
            bytes[0] ^= 0xff;
            bytes::Bytes::from(bytes)
        });
        std::future::ready(Some(output))
    }))
}

#[derive(Clone)]
pub(super) struct TailnetTransport {
    tailnet: FakeTailnet,
    source: String,
}

impl TailnetTransport {
    async fn request(
        &self,
        method: Method,
        url: Url,
        headers: BTreeMap<String, String>,
        body: Body,
    ) -> Result<TransportResponse, String> {
        let response = self
            .tailnet
            .dispatch(&self.source, method, url, headers, body)
            .await?;
        let (parts, body) = response.into_parts();
        let bytes = http_body_util::BodyExt::collect(body)
            .await
            .map_err(|error| error.to_string())?
            .to_bytes();
        Ok(TransportResponse {
            status: parts.status.as_u16(),
            headers: parts.headers,
            body: bytes.to_vec(),
        })
    }
}

#[async_trait]
impl HubTransport for TailnetTransport {
    async fn get(
        &self,
        url: Url,
        headers: BTreeMap<String, String>,
    ) -> Result<TransportResponse, String> {
        self.request(Method::GET, url, headers, Body::empty()).await
    }

    async fn post(
        &self,
        url: Url,
        mut headers: BTreeMap<String, String>,
        body: Vec<u8>,
    ) -> Result<TransportResponse, String> {
        headers.insert("content-type".into(), "application/json".into());
        self.request(Method::POST, url, headers, Body::from(body))
            .await
    }

    async fn patch(
        &self,
        url: Url,
        mut headers: BTreeMap<String, String>,
        body: Vec<u8>,
    ) -> Result<TransportResponse, String> {
        headers.insert("content-type".into(), "application/json".into());
        self.request(Method::PATCH, url, headers, Body::from(body))
            .await
    }

    async fn delete(
        &self,
        url: Url,
        headers: BTreeMap<String, String>,
    ) -> Result<TransportResponse, String> {
        self.request(Method::DELETE, url, headers, Body::empty())
            .await
    }

    async fn head(
        &self,
        url: Url,
        headers: BTreeMap<String, String>,
    ) -> Result<TransportResponse, String> {
        self.request(Method::HEAD, url, headers, Body::empty())
            .await
    }

    async fn put_file(
        &self,
        url: Url,
        mut headers: BTreeMap<String, String>,
        path: &std::path::Path,
        byte_size: u64,
        media_type: &str,
    ) -> Result<TransportResponse, String> {
        headers.insert("content-length".into(), byte_size.to_string());
        headers.insert("content-type".into(), media_type.to_owned());
        let file = tokio::fs::File::open(path)
            .await
            .map_err(|error| error.to_string())?;
        let body = Body::from_stream(tokio_util::io::ReaderStream::new(file));
        self.request(Method::PUT, url, headers, body).await
    }

    async fn get_stream(
        &self,
        url: Url,
        headers: BTreeMap<String, String>,
    ) -> Result<StreamingTransportResponse, String> {
        let response = self
            .tailnet
            .dispatch(&self.source, Method::GET, url, headers, Body::empty())
            .await?;
        let (parts, body) = response.into_parts();
        let stream = body
            .into_data_stream()
            .map_err(|error| HubTransferError::Transport(error.to_string()));
        Ok(StreamingTransportResponse {
            status: parts.status.as_u16(),
            headers: parts.headers,
            body: Box::pin(stream),
        })
    }
}

struct FakeTailscale {
    tailnet: FakeTailnet,
    node: String,
}

impl TailscaleControl for FakeTailscale {
    fn status(&self) -> Result<TailscaleStatus, TailscaleError> {
        let own = self.tailnet.node(&self.node);
        if own.tagged {
            return Err(TailscaleError::TaggedDevice);
        }
        let peers = self
            .tailnet
            .state
            .lock()
            .unwrap()
            .nodes
            .iter()
            .filter(|(name, _)| *name != &self.node)
            .map(|(_, peer)| TailscalePeer {
                dns_name: peer.dns_name.clone(),
                online: peer.online,
                tagged: peer.tagged,
            })
            .collect();
        Ok(TailscaleStatus {
            version: Version::parse("1.80.0").unwrap(),
            backend_state: own.backend_state,
            self_dns_name: format!("{}.", own.dns_name),
            owner_login: own.owner_login,
            self_is_tagged: false,
            peers,
        })
    }

    fn serve_assessment(&self) -> Result<ServeAssessment, TailscaleError> {
        let state = self.tailnet.node(&self.node);
        Ok(ServeAssessment {
            mapping: state.mapping,
            funnel_enabled: state.funnel,
        })
    }

    fn apply_audetic_serve(&self) -> Result<bool, TailscaleError> {
        let state = self.tailnet.node(&self.node);
        if state.funnel {
            return Err(TailscaleError::FunnelEnabled);
        }
        if state.mapping == MappingState::Collision {
            return Err(TailscaleError::ServeCollision);
        }
        let created = state.mapping == MappingState::Vacant;
        self.tailnet
            .set_mapping(&self.node, MappingState::OwnedByAudetic);
        Ok(created)
    }

    fn remove_audetic_serve(&self) -> Result<bool, TailscaleError> {
        let owned = self.tailnet.node(&self.node).mapping == MappingState::OwnedByAudetic;
        if owned {
            self.tailnet.set_mapping(&self.node, MappingState::Vacant);
        }
        Ok(owned)
    }

    fn serve_preview(&self) -> String {
        "tailscale serve --bg --https=8443 --set-path=/audetic http://127.0.0.1:3738".into()
    }
}

struct TailnetHubLauncher {
    tailnet: FakeTailnet,
    node: String,
}

#[async_trait]
impl HubRuntimeLauncher for TailnetHubLauncher {
    async fn launch(
        &self,
        server: HubServer,
        _bind_address: SocketAddr,
        shutdown: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<HubListenerFuture, RuntimeError> {
        let router = server.router();
        self.tailnet.node_mut(&self.node, |state| {
            assert!(state.router.is_none(), "only one router may be published");
            state.router = Some(router);
        });
        let tailnet = self.tailnet.clone();
        let node = self.node.clone();
        Ok(Box::pin(async move {
            let _ = shutdown.await;
            tailnet.node_mut(&node, |state| state.router = None);
            Ok(())
        }))
    }
}
