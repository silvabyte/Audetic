//! Small public HTTP surface for authenticated external audio delivery.

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Extension, Multipart, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::post;
use axum::Router;
use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tower_http::timeout::TimeoutLayer;
use utoipa::openapi::security::{Http, HttpAuthScheme, SecurityScheme};
use utoipa::{Modify, OpenApi, ToSchema};

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{
    AccessKeyScope, AuthenticatedAccessKey, ExternalImportOutcome, ExternalImportRequest,
    IntegrationService,
};

pub const INDEX_PATH: &str = "/v1/index";
pub const GENERIC_AUDIO_PATH: &str = "/v1/audio";
pub(super) const MAX_UPLOAD_BYTES: u64 = 50 * 1024 * 1024;
const MAX_REQUEST_BYTES: usize = MAX_UPLOAD_BYTES as usize + 1024 * 1024;
const MAX_TEXT_BYTES: usize = 8 * 1024;

#[derive(Clone)]
pub struct IngressServer {
    service: IntegrationService,
}

impl IngressServer {
    pub fn new(service: IntegrationService) -> Self {
        Self { service }
    }

    pub fn router(&self) -> Router {
        let index = scoped_router(
            INDEX_PATH,
            index_webhook,
            self.service.clone(),
            AccessKeyScope::Index,
        );
        let generic = scoped_router(
            GENERIC_AUDIO_PATH,
            generic_audio_webhook,
            self.service.clone(),
            AccessKeyScope::Generic,
        );
        Router::new()
            .merge(index)
            .merge(generic)
            .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
            .layer(TimeoutLayer::new(Duration::from_secs(300)))
    }

    pub async fn serve(self, listener: tokio::net::TcpListener) -> anyhow::Result<()> {
        axum::serve(listener, self.router()).await?;
        Ok(())
    }
}

pub fn ingress_bind_addr() -> anyhow::Result<SocketAddr> {
    Ok(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        audetic_core::url::ingress_port()?,
    ))
}

