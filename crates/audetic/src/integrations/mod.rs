//! External audio integrations behind a small authenticated domain interface.

pub mod ingress;
pub mod plaud;
pub mod recording_source;

use anyhow::{Context, Result};
use audetic_core::url::HOST;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rusqlite::Connection;
use serde::Serialize;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use utoipa::ToSchema;
use uuid::Uuid;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::audio_notes::import::{import_audio_note_file, ImportArgs};
use crate::audio_notes::media_inspector::MediaInspector;
use crate::audio_notes::processing::ProcessingServices;
use crate::db::integrations::{
    ExternalImport, ImportClaim, IngressAccessKey, IntegrationRepository, RecordingSource,
};

use self::plaud::PlaudSource;
use self::recording_source::RecordingSource as RecordingSourceDriver;

const ACCESS_KEY_PREFIX: &str = "audetic_ingress_";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccessKeyScope {
    Index,
    Generic,
}

impl AccessKeyScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Index => "index",
            Self::Generic => "generic",
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AccessKeyInfo {
    pub id: String,
    pub name: String,
    pub scope: AccessKeyScope,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
}

#[derive(Clone, Serialize, ToSchema)]
pub struct IssuedAccessKey {
    #[serde(flatten)]
    pub info: AccessKeyInfo,
    /// Displayed exactly once. Only its SHA-256 digest is persisted.
    pub secret: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ExternalImportInfo {
    pub id: i64,
    pub provider: String,
    pub external_id: String,
    pub status: String,
    pub audio_note_id: Option<i64>,
    pub recorded_at: Option<String>,
    pub source_filename: Option<String>,
    pub error: Option<String>,
    pub attempt_count: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PlaudStatus {
    pub available: bool,
    pub authenticated: bool,
    pub version: Option<String>,
    pub enabled: bool,
    pub interval_minutes: i64,
    pub import_after: Option<String>,
    pub running: bool,
    pub last_started_at: Option<String>,
    pub last_completed_at: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PlaudSyncAccepted {
    pub scheduled: bool,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct AuthenticatedAccessKey {
    pub id: String,
    pub scope: AccessKeyScope,
}

pub struct ExternalImportRequest {
    pub provider: String,
    pub source_instance: String,
    pub external_id: String,
    pub recorded_at: Option<String>,
    pub source_filename: Option<String>,
    pub title: Option<String>,
    pub staged_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalImportOutcome {
    Accepted { note_id: i64 },
    Duplicate { note_id: i64 },
    InProgress,
}

#[derive(Clone)]
pub struct IntegrationService {
    db_path: PathBuf,
    audio_notes_dir: PathBuf,
    services: ProcessingServices,
    inspector: Arc<dyn MediaInspector>,
    ingress_slots: Arc<tokio::sync::Semaphore>,
}

impl IntegrationService {
    pub fn new(
        db_path: PathBuf,
        audio_notes_dir: PathBuf,
        services: ProcessingServices,
        inspector: Arc<dyn MediaInspector>,
    ) -> Self {
        Self {
            db_path,
            audio_notes_dir,
            services,
            inspector,
            ingress_slots: Arc::new(tokio::sync::Semaphore::new(4)),
        }
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    pub fn uploads_dir(&self) -> PathBuf {
        self.audio_notes_dir.join(".uploads")
    }

    pub fn try_acquire_ingress(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        self.ingress_slots.clone().try_acquire_owned().ok()
    }

    async fn with_conn<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
    {
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = crate::db::init_db_at(&db_path)?;
            f(&conn)
        })
        .await?
    }

    pub async fn recover_interrupted_work(&self) -> Result<()> {
        match tokio::fs::remove_dir_all(self.uploads_dir()).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("Failed to remove interrupted uploads"),
        }
        self.with_conn(|conn| {
            IntegrationRepository::sweep_interrupted_imports(conn)?;
            IntegrationRepository::sweep_interrupted_syncs(conn)
        })
        .await?;
        Ok(())
    }

    pub async fn issue_access_key(
        &self,
        name: String,
        scope: AccessKeyScope,
    ) -> Result<IssuedAccessKey> {
        let id = Uuid::new_v4().to_string();
        let mut random = [0_u8; 32];
        getrandom::fill(&mut random)
            .map_err(|error| anyhow::anyhow!("Failed to generate ingress access key: {error}"))?;
        let secret = format!("{ACCESS_KEY_PREFIX}{id}_{}", URL_SAFE_NO_PAD.encode(random));
        let hash: [u8; 32] = Sha256::digest(secret.as_bytes()).into();
        let id_for_db = id.clone();
        let scope_text = scope.as_str().to_string();
        let key = self
            .with_conn(move |conn| {
                IntegrationRepository::insert_access_key(
                    conn,
                    &id_for_db,
                    &name,
                    &scope_text,
                    &hash,
                )
            })
            .await?;
        Ok(IssuedAccessKey {
            info: key.try_into()?,
            secret,
        })
    }

    pub async fn list_access_keys(&self) -> Result<Vec<AccessKeyInfo>> {
        self.with_conn(|conn| {
            IntegrationRepository::list_access_keys(conn)?
                .into_iter()
                .map(TryInto::try_into)
                .collect()
        })
        .await
    }

    pub async fn revoke_access_key(&self, id: String) -> Result<Option<AccessKeyInfo>> {
        self.with_conn(move |conn| {
            IntegrationRepository::revoke_access_key(conn, &id)?
                .map(TryInto::try_into)
                .transpose()
        })
        .await
    }

    pub async fn authenticate(
        &self,
        token: &str,
        scope: AccessKeyScope,
    ) -> Result<Option<AuthenticatedAccessKey>> {
        let Some(id) = token_id(token) else {
            return Ok(None);
        };
        let candidate: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        let id_for_db = id.clone();
        let scope_text = scope.as_str().to_string();
        let authenticated = self
            .with_conn(move |conn| {
                let Some(stored) =
                    IntegrationRepository::active_access_key_hash(conn, &id_for_db, &scope_text)?
                else {
                    return Ok(false);
                };
                if stored.len() != candidate.len()
                    || stored.ct_eq(candidate.as_slice()).unwrap_u8() != 1
                {
                    return Ok(false);
                }
                IntegrationRepository::touch_access_key(conn, &id_for_db)?;
                Ok(true)
            })
            .await?;
        Ok(authenticated.then_some(AuthenticatedAccessKey { id, scope }))
    }

    pub async fn import_external(
        &self,
        request: ExternalImportRequest,
    ) -> Result<ExternalImportOutcome> {
        let ExternalImportRequest {
            provider,
            source_instance,
            external_id,
            recorded_at,
            source_filename,
            title,
            staged_path,
        } = request;
        let _staged_cleanup = StagedPathCleanup(staged_path.clone());
        let claim_provider = provider.clone();
        let claim_instance = source_instance.clone();
        let claim_external_id = external_id.clone();
        let claim_recorded_at = recorded_at.clone();
        let claim_filename = source_filename.clone();
        let claim = self
            .with_conn(move |conn| {
                IntegrationRepository::claim_external_import(
                    conn,
                    &claim_provider,
                    &claim_instance,
                    &claim_external_id,
                    claim_recorded_at.as_deref(),
                    claim_filename.as_deref(),
                )
            })
            .await?;

        let import_id = match claim {
            ImportClaim::Accepted { note_id, .. } => {
                remove_staged(&staged_path).await;
                return Ok(ExternalImportOutcome::Duplicate { note_id });
            }
            ImportClaim::InProgress { .. } => {
                remove_staged(&staged_path).await;
                return Ok(ExternalImportOutcome::InProgress);
            }
            ImportClaim::Claimed(import_id) => import_id,
        };

        let staged_path_for_cleanup = staged_path.clone();
        let result = import_audio_note_file(ImportArgs {
            source_path: staged_path,
            original_filename: source_filename,
            title,
            source_provider: Some(provider),
            source_external_id: Some(external_id),
            source_recorded_at: recorded_at,
            external_import_id: Some(import_id),
            services: self.services.clone(),
            inspector: self.inspector.clone(),
            audio_notes_dir: self.audio_notes_dir.clone(),
        })
        .await;

        match result {
            Ok(result) => {
                let note_id = result.note_id;
                Ok(ExternalImportOutcome::Accepted { note_id })
            }
            Err(error) => {
                remove_staged(&staged_path_for_cleanup).await;
                let public_error = error.to_string();
                let persisted_error = public_error.clone();
                self.with_conn(move |conn| {
                    IntegrationRepository::fail_external_import(conn, import_id, &persisted_error)
                })
                .await?;
                Err(error)
            }
        }
    }

    pub async fn recent_imports(&self, limit: usize) -> Result<Vec<ExternalImportInfo>> {
        self.with_conn(move |conn| {
            Ok(IntegrationRepository::recent_external_imports(conn, limit)?
                .into_iter()
                .map(Into::into)
                .collect())
        })
        .await
    }

    pub async fn recording_source(
        &self,
        provider: &str,
        source_instance: &str,
    ) -> Result<RecordingSource> {
        let provider = provider.to_string();
        let source_instance = source_instance.to_string();
        self.with_conn(move |conn| {
            IntegrationRepository::recording_source(conn, &provider, &source_instance)
        })
        .await
    }

    pub async fn enabled_poll_sources(&self) -> Result<Vec<RecordingSource>> {
        self.with_conn(IntegrationRepository::enabled_poll_sources)
            .await
    }

    pub async fn update_recording_source(
        &self,
        provider: &str,
        source_instance: &str,
        enabled: bool,
        interval_minutes: Option<i64>,
        import_after: Option<String>,
    ) -> Result<()> {
        let provider = provider.to_string();
        let source_instance = source_instance.to_string();
        self.with_conn(move |conn| {
            IntegrationRepository::update_recording_source_settings(
                conn,
                &provider,
                &source_instance,
                enabled,
                interval_minutes,
                import_after.as_deref(),
            )
        })
        .await
    }

    pub(crate) async fn update_import_cursor(
        &self,
        provider: &str,
        source_instance: &str,
        import_after: &str,
    ) -> Result<()> {
        let provider = provider.to_string();
        let source_instance = source_instance.to_string();
        let import_after = import_after.to_string();
        self.with_conn(move |conn| {
            IntegrationRepository::update_import_cursor(
                conn,
                &provider,
                &source_instance,
                &import_after,
            )
        })
        .await
    }

    pub async fn plaud_status(&self) -> Result<PlaudStatus> {
        let state = self
            .recording_source(PlaudSource::PROVIDER, PlaudSource::SOURCE_INSTANCE)
            .await?;
        let Some(cli) = plaud::PlaudCli::discover() else {
            return PlaudStatus::from_state(state, false, false, None);
        };
        let (version, authenticated) =
            tokio::join!(cli.version(), async { cli.authenticated().await });
        PlaudStatus::from_state(state, true, authenticated, version.ok())
    }

    /// Claim the source, then run its poll entry point off the request thread.
    pub async fn schedule_poll(
        &self,
        source: Box<dyn RecordingSourceDriver>,
        backfill: bool,
    ) -> Result<bool> {
        let provider = source.provider().to_string();
        let source_instance = source.source_instance().to_string();
        let start_provider = provider.clone();
        let start_instance = source_instance.clone();
        let claimed = self
            .with_conn(move |conn| {
                IntegrationRepository::start_sync(conn, &start_provider, &start_instance)
            })
            .await?;
        if !claimed {
            return Ok(false);
        }
        let service = self.clone();
        tokio::spawn(async move {
            let result = source.poll(&service, backfill).await;
            if let Err(error) = &result {
                tracing::error!(%error, "Recording source synchronization failed");
            }
            let error = result.as_ref().err().map(|error| error.to_string());
            let finish_result = service
                .with_conn(move |conn| {
                    IntegrationRepository::finish_sync(
                        conn,
                        &provider,
                        &source_instance,
                        error.as_deref(),
                    )
                })
                .await;
            if let Err(finish_error) = finish_result {
                tracing::error!(%finish_error, "failed to persist recording source completion");
            }
        });
        Ok(true)
    }

    pub fn spawn_recording_source_scheduler(&self) {
        let service = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let Ok(sources) = service.enabled_poll_sources().await else {
                    continue;
                };
                for record in sources {
                    if record.running || !recording_source::poll_is_due(&record) {
                        continue;
                    }
                    let Ok(source) =
                        recording_source::resolve(&record.provider, &record.source_instance)
                    else {
                        tracing::error!(
                            provider = %record.provider,
                            source_instance = %record.source_instance,
                            "Configured poll source has no implementation"
                        );
                        continue;
                    };
                    if let Err(error) = service.schedule_poll(source, false).await {
                        tracing::error!(
                            provider = %record.provider,
                            %error,
                            "Failed to schedule recording source sync"
                        );
                    }
                }
            }
        });
    }

