use audetic_core::sync::HubId;
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{header, header::ORIGIN, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use thiserror::Error;
use tokio::net::TcpListener;
use utoipa::OpenApi;

use std::future::Future;
use std::io;
use std::sync::Arc;
use tower::ServiceExt;
use tower_http::services::ServeFile;

use super::identity::{parse_stored_tailscale_login, parse_tailscale_login, LoginParseError};
use super::library::HubLibrary;
use super::protocol::{
    is_canonical_sha256, ChangeCursor, ChangePage, ChangeTarget, DictationPage, HubApiError,
    HubInfo, MeetingPage, MeetingTitlePatch, ProtocolRange, RecordKind, SharedMeeting,
    SnapshotBatch, SnapshotBatchResponse, HUB_BLOBS_ROUTE, HUB_CHANGES_ROUTE, HUB_DICTATIONS_ROUTE,
    HUB_ID_HEADER, HUB_INFO_ROUTE, HUB_MEETINGS_ROUTE, HUB_SNAPSHOTS_ROUTE, MAX_BLOB_BYTES,
    MAX_DICTATION_PAGE, MAX_MEETING_PAGE, PROTOCOL_VERSION, PROTOCOL_VERSION_HEADER,
    TAILSCALE_FUNNEL_REQUEST_HEADER,
};

#[derive(Clone, Debug)]
pub struct HubServerConfig {
    pub hub_id: HubId,
    pub owner_login: String,
    pub device_name: Option<String>,
    pub audetic_version: String,
    pub library: Option<HubLibrary>,
}

impl HubServerConfig {
    pub fn new(hub_id: HubId, owner_login: &str) -> Result<Self, LoginParseError> {
        Ok(Self {
            hub_id,
            owner_login: parse_stored_tailscale_login(owner_login)?,
            device_name: None,
            audetic_version: env!("CARGO_PKG_VERSION").to_owned(),
            library: None,
        })
    }

    pub fn with_device_name(mut self, device_name: impl Into<String>) -> Self {
        self.device_name = Some(device_name.into());
        self
    }

    pub fn with_library(mut self, library: HubLibrary) -> Self {
        self.library = Some(library);
        self
    }
}

#[derive(Debug, Error)]
pub enum HubServerError {
    #[error("the Home Hub listener must bind to a loopback address, not {0}")]
    NonLoopback(std::net::SocketAddr),
    #[error("Home Hub listener failed: {0}")]
    Io(#[from] io::Error),
}

#[derive(Clone)]
pub struct HubServer {
    state: Arc<HubServerConfig>,
}

impl HubServer {
    pub fn new(config: HubServerConfig) -> Self {
        Self {
            state: Arc::new(config),
        }
    }

    pub fn router(&self) -> Router {
        Router::new()
            .route(HUB_INFO_ROUTE, get(info))
            .route(HUB_SNAPSHOTS_ROUTE, post(apply_snapshots))
            .route(HUB_BLOBS_ROUTE, put(upload_blob).head(head_blob))
            .route(HUB_CHANGES_ROUTE, get(page_changes))
            .route(HUB_DICTATIONS_ROUTE, get(page_dictations))
            .route(HUB_MEETINGS_ROUTE, get(page_meetings))
            .route(
                "/v1/meetings/:sync_id",
                get(get_meeting)
                    .patch(update_meeting_title)
                    .delete(delete_meeting),
            )
            .route("/v1/dictations/:sync_id", delete(delete_dictation))
            .route(
                "/v1/dictations/:sync_id/payload",
                get(get_dictation_payload),
            )
            .route("/v1/meetings/:sync_id/payload", get(get_meeting_payload))
            .route("/v1/artifacts/:sync_id", delete(delete_artifact))
            .layer(DefaultBodyLimit::max(1024 * 1024))
            .layer(middleware::from_fn_with_state(
                self.state.clone(),
                enforce_hub_policy,
            ))
            .with_state(self.state.clone())
    }

    pub async fn serve(self, listener: TcpListener) -> Result<(), HubServerError> {
        self.serve_with_shutdown(listener, std::future::pending::<()>())
            .await
    }

    pub async fn serve_with_shutdown(
        self,
        listener: TcpListener,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> Result<(), HubServerError> {
        let address = listener.local_addr()?;
        if !address.ip().is_loopback() {
            return Err(HubServerError::NonLoopback(address));
        }
        axum::serve(listener, self.router())
            .with_graceful_shutdown(shutdown)
            .await?;
        Ok(())
    }
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
struct ChangePageQuery {
    /// Last committed cursor already applied by the caller. Use zero to begin.
    after: Option<ChangeCursor>,
    /// Immutable target returned by the first page of this traversal.
    target: Option<ChangeTarget>,
    /// Maximum changes to return (default 100, maximum 250).
    limit: Option<usize>,
}

#[utoipa::path(
    get,
    path = "/v1/changes",
    tag = "hub_cache",
    params(ChangePageQuery),
    responses(
        (status = 200, description = "Stable bounded page of self-contained authoritative changes", body = ChangePage),
        (status = 400, description = "Missing, malformed, nonadvancing, or unbounded cursor request", body = HubApiError),
        (status = 403, description = "Untrusted caller", body = HubApiError),
        (status = 409, description = "Wrong expected Hub ID", body = HubApiError),
    )
)]
async fn page_changes(
    State(state): State<Arc<HubServerConfig>>,
    Query(query): Query<ChangePageQuery>,
) -> Response {
    let Some(library) = &state.library else {
        return library_unavailable();
    };
    let Some(after) = query.after else {
        return hub_error(
            StatusCode::BAD_REQUEST,
            "change_cursor_required",
            "the after cursor is required; use zero to begin",
        );
    };
    match library.page_changes(after, query.target, query.limit.unwrap_or(100)) {
        Ok(page) => Json(page).into_response(),
        Err(error) => hub_error(
            StatusCode::BAD_REQUEST,
            "invalid_change_page",
            &error.to_string(),
        ),
    }
}

#[utoipa::path(
    get,
    path = "/v1/info",
    tag = "hub_discovery",
    params(
        ("Tailscale-User-Login" = String, Header, description = "Exact identity injected by Tailscale Serve"),
        ("X-Audetic-Protocol-Version" = u16, Header, description = "Required sync protocol version"),
        ("X-Audetic-Hub-ID" = Option<String>, Header, description = "Expected stable Hub ID when reconnecting"),
    ),
    responses(
        (status = 200, description = "Compatible Home Hub identity and protocol range", body = HubInfo),
        (status = 400, description = "Missing or malformed protocol/Hub ID header", body = HubApiError),
        (status = 403, description = "Untrusted origin, Funnel request, or wrong Tailscale identity", body = HubApiError),
        (status = 409, description = "The expected Hub ID does not match this Home Hub", body = HubApiError),
        (status = 426, description = "Unsupported sync protocol", body = HubApiError),
    )
)]
async fn info(State(state): State<Arc<HubServerConfig>>) -> Json<HubInfo> {
    Json(HubInfo {
        hub_id: state.hub_id,
        owner_login: state.owner_login.clone(),
        device_name: state.device_name.clone(),
        protocol: ProtocolRange::supported(),
        audetic_version: state.audetic_version.clone(),
    })
}