type IngressHandler = fn(
    State<IntegrationService>,
    Extension<AuthenticatedAccessKey>,
    HeaderMap,
    Multipart,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Response, IngressError>> + Send>,
>;

fn scoped_router(
    path: &'static str,
    handler: IngressHandler,
    service: IntegrationService,
    scope: AccessKeyScope,
) -> Router {
    Router::new()
        .route(path, post(handler))
        .route_layer(middleware::from_fn_with_state(
            AuthState {
                service: service.clone(),
                scope,
            },
            authenticate,
        ))
        .with_state(service)
}

#[derive(Clone)]
struct AuthState {
    service: IntegrationService,
    scope: AccessKeyScope,
}

async fn authenticate(
    State(state): State<AuthState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let Some(token) = bearer_token(request.headers().get(header::AUTHORIZATION)) else {
        return unauthorized();
    };
    match state.service.authenticate(token, state.scope).await {
        Ok(Some(authenticated)) => {
            request.extensions_mut().insert(authenticated);
            next.run(request).await
        }
        Ok(None) => unauthorized(),
        Err(error) => {
            tracing::error!(%error, "Ingress authentication failed");
            IngressError::Internal.into_response()
        }
    }
}

fn bearer_token(value: Option<&HeaderValue>) -> Option<&str> {
    let value = value?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer")
        || token.is_empty()
        || token.chars().any(char::is_whitespace)
    {
        return None;
    }
    Some(token)
}

fn index_webhook(
    state: State<IntegrationService>,
    authenticated: Extension<AuthenticatedAccessKey>,
    headers: HeaderMap,
    multipart: Multipart,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response, IngressError>> + Send>> {
    Box::pin(index_webhook_inner(
        state,
        authenticated,
        headers,
        multipart,
    ))
}

#[utoipa::path(
    post,
    path = "/v1/index",
    tag = "ingress",
    operation_id = "ingest_index_recording",
    security(("bearer_auth" = [])),
    request_body(content_type = "multipart/form-data"),
    responses(
        (status = 200, description = "Test or duplicate delivery accepted", body = IngressResponse),
        (status = 202, description = "Recording accepted", body = IngressResponse),
        (status = 400, description = "Invalid Index payload", body = IngressErrorResponse),
        (status = 401, description = "Missing or invalid access key", body = IngressErrorResponse),
        (status = 409, description = "Delivery is already being imported", body = IngressResponse),
        (status = 413, description = "Audio exceeds the upload limit", body = IngressErrorResponse),
        (status = 429, description = "Too many concurrent imports", body = IngressErrorResponse),
        (status = 500, description = "Import failed", body = IngressErrorResponse),
    ),
)]
async fn index_webhook_inner(
    State(service): State<IntegrationService>,
    Extension(authenticated): Extension<AuthenticatedAccessKey>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Response, IngressError> {
    let permit = service.try_acquire_ingress().ok_or(IngressError::Busy)?;
    require_header(&headers, "x-index-webhook-version", "1")?;
    if let Some(size) = header_u64(&headers, "x-audio-size")? {
        if size > MAX_UPLOAD_BYTES {
            return Err(IngressError::TooLarge);
        }
    }

    let header_is_test = headers
        .get("x-index-test")
        .and_then(|value| value.to_str().ok())
        == Some("true");
    let trigger = headers
        .get("x-index-trigger")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown")
        .to_string();
    let delivery_id = headers
        .get("x-index-delivery")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    let mut staged: Option<(StagedUpload, String)> = None;
    let mut recorded_at: Option<String> = None;
    let mut client: Option<String> = None;
    let mut form_is_test = false;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| IngressError::BadRequest("malformed multipart body"))?
    {
        match field.name() {
            Some("audio") => {
                if staged.is_some() {
                    return Err(IngressError::BadRequest("only one audio part is allowed"));
                }
                let filename = field
                    .file_name()
                    .map(str::to_string)
                    .ok_or(IngressError::BadRequest("audio filename is required"))?;
                if !filename.to_ascii_lowercase().ends_with(".m4a") {
                    return Err(IngressError::BadRequest("Index audio must be M4A"));
                }
                let path = stage_field(&service, field).await?;
                staged = Some((path, filename));
            }
            Some("recordedAt") => {
                let millis = read_text(field, "recordedAt")
                    .await?
                    .ok_or(IngressError::BadRequest("invalid recordedAt"))?
                    .parse::<i64>()
                    .map_err(|_| IngressError::BadRequest("invalid recordedAt"))?;
                recorded_at = Some(timestamp_millis(millis)?);
            }
            Some("client") => {
                client = read_text(field, "client").await?;
            }
            Some("test") => {
                form_is_test = read_text(field, "test").await?.as_deref() == Some("true");
            }
            _ => {}
        }
    }

    if header_is_test || form_is_test || trigger == "test-event" {
        drop(staged);
        return Ok((
            StatusCode::OK,
            Json(IngressResponse::test("Index test event accepted")),
        )
            .into_response());
    }
    if client.as_deref() != Some("ring") {
        drop(staged);
        return Err(IngressError::BadRequest("Index client must be ring"));
    }
    let (staged_upload, filename) = staged.ok_or(IngressError::BadRequest(
        "recording delivery is missing audio",
    ))?;
    let staged_path = staged_upload.persist();
    let external_id = delivery_id
        .or_else(|| {
            Path::new(&filename)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_string)
        })
        .filter(|id| !id.trim().is_empty())
        .ok_or(IngressError::BadRequest(
            "recording delivery is missing an external ID",
        ))?;

    import_response(
        &service,
        ExternalImportRequest {
            provider: "pebble_index".to_string(),
            source_instance: authenticated.id.clone(),
            external_id,
            recorded_at,
            source_filename: Some(filename),
            title: None,
            staged_path,
        },
        permit,
    )
    .await
}

