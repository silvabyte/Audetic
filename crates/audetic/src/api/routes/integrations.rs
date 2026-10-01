//! Local administrative API for external audio integrations.

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{delete, get, post};
use axum::Router;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::api::error::{ApiError, ApiErrorResponse, ApiResult};
use crate::integrations::ingress::{GENERIC_AUDIO_PATH, INDEX_PATH};
use crate::integrations::{
    AccessKeyInfo, AccessKeyScope, ExternalImportInfo, IntegrationService, IssuedAccessKey,
    PlaudStatus, PlaudSyncAccepted, PUBLIC_INGRESS_BASE_URL,
};

#[derive(Clone)]
pub struct IntegrationApiState {
    pub service: IntegrationService,
}

pub fn router(state: IntegrationApiState) -> Router {
    Router::new()
        .route("/integrations", get(get_overview))
        .route(
            "/integrations/keys",
            get(list_access_keys).post(create_access_key),
        )
        .route("/integrations/keys/:id", delete(revoke_access_key))
        .route("/integrations/imports", get(list_imports))
        .route(
            "/integrations/plaud",
            get(get_plaud_status).put(update_plaud_settings),
        )
        .route("/integrations/plaud/sync", post(sync_plaud))
        .route("/integrations/plaud/backfill", post(backfill_plaud))
        .with_state(state)
}

#[derive(Debug, Serialize, ToSchema)]
pub struct IntegrationOverview {
    pub public_base_url: String,
    pub index_webhook_url: String,
    pub generic_audio_url: String,
    pub ingress_origin: String,
    pub upload_limit_bytes: u64,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateAccessKeyRequest {
    pub name: String,
    pub scope: AccessKeyScope,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AccessKeyListResponse {
    pub keys: Vec<AccessKeyInfo>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ExternalImportListResponse {
    pub imports: Vec<ExternalImportInfo>,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct ExternalImportListQuery {
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdatePlaudSettingsRequest {
    pub enabled: bool,
    pub interval_minutes: i64,
}

#[utoipa::path(
    get,
    path = "/integrations",
    tag = "integrations",
    responses((status = 200, description = "External integration endpoints", body = IntegrationOverview)),
)]
pub async fn get_overview() -> ApiResult<Json<IntegrationOverview>> {
    Ok(Json(IntegrationOverview {
        public_base_url: PUBLIC_INGRESS_BASE_URL.to_string(),
        index_webhook_url: format!("{PUBLIC_INGRESS_BASE_URL}{INDEX_PATH}"),
        generic_audio_url: format!("{PUBLIC_INGRESS_BASE_URL}{GENERIC_AUDIO_PATH}"),
        ingress_origin: crate::integrations::ingress_url("").map_err(ApiError::from)?,
        upload_limit_bytes: 50 * 1024 * 1024,
    }))
}

#[utoipa::path(
    get,
    path = "/integrations/keys",
    tag = "integrations",
    responses((status = 200, description = "Ingress access keys without secrets or hashes", body = AccessKeyListResponse)),
)]
pub async fn list_access_keys(
    State(state): State<IntegrationApiState>,
) -> ApiResult<Json<AccessKeyListResponse>> {
    Ok(Json(AccessKeyListResponse {
        keys: state
            .service
            .list_access_keys()
            .await
            .map_err(ApiError::from)?,
    }))
}

#[utoipa::path(
    post,
    path = "/integrations/keys",
    tag = "integrations",
    request_body = CreateAccessKeyRequest,
    responses(
        (status = 201, description = "Access key created; secret is returned exactly once", body = IssuedAccessKey),
        (status = 400, description = "Invalid key name", body = ApiErrorResponse),
        (status = 500, description = "Access key could not be created", body = ApiErrorResponse),
    ),
)]
pub async fn create_access_key(
    State(state): State<IntegrationApiState>,
    Json(request): Json<CreateAccessKeyRequest>,
) -> ApiResult<Response> {
    if request.name.trim().is_empty() {
        return Err(ApiError::bad_request("name is required"));
    }
    let issued = state
        .service
        .issue_access_key(request.name, request.scope)
        .await
        .map_err(ApiError::from)?;
    let mut response = (StatusCode::CREATED, Json(issued)).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

#[utoipa::path(
    delete,
    path = "/integrations/keys/{id}",
    tag = "integrations",
    params(("id" = String, Path, description = "Access key ID")),
    responses(
        (status = 200, description = "Access key revoked", body = AccessKeyInfo),
        (status = 404, description = "Access key not found", body = ApiErrorResponse),
        (status = 500, description = "Access key could not be revoked", body = ApiErrorResponse),
    ),
)]
pub async fn revoke_access_key(
    Path(id): Path<String>,
    State(state): State<IntegrationApiState>,
) -> ApiResult<Json<AccessKeyInfo>> {
    state
        .service
        .revoke_access_key(id)
        .await
        .map_err(ApiError::from)?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("Access key not found"))
}