    pub(crate) async fn external_imported(
        &self,
        provider: &str,
        source_instance: &str,
        external_id: &str,
    ) -> Result<bool> {
        let provider = provider.to_string();
        let source_instance = source_instance.to_string();
        let external_id = external_id.to_string();
        self.with_conn(move |conn| {
            Ok(IntegrationRepository::accepted_external_import_note_id(
                conn,
                &provider,
                &source_instance,
                &external_id,
            )?
            .is_some())
        })
        .await
    }
}

fn token_id(token: &str) -> Option<String> {
    let remainder = token.strip_prefix(ACCESS_KEY_PREFIX)?;
    let (id, secret) = remainder.split_once('_')?;
    if secret.is_empty() || Uuid::parse_str(id).is_err() {
        return None;
    }
    Some(id.to_string())
}

pub(crate) async fn remove_staged(path: &Path) {
    if let Err(error) = tokio::fs::remove_file(path).await {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(?path, %error, "Failed to clean up staged external import");
        }
    }
}

struct StagedPathCleanup(PathBuf);

impl Drop for StagedPathCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

impl TryFrom<IngressAccessKey> for AccessKeyInfo {
    type Error = anyhow::Error;

    fn try_from(value: IngressAccessKey) -> Result<Self> {
        let scope = match value.scope.as_str() {
            "index" => AccessKeyScope::Index,
            "generic" => AccessKeyScope::Generic,
            other => anyhow::bail!("Stored ingress access key has unknown scope {other}"),
        };
        Ok(Self {
            id: value.id,
            name: value.name,
            scope,
            created_at: value.created_at,
            last_used_at: value.last_used_at,
            revoked_at: value.revoked_at,
        })
    }
}