fn generic_audio_webhook(
    state: State<IntegrationService>,
    authenticated: Extension<AuthenticatedAccessKey>,
    headers: HeaderMap,
    multipart: Multipart,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response, IngressError>> + Send>> {
    Box::pin(generic_audio_webhook_inner(
        state,
        authenticated,
        headers,
        multipart,
    ))
}

#[utoipa::path(
    post,
    path = "/v1/audio",
    tag = "ingress",
    operation_id = "ingest_generic_audio",
    security(("bearer_auth" = [])),
    request_body(content_type = "multipart/form-data"),
    responses(
        (status = 200, description = "Duplicate delivery accepted", body = IngressResponse),
        (status = 202, description = "Audio accepted", body = IngressResponse),
        (status = 400, description = "Invalid audio payload", body = IngressErrorResponse),
        (status = 401, description = "Missing or invalid access key", body = IngressErrorResponse),
        (status = 409, description = "Delivery is already being imported", body = IngressResponse),
        (status = 413, description = "Audio exceeds the upload limit", body = IngressErrorResponse),
        (status = 429, description = "Too many concurrent imports", body = IngressErrorResponse),
        (status = 500, description = "Import failed", body = IngressErrorResponse),
    ),
)]
async fn generic_audio_webhook_inner(
    State(service): State<IntegrationService>,
    Extension(authenticated): Extension<AuthenticatedAccessKey>,
    _headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Response, IngressError> {
    let permit = service.try_acquire_ingress().ok_or(IngressError::Busy)?;
    let mut staged: Option<(StagedUpload, String)> = None;
    let mut external_id: Option<String> = None;
    let mut recorded_at: Option<String> = None;
    let mut title: Option<String> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| IngressError::BadRequest("malformed multipart body"))?
    {
        match field.name() {
            Some("audio") => {
                if staged.is_some() {
                    return Err(IngressError::BadRequest("only one audio part is allowed"));
                }
                let filename = field
                    .file_name()
                    .map(str::to_string)
                    .ok_or(IngressError::BadRequest("audio filename is required"))?;
                let extension = Path::new(&filename)
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .map(str::to_ascii_lowercase)
                    .ok_or(IngressError::BadRequest(
                        "audio filename needs an extension",
                    ))?;
                if audetic_core::jobs_client::mime_type_for_extension(&extension).is_none() {
                    return Err(IngressError::BadRequest("unsupported audio extension"));
                }
                let path = stage_field(&service, field).await?;
                staged = Some((path, filename));
            }
            Some("external_id") => external_id = read_text(field, "external_id").await?,
            Some("title") => title = read_text(field, "title").await?,
            Some("recorded_at") => {
                if let Some(value) = read_text(field, "recorded_at").await? {
                    recorded_at = Some(parse_timestamp(&value)?);
                }
            }
            _ => {}
        }
    }

    let (staged_upload, filename) = staged.ok_or(IngressError::BadRequest("audio is required"))?;
    let staged_path = staged_upload.persist();
    let external_id = external_id.ok_or(IngressError::BadRequest("external_id is required"))?;
    import_response(
        &service,
        ExternalImportRequest {
            provider: "generic_webhook".to_string(),
            source_instance: authenticated.id.clone(),
            external_id,
            recorded_at,
            source_filename: Some(filename),
            title,
            staged_path,
        },
        permit,
    )
    .await
}

async fn import_response(
    service: &IntegrationService,
    request: ExternalImportRequest,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> Result<Response, IngressError> {
    let service = service.clone();
    let result = tokio::spawn(async move {
        let _permit = permit;
        service.import_external(request).await
    })
    .await
    .map_err(|error| {
        tracing::error!(%error, "External audio import task failed");
        IngressError::Internal
    })?;
    match result {
        Ok(ExternalImportOutcome::Accepted { note_id }) => Ok((
            StatusCode::ACCEPTED,
            Json(IngressResponse::accepted(note_id, false)),
        )
            .into_response()),
        Ok(ExternalImportOutcome::Duplicate { note_id }) => Ok((
            StatusCode::OK,
            Json(IngressResponse::accepted(note_id, true)),
        )
            .into_response()),
        Ok(ExternalImportOutcome::InProgress) => Ok((
            StatusCode::CONFLICT,
            Json(IngressResponse {
                accepted: false,
                duplicate: true,
                test: false,
                note_id: None,
                message: "Delivery is already being imported".to_string(),
            }),
        )
            .into_response()),
        Err(error) => {
            tracing::error!(%error, "External audio import failed");
            Err(IngressError::Internal)
        }
    }
}

async fn stage_field(
    service: &IntegrationService,
    mut field: axum::extract::multipart::Field<'_>,
) -> Result<StagedUpload, IngressError> {
    let uploads_dir = service.uploads_dir();
    tokio::fs::create_dir_all(&uploads_dir)
        .await
        .map_err(|_| IngressError::Internal)?;
    let path = uploads_dir.join(format!("external-{}", uuid::Uuid::new_v4().simple()));
    let staged = StagedUpload::new(path);
    let mut file = tokio::fs::File::create(&staged.path)
        .await
        .map_err(|_| IngressError::Internal)?;
    let mut written = 0_u64;
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|_| IngressError::BadRequest("invalid audio body"))?
    {
        written = written.saturating_add(chunk.len() as u64);
        if written > MAX_UPLOAD_BYTES {
            drop(file);
            return Err(IngressError::TooLarge);
        }
        file.write_all(&chunk)
            .await
            .map_err(|_| IngressError::Internal)?;
    }
    file.flush().await.map_err(|_| IngressError::Internal)?;
    if written == 0 {
        return Err(IngressError::BadRequest("audio is empty"));
    }
    Ok(staged)
}