#[utoipa::path(
    post,
    path = "/v1/snapshots",
    tag = "hub_dictations",
    request_body = SnapshotBatch,
    responses(
        (status = 200, description = "Per-snapshot idempotent acceptance results", body = SnapshotBatchResponse),
        (status = 400, description = "Malformed or unbounded batch", body = HubApiError),
        (status = 403, description = "Untrusted caller", body = HubApiError),
        (status = 409, description = "Wrong expected Hub ID", body = HubApiError),
    )
)]
async fn apply_snapshots(
    State(state): State<Arc<HubServerConfig>>,
    Json(batch): Json<SnapshotBatch>,
) -> Response {
    let Some(library) = &state.library else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(HubApiError::new(
                "library_unavailable",
                "Shared Library is not active",
            )),
        )
            .into_response();
    };
    match library.apply_snapshots(batch.snapshots) {
        Ok(response) => Json(response).into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(HubApiError::new("invalid_batch", error.to_string())),
        )
            .into_response(),
    }
}

#[utoipa::path(
    put,
    path = "/v1/blobs/{sha256}",
    tag = "hub_payloads",
    params(("sha256" = String, Path, description = "Lowercase SHA-256 checksum")),
    responses(
        (status = 201, description = "Blob verified and atomically stored"),
        (status = 204, description = "Identical blob was already stored"),
        (status = 400, description = "Invalid checksum, media type, or body size", body = HubApiError),
        (status = 409, description = "No accepted record references this checksum", body = HubApiError),
        (status = 413, description = "Blob exceeds the bounded upload limit", body = HubApiError),
    )
)]
async fn upload_blob(
    State(state): State<Arc<HubServerConfig>>,
    Path(sha256): Path<String>,
    request: Request,
) -> Response {
    let Some(library) = &state.library else {
        return library_unavailable();
    };
    if !is_canonical_sha256(&sha256) {
        return hub_error(
            StatusCode::BAD_REQUEST,
            "invalid_checksum",
            "checksum is not canonical SHA-256",
        );
    }
    let Some(byte_size) = exactly_one_header(request.headers(), header::CONTENT_LENGTH)
        .and_then(|value| value.parse::<u64>().ok())
    else {
        return hub_error(
            StatusCode::BAD_REQUEST,
            "invalid_size",
            "one valid Content-Length header is required",
        );
    };
    if byte_size == 0 || byte_size > MAX_BLOB_BYTES {
        return hub_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "Recording Payload size is outside the supported range",
        );
    }
    let Some(media_type) = exactly_one_header(request.headers(), header::CONTENT_TYPE) else {
        return hub_error(
            StatusCode::BAD_REQUEST,
            "invalid_media_type",
            "one valid Content-Type header is required",
        );
    };
    let existed = match library.has_blob(&sha256) {
        Ok(value) => value,
        Err(error) => {
            return hub_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "storage_error",
                &error.to_string(),
            )
        }
    };
    let stream = http_body_util::BodyExt::into_data_stream(request.into_body());
    match library
        .accept_blob_stream(&sha256, byte_size, &media_type, stream)
        .await
    {
        Ok(_) if existed => StatusCode::NO_CONTENT.into_response(),
        Ok(_) => StatusCode::CREATED.into_response(),
        Err(error) if error.to_string().contains("no accepted record") => hub_error(
            StatusCode::CONFLICT,
            "association_required",
            &error.to_string(),
        ),
        Err(error) if error.to_string().contains("exceeds") => hub_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            &error.to_string(),
        ),
        Err(error) => {
            let message = error.to_string();
            if message.contains("blob verification failed") {
                hub_error(
                    StatusCode::BAD_REQUEST,
                    "blob_verification_failed",
                    &message,
                )
            } else {
                hub_error(StatusCode::INTERNAL_SERVER_ERROR, "storage_error", &message)
            }
        }
    }
}