impl From<ExternalImport> for ExternalImportInfo {
    fn from(value: ExternalImport) -> Self {
        Self {
            id: value.id,
            provider: value.provider,
            external_id: value.external_id,
            status: value.status,
            audio_note_id: value.audio_note_id,
            recorded_at: value.recorded_at,
            source_filename: value.source_filename,
            error: value.error,
            attempt_count: value.attempt_count,
            created_at: value.created_at,
            updated_at: value.updated_at,
        }
    }
}

pub fn ingress_url(path: &str) -> Result<String> {
    Ok(format!(
        "http://{HOST}:{}{path}",
        audetic_core::url::ingress_port()?
    ))
}

pub const PUBLIC_INGRESS_BASE_URL: &str = "https://ingest.audetic.link";

impl PlaudStatus {
    fn from_state(
        state: RecordingSource,
        available: bool,
        authenticated: bool,
        version: Option<String>,
    ) -> Result<Self> {
        let interval_minutes = state
            .interval_minutes
            .context("Configured Plaud recording source has no poll interval")?;
        Ok(Self {
            available,
            authenticated,
            version,
            enabled: state.enabled,
            interval_minutes,
            import_after: state.import_after,
            running: state.running,
            last_started_at: state.last_started_at,
            last_completed_at: state.last_completed_at,
            last_error: state.last_error,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recording_source(interval_minutes: Option<i64>) -> RecordingSource {
        RecordingSource {
            provider: PlaudSource::PROVIDER.to_string(),
            source_instance: PlaudSource::SOURCE_INSTANCE.to_string(),
            sync_mode: crate::db::integrations::SyncMode::Poll,
            enabled: true,
            interval_minutes,
            import_after: None,
            running: false,
            last_started_at: None,
            last_completed_at: None,
            last_error: None,
        }
    }

    #[test]
    fn token_parser_accepts_only_the_issued_shape() {
        let id = Uuid::new_v4();
        assert_eq!(
            token_id(&format!("{ACCESS_KEY_PREFIX}{id}_secret")),
            Some(id.to_string())
        );
        for invalid in [
            "wrong",
            "audetic_ingress_not-a-uuid_secret",
            "audetic_ingress_00000000-0000-0000-0000-000000000000_",
        ] {
            assert!(token_id(invalid).is_none(), "{invalid}");
        }
    }

    #[test]
    fn plaud_status_uses_the_stored_interval() {
        let status =
            PlaudStatus::from_state(recording_source(Some(30)), false, false, None).unwrap();
        assert_eq!(status.interval_minutes, 30);
        assert!(PlaudStatus::from_state(recording_source(None), false, false, None).is_err());
    }
}