async fn read_text(
    mut field: axum::extract::multipart::Field<'_>,
    label: &'static str,
) -> Result<Option<String>, IngressError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|_| IngressError::BadRequest(label))?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_TEXT_BYTES {
            return Err(IngressError::BadRequest(
                "multipart text field is too large",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    let value = String::from_utf8(bytes).map_err(|_| IngressError::BadRequest(label))?;
    Ok((!value.trim().is_empty()).then(|| value.trim().to_string()))
}

fn timestamp_millis(value: i64) -> Result<String, IngressError> {
    let timestamp = chrono::DateTime::from_timestamp_millis(value)
        .ok_or(IngressError::BadRequest("invalid recordedAt"))?;
    Ok(timestamp.format("%Y-%m-%d %H:%M:%S").to_string())
}

fn parse_timestamp(value: &str) -> Result<String, IngressError> {
    let timestamp = chrono::DateTime::parse_from_rfc3339(value)
        .map_err(|_| IngressError::BadRequest("recorded_at must be RFC 3339"))?;
    Ok(timestamp
        .with_timezone(&chrono::Utc)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string())
}

fn require_header(
    headers: &HeaderMap,
    name: &'static str,
    expected: &'static str,
) -> Result<(), IngressError> {
    if headers.get(name).and_then(|value| value.to_str().ok()) == Some(expected) {
        Ok(())
    } else {
        Err(IngressError::BadRequest("unsupported webhook version"))
    }
}

fn header_u64(headers: &HeaderMap, name: &'static str) -> Result<Option<u64>, IngressError> {
    headers
        .get(name)
        .map(|value| {
            value
                .to_str()
                .ok()
                .and_then(|value| value.parse().ok())
                .ok_or(IngressError::BadRequest("invalid size header"))
        })
        .transpose()
}

struct StagedUpload {
    path: PathBuf,
    persist: bool,
}

impl StagedUpload {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            persist: false,
        }
    }

    fn persist(mut self) -> PathBuf {
        self.persist = true;
        std::mem::take(&mut self.path)
    }
}