#[utoipa::path(
    head,
    path = "/v1/blobs/{sha256}",
    tag = "hub_payloads",
    params(("sha256" = String, Path)),
    responses((status = 200, description = "Verified blob is present"), (status = 404, description = "Blob is absent"))
)]
async fn head_blob(
    State(state): State<Arc<HubServerConfig>>,
    Path(sha256): Path<String>,
) -> Response {
    let Some(library) = &state.library else {
        return library_unavailable();
    };
    match library.has_blob(&sha256) {
        Ok(true) => StatusCode::OK.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => hub_error(
            StatusCode::BAD_REQUEST,
            "invalid_checksum",
            &error.to_string(),
        ),
    }
}

#[utoipa::path(get,path="/v1/dictations/{sync_id}/payload",tag="hub_payloads",params(("sync_id"=String,Path)),responses((status=200,description="Associated Recording Payload bytes"),(status=206,description="Associated Recording Payload range"),(status=404,body=HubApiError)))]
async fn get_dictation_payload(
    State(state): State<Arc<HubServerConfig>>,
    Path(id): Path<String>,
    request: Request,
) -> Response {
    get_payload(state, id, RecordKind::Dictation, request).await
}

#[utoipa::path(get,path="/v1/meetings/{sync_id}/payload",tag="hub_payloads",params(("sync_id"=String,Path)),responses((status=200,description="Associated Recording Payload bytes"),(status=206,description="Associated Recording Payload range"),(status=404,body=HubApiError)))]
async fn get_meeting_payload(
    State(state): State<Arc<HubServerConfig>>,
    Path(id): Path<String>,
    request: Request,
) -> Response {
    get_payload(state, id, RecordKind::Meeting, request).await
}

async fn get_payload(
    state: Arc<HubServerConfig>,
    id: String,
    kind: RecordKind,
    request: Request,
) -> Response {
    let Some(library) = &state.library else {
        return library_unavailable();
    };
    let Ok(record_id) = id.parse() else {
        return hub_error(
            StatusCode::BAD_REQUEST,
            "invalid_record_id",
            "record UUID is malformed",
        );
    };
    let payload = match library.payload(record_id, kind) {
        Ok(Some(payload)) => payload,
        Ok(None) => {
            return hub_error(
                StatusCode::NOT_FOUND,
                "payload_not_found",
                "record has no available Recording Payload",
            )
        }
        Err(error) => {
            return hub_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "storage_error",
                &error.to_string(),
            )
        }
    };
    match ServeFile::new(payload.canonical_path)
        .oneshot(request)
        .await
    {
        Ok(mut response) => {
            if let Ok(value) = HeaderValue::from_str(&payload.media_type) {
                response.headers_mut().insert(header::CONTENT_TYPE, value);
            }
            response.into_response()
        }
        Err(error) => hub_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "storage_error",
            &error.to_string(),
        ),
    }
}

fn exactly_one_header(headers: &HeaderMap, name: header::HeaderName) -> Option<String> {
    let values = headers.get_all(name).iter().collect::<Vec<_>>();
    let [value] = values.as_slice() else {
        return None;
    };
    value.to_str().ok().map(str::to_owned)
}

fn library_unavailable() -> Response {
    hub_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "library_unavailable",
        "Shared Library is not active",
    )
}

fn hub_error(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(HubApiError::new(code, message))).into_response()
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
struct DictationPageQuery {
    q: Option<String>,
    from: Option<String>,
    to: Option<String>,
    cursor: Option<String>,
    limit: Option<usize>,
}

#[utoipa::path(
    get,
    path = "/v1/dictations",
    tag = "hub_dictations",
    params(DictationPageQuery),
    responses(
        (status = 200, description = "Canonical page of visible authoritative dictations", body = DictationPage),
        (status = 400, description = "Invalid cursor", body = HubApiError),
        (status = 403, description = "Untrusted caller", body = HubApiError),
        (status = 409, description = "Wrong expected Hub ID", body = HubApiError),
    )
)]
async fn page_dictations(
    State(state): State<Arc<HubServerConfig>>,
    Query(query): Query<DictationPageQuery>,
) -> Response {
    let Some(library) = &state.library else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(HubApiError::new(
                "library_unavailable",
                "Shared Library is not active",
            )),
        )
            .into_response();
    };
    match library.page_dictations(
        query.q.as_deref(),
        query.from.as_deref(),
        query.to.as_deref(),
        query.cursor.as_deref(),
        query.limit.unwrap_or(50).min(MAX_DICTATION_PAGE),
    ) {
        Ok(page) => Json(page).into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(HubApiError::new("invalid_page", error.to_string())),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
struct MeetingPageQuery {
    q: Option<String>,
    cursor: Option<String>,
    /// Maximum meetings to return (default 50, maximum 100).
    limit: Option<usize>,
}

#[utoipa::path(get,path="/v1/meetings",tag="hub_meetings",params(MeetingPageQuery),responses((status=200,body=MeetingPage),(status=400,body=HubApiError)))]
async fn page_meetings(
    State(state): State<Arc<HubServerConfig>>,
    Query(query): Query<MeetingPageQuery>,
) -> Response {
    let Some(library) = &state.library else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(HubApiError::new(
                "library_unavailable",
                "Shared Library is not active",
            )),
        )
            .into_response();
    };
    match library.page_meetings(
        query.q.as_deref(),
        query.cursor.as_deref(),
        query.limit.unwrap_or(50).min(MAX_MEETING_PAGE),
    ) {
        Ok(page) => Json(page).into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(HubApiError::new("invalid_page", error.to_string())),
        )
            .into_response(),
    }
}