#[utoipa::path(
    get,
    path = "/integrations/imports",
    tag = "integrations",
    params(ExternalImportListQuery),
    responses((status = 200, description = "Recent external imports", body = ExternalImportListResponse)),
)]
pub async fn list_imports(
    Query(query): Query<ExternalImportListQuery>,
    State(state): State<IntegrationApiState>,
) -> ApiResult<Json<ExternalImportListResponse>> {
    Ok(Json(ExternalImportListResponse {
        imports: state
            .service
            .recent_imports(query.limit.unwrap_or(20))
            .await
            .map_err(ApiError::from)?,
    }))
}

#[utoipa::path(
    get,
    path = "/integrations/plaud",
    tag = "integrations",
    responses((status = 200, description = "Plaud CLI and synchronization status", body = PlaudStatus)),
)]
pub async fn get_plaud_status(
    State(state): State<IntegrationApiState>,
) -> ApiResult<Json<PlaudStatus>> {
    Ok(Json(
        state.service.plaud_status().await.map_err(ApiError::from)?,
    ))
}

#[utoipa::path(
    put,
    path = "/integrations/plaud",
    tag = "integrations",
    request_body = UpdatePlaudSettingsRequest,
    responses(
        (status = 200, description = "Plaud synchronization settings updated", body = PlaudStatus),
        (status = 400, description = "Invalid synchronization interval", body = ApiErrorResponse),
        (status = 500, description = "Plaud settings could not be updated", body = ApiErrorResponse),
    ),
)]
pub async fn update_plaud_settings(
    State(state): State<IntegrationApiState>,
    Json(request): Json<UpdatePlaudSettingsRequest>,
) -> ApiResult<Json<PlaudStatus>> {
    if !(5..=1440).contains(&request.interval_minutes) {
        return Err(ApiError::bad_request(
            "interval_minutes must be between 5 and 1440",
        ));
    }
    let current = state.service.plaud_state().await.map_err(ApiError::from)?;
    let import_after = current.import_after.or_else(|| {
        request
            .enabled
            .then(|| chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string())
    });
    Ok(Json(
        state
            .service
            .update_plaud_settings(request.enabled, request.interval_minutes, import_after)
            .await
            .map_err(ApiError::from)?,
    ))
}

#[utoipa::path(
    post,
    path = "/integrations/plaud/sync",
    tag = "integrations",
    responses(
        (status = 202, description = "Incremental Plaud synchronization scheduled", body = PlaudSyncAccepted),
        (status = 409, description = "Plaud synchronization is already running", body = ApiErrorResponse),
        (status = 500, description = "Plaud synchronization could not be scheduled", body = ApiErrorResponse),
    ),
)]
pub async fn sync_plaud(
    State(state): State<IntegrationApiState>,
) -> ApiResult<(StatusCode, Json<PlaudSyncAccepted>)> {
    schedule_plaud(&state.service, false).await
}

#[utoipa::path(
    post,
    path = "/integrations/plaud/backfill",
    tag = "integrations",
    responses(
        (status = 202, description = "Historical Plaud synchronization scheduled", body = PlaudSyncAccepted),
        (status = 409, description = "Plaud synchronization is already running", body = ApiErrorResponse),
        (status = 500, description = "Plaud synchronization could not be scheduled", body = ApiErrorResponse),
    ),
)]
pub async fn backfill_plaud(
    State(state): State<IntegrationApiState>,
) -> ApiResult<(StatusCode, Json<PlaudSyncAccepted>)> {
    schedule_plaud(&state.service, true).await
}

async fn schedule_plaud(
    service: &IntegrationService,
    backfill: bool,
) -> ApiResult<(StatusCode, Json<PlaudSyncAccepted>)> {
    if !service
        .schedule_plaud_sync(backfill)
        .await
        .map_err(ApiError::from)?
    {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "Plaud synchronization is already running",
        ));
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(PlaudSyncAccepted {
            scheduled: true,
            message: if backfill {
                "Historical Plaud synchronization scheduled".to_string()
            } else {
                "Plaud synchronization scheduled".to_string()
            },
        }),
    ))
}