impl Drop for StagedUpload {
    fn drop(&mut self) {
        if !self.persist {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct IngressResponse {
    pub accepted: bool,
    pub duplicate: bool,
    pub test: bool,
    pub note_id: Option<i64>,
    pub message: String,
}

impl IngressResponse {
    fn accepted(note_id: i64, duplicate: bool) -> Self {
        Self {
            accepted: true,
            duplicate,
            test: false,
            note_id: Some(note_id),
            message: if duplicate {
                "Delivery was already accepted".to_string()
            } else {
                "Audio accepted for processing".to_string()
            },
        }
    }

    fn test(message: &str) -> Self {
        Self {
            accepted: true,
            duplicate: false,
            test: true,
            note_id: None,
            message: message.to_string(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct IngressErrorResponse {
    error: &'static str,
}

#[derive(Debug)]
enum IngressError {
    BadRequest(&'static str),
    TooLarge,
    Busy,
    Internal,
}

impl IntoResponse for IngressError {
    fn into_response(self) -> Response {
        match self {
            Self::BadRequest(error) => (
                StatusCode::BAD_REQUEST,
                Json(IngressErrorResponse { error }),
            )
                .into_response(),
            Self::TooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(IngressErrorResponse {
                    error: "audio exceeds the 50 MiB limit",
                }),
            )
                .into_response(),
            Self::Busy => (
                StatusCode::TOO_MANY_REQUESTS,
                Json(IngressErrorResponse {
                    error: "too many concurrent imports",
                }),
            )
                .into_response(),
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(IngressErrorResponse {
                    error: "internal_error",
                }),
            )
                .into_response(),
        }
    }
}

fn unauthorized() -> Response {
    let mut response = (
        StatusCode::UNAUTHORIZED,
        Json(IngressErrorResponse {
            error: "unauthorized",
        }),
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Audetic external audio ingress API",
        description = "Authenticated, loopback-only origin for third-party audio delivery.",
        version = env!("CARGO_PKG_VERSION"),
    ),
    servers((url = "http://127.0.0.1:3739", description = "Local tunnel origin")),
    paths(index_webhook_inner, generic_audio_webhook_inner),
    components(schemas(IngressResponse, IngressErrorResponse)),
    modifiers(&IngressSecurity),
    tags((name = "ingress", description = "Third-party audio delivery")),
)]
pub struct IngressApiDoc;

struct IngressSecurity;

impl Modify for IngressSecurity {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "bearer_auth",
                SecurityScheme::Http(Http::new(HttpAuthScheme::Bearer)),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use async_trait::async_trait;
    use axum::http::Request;
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use tower::ServiceExt;

    struct UnusedTranscription;

    #[async_trait]
    impl crate::transcription::job_service::TranscriptionJobService for UnusedTranscription {
        async fn submit_and_poll(
            &self,
            _file_path: &Path,
            _language: Option<&str>,
        ) -> Result<crate::transcription::job_service::TranscriptionJobResult> {
            anyhow::bail!("test event must not transcribe")
        }
    }

    struct UnusedInspector;

    #[async_trait]
    impl crate::audio_notes::media_inspector::MediaInspector for UnusedInspector {
        async fn probe_duration_seconds(&self, _path: &Path) -> Option<u64> {
            None
        }
    }

    #[test]
    fn timestamps_are_normalized_for_audio_note_ordering() {
        assert_eq!(
            timestamp_millis(1_780_012_800_000).unwrap(),
            "2026-05-29 00:00:00"
        );
        assert_eq!(
            parse_timestamp("2026-05-28T20:00:00-04:00").unwrap(),
            "2026-05-29 00:00:00"
        );
    }

    #[test]
    fn ingress_document_and_listener_are_isolated() {
        assert_eq!(
            ingress_bind_addr().unwrap(),
            "127.0.0.1:3739".parse().unwrap()
        );
        let document = IngressApiDoc::openapi();
        let paths = document
            .paths
            .paths
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        assert_eq!(
            paths,
            BTreeSet::from([INDEX_PATH.to_string(), GENERIC_AUDIO_PATH.to_string()])
        );
    }

    #[test]
    fn staged_upload_removes_unclaimed_files() {
        let directory = tempfile::tempdir().unwrap();
        let abandoned = directory.path().join("abandoned");
        std::fs::write(&abandoned, b"audio").unwrap();
        drop(StagedUpload::new(abandoned.clone()));
        assert!(!abandoned.exists());

        let claimed = directory.path().join("claimed");
        std::fs::write(&claimed, b"audio").unwrap();
        let retained = StagedUpload::new(claimed.clone()).persist();
        assert_eq!(retained, claimed);
        assert!(claimed.exists());
    }

    #[tokio::test]
    async fn index_test_events_require_an_index_scoped_bearer_key() {
        let directory = tempfile::tempdir().unwrap();
        let db_path = directory.path().join("audetic.db");
        let service = IntegrationService::new(
            db_path.clone(),
            directory.path().join("audio-notes"),
            crate::audio_notes::processing::ProcessingServices::new(
                Arc::new(UnusedTranscription),
                db_path,
            ),
            Arc::new(UnusedInspector),
        );
        let generic = service
            .issue_access_key("Generic".to_string(), AccessKeyScope::Generic)
            .await
            .unwrap();
        let index = service
            .issue_access_key("Index".to_string(), AccessKeyScope::Index)
            .await
            .unwrap();
        let router = IngressServer::new(service).router();

        assert_eq!(
            router
                .clone()
                .oneshot(index_test_request(None))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            router
                .clone()
                .oneshot(index_test_request(Some(&generic.secret)))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            router
                .oneshot(index_test_request(Some(&index.secret)))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }

    fn index_test_request(token: Option<&str>) -> Request<Body> {
        let boundary = "audetic-test-boundary";
        let body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"test\"\r\n\r\ntrue\r\n--{boundary}--\r\n"
        );
        let mut request = Request::post(INDEX_PATH)
            .header(
                "content-type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .header("x-index-webhook-version", "1")
            .body(Body::from(body))
            .unwrap();
        if let Some(token) = token {
            request.headers_mut().insert(
                header::AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
            );
        }
        request
    }
}