#[utoipa::path(get,path="/v1/meetings/{sync_id}",tag="hub_meetings",params(("sync_id"=String,Path)),responses((status=200,body=SharedMeeting),(status=404,body=HubApiError)))]
async fn get_meeting(
    State(state): State<Arc<HubServerConfig>>,
    Path(sync_id): Path<String>,
) -> Response {
    let Some(library) = &state.library else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(HubApiError::new(
                "library_unavailable",
                "Shared Library is not active",
            )),
        )
            .into_response();
    };
    let Ok(id) = sync_id.parse() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(HubApiError::new(
                "invalid_record_id",
                "record UUID is malformed",
            )),
        )
            .into_response();
    };
    match library.meeting(id) {
        Ok(Some(value)) => Json(value).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(HubApiError::new("not_found", "meeting not found")),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(HubApiError::new("storage_error", error.to_string())),
        )
            .into_response(),
    }
}

#[utoipa::path(patch,path="/v1/meetings/{sync_id}",tag="hub_meetings",params(("sync_id"=String,Path)),request_body=MeetingTitlePatch,responses((status=200,body=SharedMeeting),(status=409,body=HubApiError)))]
async fn update_meeting_title(
    State(state): State<Arc<HubServerConfig>>,
    Path(sync_id): Path<String>,
    Json(patch): Json<MeetingTitlePatch>,
) -> Response {
    let Some(library) = &state.library else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(HubApiError::new(
                "library_unavailable",
                "Shared Library is not active",
            )),
        )
            .into_response();
    };
    let Ok(id) = sync_id.parse() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(HubApiError::new(
                "invalid_record_id",
                "record UUID is malformed",
            )),
        )
            .into_response();
    };
    match library.update_meeting_title(id, &patch) {
        Ok(Some(value)) => Json(value.meeting).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(HubApiError::new("not_found", "meeting not found")),
        )
            .into_response(),
        Err(crate::db::shared_library::MeetingTitleUpdateError::Conflict) => (
            StatusCode::CONFLICT,
            Json(HubApiError::new(
                "title_version_conflict",
                "meeting title changed; reload and retry",
            )),
        )
            .into_response(),
        Err(
            crate::db::shared_library::MeetingTitleUpdateError::InvalidTitle
            | crate::db::shared_library::MeetingTitleUpdateError::InvalidSource,
        ) => (
            StatusCode::BAD_REQUEST,
            Json(HubApiError::new(
                "invalid_title",
                "Meeting Title is invalid",
            )),
        )
            .into_response(),
        Err(error) => {
            tracing::error!(%error, "authoritative Meeting Title update failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(HubApiError::new(
                    "library_error",
                    "Shared Library operation failed",
                )),
            )
                .into_response()
        }
    }
}

async fn delete_kind(state: Arc<HubServerConfig>, sync_id: String, kind: RecordKind) -> Response {
    let Some(library) = &state.library else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(HubApiError::new(
                "library_unavailable",
                "Shared Library is not active",
            )),
        )
            .into_response();
    };
    let Ok(id) = sync_id.parse() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(HubApiError::new(
                "invalid_record_id",
                "record UUID is malformed",
            )),
        )
            .into_response();
    };
    match library.delete(id, kind) {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(crate::db::shared_library::ApplySnapshotError::KindChanged) => (
            StatusCode::CONFLICT,
            Json(HubApiError::new(
                "kind_changed",
                "record UUID is reserved for another kind",
            )),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(HubApiError::new("storage_error", error.to_string())),
        )
            .into_response(),
    }
}
#[utoipa::path(delete,path="/v1/dictations/{sync_id}",tag="hub_dictations",params(("sync_id"=String,Path)),responses((status=204),(status=409,body=HubApiError)))]
async fn delete_dictation(
    State(state): State<Arc<HubServerConfig>>,
    Path(id): Path<String>,
) -> Response {
    delete_kind(state, id, RecordKind::Dictation).await
}
#[utoipa::path(delete,path="/v1/meetings/{sync_id}",tag="hub_meetings",params(("sync_id"=String,Path)),responses((status=204),(status=409,body=HubApiError)))]
async fn delete_meeting(
    State(state): State<Arc<HubServerConfig>>,
    Path(id): Path<String>,
) -> Response {
    delete_kind(state, id, RecordKind::Meeting).await
}
#[utoipa::path(delete,path="/v1/artifacts/{sync_id}",tag="hub_artifacts",params(("sync_id"=String,Path)),responses((status=204),(status=409,body=HubApiError)))]
async fn delete_artifact(
    State(state): State<Arc<HubServerConfig>>,
    Path(id): Path<String>,
) -> Response {
    delete_kind(state, id, RecordKind::Artifact).await
}

async fn enforce_hub_policy(
    State(state): State<Arc<HubServerConfig>>,
    request: Request,
    next: Next,
) -> Response {
    let headers = request.headers();
    if headers.contains_key(ORIGIN) {
        return rejection(
            &state,
            StatusCode::FORBIDDEN,
            "browser_origin_rejected",
            "browser Origin requests are not accepted by the Home Hub listener",
        );
    }
    if headers.contains_key(TAILSCALE_FUNNEL_REQUEST_HEADER) {
        return rejection(
            &state,
            StatusCode::FORBIDDEN,
            "funnel_rejected",
            "Tailscale Funnel traffic is not accepted",
        );
    }

    let identities = headers
        .get_all(super::identity::TAILSCALE_USER_LOGIN_HEADER)
        .iter()
        .collect::<Vec<_>>();
    let [identity] = identities.as_slice() else {
        return rejection(
            &state,
            StatusCode::FORBIDDEN,
            "identity_required",
            "Tailscale-User-Login is required",
        );
    };
    let Ok(identity) = parse_tailscale_login(identity) else {
        return rejection(
            &state,
            StatusCode::FORBIDDEN,
            "invalid_identity",
            "Tailscale-User-Login is malformed",
        );
    };
    if identity != state.owner_login {
        return rejection(
            &state,
            StatusCode::FORBIDDEN,
            "wrong_identity",
            "the authenticated Tailscale identity does not own this Home Hub",
        );
    }

    let Some(version) = parse_protocol_version(headers) else {
        return rejection(
            &state,
            StatusCode::BAD_REQUEST,
            "protocol_required",
            "a valid Audetic protocol version header is required",
        );
    };
    if version != PROTOCOL_VERSION {
        return rejection(
            &state,
            StatusCode::UPGRADE_REQUIRED,
            "incompatible_protocol",
            "the requested Audetic sync protocol is not supported",
        );
    }

    let expected_hub_ids = headers.get_all(HUB_ID_HEADER).iter().collect::<Vec<_>>();
    if expected_hub_ids.is_empty() && request.uri().path() != HUB_INFO_ROUTE {
        return rejection(
            &state,
            StatusCode::BAD_REQUEST,
            "hub_id_required",
            "the expected Hub ID is required for non-discovery requests",
        );
    }
    if expected_hub_ids.len() > 1 {
        return rejection(
            &state,
            StatusCode::BAD_REQUEST,
            "invalid_hub_id",
            "exactly one expected Hub ID may be supplied",
        );
    }
    if let Some(expected_hub_id) = expected_hub_ids.first() {
        let Some(expected_hub_id) = expected_hub_id
            .to_str()
            .ok()
            .and_then(|id| id.parse::<HubId>().ok())
        else {
            return rejection(
                &state,
                StatusCode::BAD_REQUEST,
                "invalid_hub_id",
                "the expected Hub ID is malformed",
            );
        };
        if expected_hub_id != state.hub_id {
            return rejection(
                &state,
                StatusCode::CONFLICT,
                "wrong_hub_id",
                "the expected Hub ID does not match this Home Hub",
            );
        }
    }

    let mut response = next.run(request).await;
    insert_hub_id(response.headers_mut(), state.hub_id);
    response
}

fn parse_protocol_version(headers: &HeaderMap) -> Option<u16> {
    let versions = headers
        .get_all(PROTOCOL_VERSION_HEADER)
        .iter()
        .collect::<Vec<_>>();
    let [version] = versions.as_slice() else {
        return None;
    };
    version.to_str().ok()?.parse().ok()
}

fn rejection(
    state: &HubServerConfig,
    status: StatusCode,
    code: &'static str,
    message: &'static str,
) -> Response {
    let mut response = (status, Json(HubApiError::new(code, message))).into_response();
    insert_hub_id(response.headers_mut(), state.hub_id);
    response
}

fn insert_hub_id(headers: &mut HeaderMap, hub_id: HubId) {
    headers.insert(
        HUB_ID_HEADER,
        HeaderValue::from_str(&hub_id.to_string()).expect("Hub IDs are valid header values"),
    );
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Audetic Home Hub API",
        description = "Narrow daemon-to-daemon Shared Library protocol. Not a browser API.",
        version = env!("CARGO_PKG_VERSION"),
        license(name = "MIT"),
    ),
    servers(
        (url = "http://127.0.0.1:3738", description = "Loopback listener behind Tailscale Serve"),
    ),
    paths(info, apply_snapshots, upload_blob, head_blob, page_changes, page_dictations, page_meetings, get_meeting, update_meeting_title, delete_dictation, delete_meeting, delete_artifact, get_dictation_payload, get_meeting_payload),
    components(schemas(
        HubInfo, ProtocolRange, HubApiError, audetic_core::sync::HubId,
        audetic_core::sync::RecordId, audetic_core::sync::DeviceId,
        super::protocol::RecordKind, super::protocol::DictationPayload,
        super::protocol::DictationSnapshot, super::protocol::SnapshotBatch,
        super::protocol::SnapshotDisposition, super::protocol::SnapshotResult,
        super::protocol::SnapshotBatchResponse, super::protocol::SharedDictation,
        super::protocol::DictationPage, super::protocol::ChangeOperation,
        super::protocol::ChangeEnvelope, super::protocol::ChangeCursor,
        super::protocol::ChangeTarget, super::protocol::ChangeRecord,
        super::protocol::ChangePage
        ,super::protocol::MeetingPayload, super::protocol::MeetingSnapshot,
        super::protocol::CompletedArtifactPayload, super::protocol::CompletedArtifactSnapshot,
        super::protocol::Snapshot, super::protocol::SharedMeeting, super::protocol::SharedArtifact,
         super::protocol::MeetingPage, super::protocol::MeetingTitlePatch,
         super::protocol::RecordingPayloadDescriptor, audetic_core::sync::PayloadAvailability
    )),
    tags(
        (name = "hub_discovery", description = "Home Hub discovery and compatibility"),
        (name = "hub_dictations", description = "Authoritative dictation text transfer and reads")
        ,(name = "hub_payloads", description = "Checksum-addressed Recording Payload transfer and associated playback")
        ,(name = "hub_cache", description = "Replay-safe Shared Library cache change feed")
    ),
)]
pub struct HubApiDoc;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::protocol::{DictationPayload, DictationSnapshot, RecordKind, SnapshotBatch};
    use axum::body::{to_bytes, Body};
    use axum::http::{Method, Request};
    use sha2::{Digest, Sha256};
    use tower::ServiceExt;
    use uuid::Uuid;

    fn hub_id() -> HubId {
        HubId::from_uuid(Uuid::new_v4())
    }

    fn server() -> (HubServer, HubId) {
        let hub_id = hub_id();
        (
            HubServer::new(HubServerConfig::new(hub_id, "Alice@Example.com").unwrap()),
            hub_id,
        )
    }

    fn request(owner: &str) -> axum::http::request::Builder {
        Request::builder()
            .method(Method::GET)
            .uri("/v1/info")
            .header(super::super::identity::TAILSCALE_USER_LOGIN_HEADER, owner)
            .header(PROTOCOL_VERSION_HEADER, PROTOCOL_VERSION.to_string())
    }

    #[tokio::test]
    async fn info_returns_identity_and_echoes_the_actual_hub_id() {
        let (server, hub_id) = server();
        let response = server
            .router()
            .oneshot(request("Alice@Example.com").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[HUB_ID_HEADER], hub_id.to_string());
        let info: HubInfo =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(info.hub_id, hub_id);
        assert_eq!(info.owner_login, "Alice@Example.com");
    }

    #[tokio::test]
    async fn identity_is_required_and_compared_without_case_or_whitespace_normalization() {
        let (server, _) = server();
        for owner in [None, Some("alice@example.com"), Some(" Alice@Example.com ")] {
            let mut builder = Request::builder().method(Method::GET).uri("/v1/info");
            if let Some(owner) = owner {
                builder =
                    builder.header(super::super::identity::TAILSCALE_USER_LOGIN_HEADER, owner);
            }
            let response = server
                .router()
                .oneshot(
                    builder
                        .header(PROTOCOL_VERSION_HEADER, "1")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{owner:?}");
        }
    }

    #[tokio::test]
    async fn rfc_2047_identity_uses_the_same_exact_decoder_as_setup() {
        let hub_id = hub_id();
        let server = HubServer::new(
            HubServerConfig::new(hub_id, "=?utf-8?q?m=C3=A1t@example.com?=").unwrap(),
        );
        let response = server
            .router()
            .oneshot(
                request("=?utf-8?q?m=C3=A1t@example.com?=")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn malformed_or_duplicated_identity_headers_fail_closed() {
        let (server, _) = server();
        let malformed = request("=?utf-8?q?broken=Q0?=")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            server
                .router()
                .clone()
                .oneshot(malformed)
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );

        let mut duplicated = request("Alice@Example.com").body(Body::empty()).unwrap();
        duplicated.headers_mut().append(
            super::super::identity::TAILSCALE_USER_LOGIN_HEADER,
            HeaderValue::from_static("Alice@Example.com"),
        );
        assert_eq!(
            server.router().oneshot(duplicated).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn browser_origin_and_funnel_marked_requests_are_rejected() {
        let (server, _) = server();
        for (header, value) in [
            (ORIGIN.as_str(), "https://example.com"),
            (TAILSCALE_FUNNEL_REQUEST_HEADER, "true"),
        ] {
            let response = server
                .router()
                .oneshot(
                    request("Alice@Example.com")
                        .header(header, value)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{header}");
        }
    }

    #[tokio::test]
    async fn protocol_is_required_and_incompatible_versions_are_rejected() {
        let (server, _) = server();
        let missing = Request::builder()
            .method(Method::GET)
            .uri("/v1/info")
            .header(
                super::super::identity::TAILSCALE_USER_LOGIN_HEADER,
                "Alice@Example.com",
            )
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            server
                .router()
                .clone()
                .oneshot(missing)
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );

        let mut incompatible = request("Alice@Example.com").body(Body::empty()).unwrap();
        incompatible
            .headers_mut()
            .insert(PROTOCOL_VERSION_HEADER, HeaderValue::from_static("2"));
        assert_eq!(
            server
                .router()
                .oneshot(incompatible)
                .await
                .unwrap()
                .status(),
            StatusCode::UPGRADE_REQUIRED
        );
    }

    #[tokio::test]
    async fn supplied_hub_id_must_match_before_the_request_reaches_the_handler() {
        let (server, actual) = server();
        let response = server
            .router()
            .oneshot(
                request("Alice@Example.com")
                    .header(HUB_ID_HEADER, hub_id().to_string())
                    .body(Body::from("body that must not be consumed"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(response.headers()[HUB_ID_HEADER], actual.to_string());
    }

    #[tokio::test]
    async fn snapshot_route_is_bounded_idempotent_and_policy_runs_before_body_application() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("hub.db");
        crate::db::migrate_db_at(&db_path).unwrap();
        let actual_hub_id = hub_id();
        let server = HubServer::new(
            HubServerConfig::new(actual_hub_id, "Alice@Example.com")
                .unwrap()
                .with_library(HubLibrary::new(db_path.clone())),
        );
        let snapshot = DictationSnapshot {
            kind: RecordKind::Dictation,
            schema_version: 1,
            record_id: audetic_core::sync::RecordId::new(),
            origin_device_id: audetic_core::sync::DeviceId::new(),
            local_version: 1,
            created_at: "2026-09-04T10:00:00Z".into(),
            updated_at: "2026-09-04T10:00:00Z".into(),
            payload: DictationPayload {
                text: "from another device".into(),
                recording_payload: Default::default(),
            },
        };
        let body = serde_json::to_vec(&SnapshotBatch {
            snapshots: vec![snapshot.into()],
        })
        .unwrap();
        let make_request = |expected: HubId, body: Vec<u8>| {
            Request::post(HUB_SNAPSHOTS_ROUTE)
                .header(
                    super::super::identity::TAILSCALE_USER_LOGIN_HEADER,
                    "Alice@Example.com",
                )
                .header(PROTOCOL_VERSION_HEADER, PROTOCOL_VERSION.to_string())
                .header(HUB_ID_HEADER, expected.to_string())
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap()
        };

        let rejected = server
            .router()
            .clone()
            .oneshot(make_request(hub_id(), b"not json".to_vec()))
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::CONFLICT);
        let conn = crate::db::open_db_at(&db_path).unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM shared_dictations", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );

        for _ in 0..2 {
            let accepted = server
                .router()
                .clone()
                .oneshot(make_request(actual_hub_id, body.clone()))
                .await
                .unwrap();
            assert_eq!(accepted.status(), StatusCode::OK);
        }
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM shared_dictations", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM shared_library_changes", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn changes_route_is_authenticated_and_returns_a_stable_typed_page() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("hub.db");
        crate::db::migrate_db_at(&db_path).unwrap();
        let library = HubLibrary::new(db_path);
        let actual_hub_id = hub_id();
        let record_id = audetic_core::sync::RecordId::new();
        library
            .apply_snapshots(vec![DictationSnapshot {
                kind: RecordKind::Dictation,
                schema_version: 1,
                record_id,
                origin_device_id: audetic_core::sync::DeviceId::new(),
                local_version: 1,
                created_at: "2026-09-05T10:00:00Z".into(),
                updated_at: "2026-09-05T10:00:00Z".into(),
                payload: DictationPayload {
                    text: "cache me".into(),
                    recording_payload: Default::default(),
                },
            }])
            .unwrap();
        let server = HubServer::new(
            HubServerConfig::new(actual_hub_id, "Alice@Example.com")
                .unwrap()
                .with_library(library),
        );

        let response = server
            .router()
            .clone()
            .oneshot(payload_request(
                Method::GET,
                "/v1/changes?after=0&limit=1",
                actual_hub_id,
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[HUB_ID_HEADER], actual_hub_id.to_string());
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let page: ChangePage = serde_json::from_slice(&body).unwrap();
        assert!(page.complete);
        assert_eq!(page.target_cursor.cursor().value(), 1);
        assert_eq!(page.changes.len(), 1);
        assert_eq!(page.changes[0].record_id, record_id);
        assert!(page.changes[0].snapshot.is_some());

        let missing_cursor = server
            .router()
            .clone()
            .oneshot(payload_request(
                Method::GET,
                HUB_CHANGES_ROUTE,
                actual_hub_id,
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_eq!(missing_cursor.status(), StatusCode::BAD_REQUEST);

        let unauthenticated = Request::get("/v1/changes?after=0")
            .header(PROTOCOL_VERSION_HEADER, PROTOCOL_VERSION.to_string())
            .header(HUB_ID_HEADER, actual_hub_id.to_string())
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            server
                .router()
                .oneshot(unauthenticated)
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }

    fn payload_request(method: Method, uri: &str, hub_id: HubId, body: Body) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header(
                super::super::identity::TAILSCALE_USER_LOGIN_HEADER,
                "Alice@Example.com",
            )
            .header(PROTOCOL_VERSION_HEADER, PROTOCOL_VERSION.to_string())
            .header(HUB_ID_HEADER, hub_id.to_string())
            .body(body)
            .unwrap()
    }

    fn server_with_payload_association(
        temp: &tempfile::TempDir,
        bytes: &[u8],
    ) -> (HubServer, HubId, audetic_core::sync::RecordId, String) {
        let db_path = temp.path().join("hub.db");
        crate::db::migrate_db_at(&db_path).unwrap();
        let blob_root = temp.path().join("canonical-blobs");
        let library = HubLibrary::with_blob_root(db_path, blob_root);
        let hub_id = hub_id();
        let record_id = audetic_core::sync::RecordId::new();
        let checksum = format!("{:x}", Sha256::digest(bytes));
        library
            .apply_snapshots(vec![DictationSnapshot {
                kind: RecordKind::Dictation,
                schema_version: 1,
                record_id,
                origin_device_id: audetic_core::sync::DeviceId::new(),
                local_version: 1,
                created_at: "2026-09-04T10:00:00Z".into(),
                updated_at: "2026-09-04T10:00:00Z".into(),
                payload: DictationPayload {
                    text: "payload metadata".into(),
                    recording_payload: super::super::protocol::RecordingPayloadDescriptor::pending(
                        checksum.clone(),
                        bytes.len() as u64,
                        "audio/wav".into(),
                    ),
                },
            }])
            .unwrap();
        let server = HubServer::new(
            HubServerConfig::new(hub_id, "Alice@Example.com")
                .unwrap()
                .with_library(library),
        );
        (server, hub_id, record_id, checksum)
    }

    #[tokio::test]
    async fn blob_put_is_atomic_verified_idempotent_and_only_associated_records_can_download() {
        let temp = tempfile::tempdir().unwrap();
        let bytes = b"verified recording payload";
        let (server, hub_id, record_id, checksum) = server_with_payload_association(&temp, bytes);
        let uri = format!("/v1/blobs/{checksum}");
        for expected in [StatusCode::CREATED, StatusCode::NO_CONTENT] {
            let request = payload_request(Method::PUT, &uri, hub_id, Body::from(bytes.to_vec()));
            let (mut parts, body) = request.into_parts();
            parts.headers.insert(
                header::CONTENT_LENGTH,
                HeaderValue::from_str(&bytes.len().to_string()).unwrap(),
            );
            parts
                .headers
                .insert(header::CONTENT_TYPE, HeaderValue::from_static("audio/wav"));
            let response = server
                .router()
                .oneshot(Request::from_parts(parts, body))
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
        }
        let head = server
            .router()
            .oneshot(payload_request(Method::HEAD, &uri, hub_id, Body::empty()))
            .await
            .unwrap();
        assert_eq!(head.status(), StatusCode::OK);
        let payload = server
            .router()
            .oneshot(payload_request(
                Method::GET,
                &format!("/v1/dictations/{record_id}/payload"),
                hub_id,
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_eq!(payload.status(), StatusCode::OK);
        assert_eq!(payload.headers()[header::CONTENT_TYPE], "audio/wav");
        assert_eq!(
            to_bytes(payload.into_body(), usize::MAX).await.unwrap(),
            bytes.as_slice()
        );

        let available = server
            .router()
            .oneshot(payload_request(
                Method::GET,
                HUB_DICTATIONS_ROUTE,
                hub_id,
                Body::empty(),
            ))
            .await
            .unwrap();
        let page: super::super::protocol::DictationPage =
            serde_json::from_slice(&to_bytes(available.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(
            page.items[0].recording_payload.availability,
            audetic_core::sync::PayloadAvailability::Available
        );

        let ranged = server
            .router()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(format!("/v1/dictations/{record_id}/payload"))
                    .header(
                        super::super::identity::TAILSCALE_USER_LOGIN_HEADER,
                        "Alice@Example.com",
                    )
                    .header(PROTOCOL_VERSION_HEADER, PROTOCOL_VERSION.to_string())
                    .header(HUB_ID_HEADER, hub_id.to_string())
                    .header(header::RANGE, "bytes=2-7")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ranged.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            to_bytes(ranged.into_body(), usize::MAX).await.unwrap(),
            &bytes[2..=7]
        );

        let unauthenticated = server
            .router()
            .oneshot(
                Request::get(format!("/v1/dictations/{record_id}/payload"))
                    .header(PROTOCOL_VERSION_HEADER, PROTOCOL_VERSION.to_string())
                    .header(HUB_ID_HEADER, hub_id.to_string())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthenticated.status(), StatusCode::FORBIDDEN);

        let arbitrary = server
            .router()
            .oneshot(payload_request(
                Method::GET,
                &format!(
                    "/v1/dictations/{}/payload",
                    audetic_core::sync::RecordId::new()
                ),
                hub_id,
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_eq!(arbitrary.status(), StatusCode::NOT_FOUND);
        let canonical = temp
            .path()
            .join("canonical-blobs")
            .join(&checksum[..2])
            .join(&checksum);
        assert_eq!(std::fs::read(canonical).unwrap(), bytes);
    }

    #[tokio::test]
    async fn checksum_mismatch_and_size_limit_leave_no_canonical_file() {
        let temp = tempfile::tempdir().unwrap();
        let expected = b"expected";
        let (server, hub_id, _record_id, checksum) =
            server_with_payload_association(&temp, expected);
        let request = payload_request(
            Method::PUT,
            &format!("/v1/blobs/{checksum}"),
            hub_id,
            Body::from(b"mismatch".to_vec()),
        );
        let (mut parts, body) = request.into_parts();
        parts
            .headers
            .insert(header::CONTENT_LENGTH, HeaderValue::from_static("8"));
        parts
            .headers
            .insert(header::CONTENT_TYPE, HeaderValue::from_static("audio/wav"));
        let response = server
            .router()
            .clone()
            .oneshot(Request::from_parts(parts, body))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(!temp
            .path()
            .join("canonical-blobs")
            .join(&checksum[..2])
            .join(&checksum)
            .exists());

        let oversized = payload_request(
            Method::PUT,
            &format!("/v1/blobs/{checksum}"),
            hub_id,
            Body::empty(),
        );
        let (mut parts, body) = oversized.into_parts();
        parts.headers.insert(
            header::CONTENT_LENGTH,
            HeaderValue::from_str(&(MAX_BLOB_BYTES + 1).to_string()).unwrap(),
        );
        parts
            .headers
            .insert(header::CONTENT_TYPE, HeaderValue::from_static("audio/wav"));
        let response = server
            .router()
            .oneshot(Request::from_parts(parts, body))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn listener_refuses_non_loopback_bindings() {
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let (server, _) = server();

        assert!(matches!(
            server.serve(listener).await,
            Err(HubServerError::NonLoopback(_))
        ));
    }

    #[test]
    fn hub_api_document_contains_slice_three_library_operations() {
        let document = HubApiDoc::openapi();
        assert_eq!(
            document.servers.as_ref().unwrap()[0].url,
            super::super::protocol::HUB_LOOPBACK_BASE_URL
        );
        assert_eq!(document.paths.paths.len(), 11);
        assert!(document.paths.paths.contains_key("/v1/info"));
        assert!(document.paths.paths.contains_key("/v1/snapshots"));
        assert!(document.paths.paths.contains_key("/v1/changes"));
        assert!(document.paths.paths.contains_key("/v1/dictations"));
        assert!(document
            .paths
            .paths
            .contains_key("/v1/dictations/{sync_id}"));
        assert!(document.paths.paths.contains_key("/v1/meetings"));
        assert!(document.paths.paths.contains_key("/v1/meetings/{sync_id}"));
        assert!(document.paths.paths.contains_key("/v1/artifacts/{sync_id}"));
        assert!(document.paths.paths.contains_key("/v1/blobs/{sha256}"));
        assert!(document
            .paths
            .paths
            .contains_key("/v1/dictations/{sync_id}/payload"));
        assert!(document
            .paths
            .paths
            .contains_key("/v1/meetings/{sync_id}/payload"));
        let operation = document
            .paths
            .paths
            .get("/v1/info")
            .and_then(|item| item.get.as_ref());
        assert!(operation.is_some());
        assert!(document
            .components
            .as_ref()
            .is_some_and(|components| components.schemas.contains_key("HubId")));
    }
}
