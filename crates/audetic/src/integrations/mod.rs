//! External audio integrations behind a small authenticated domain interface.

pub mod ingress;
pub mod plaud;

use anyhow::{Context, Result};
use audetic_core::url::HOST;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::Serialize;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::io::AsyncWriteExt;
use utoipa::ToSchema;
use uuid::Uuid;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::audio_notes::import::{import_audio_note_file, ImportArgs};
use crate::audio_notes::media_inspector::MediaInspector;
use crate::audio_notes::processing::ProcessingServices;
use crate::db::integrations::{
    ExternalImport, ImportClaim, IngressAccessKey, IntegrationRepository, PlaudSyncState,
};

const ACCESS_KEY_PREFIX: &str = "audetic_ingress_";
const MAX_PLAUD_AUDIO_BYTES: u64 = 1024 * 1024 * 1024;
const PLAUD_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);

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

    pub async fn recover_interrupted_work(&self) -> Result<()> {
        match tokio::fs::remove_dir_all(self.uploads_dir()).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("Failed to remove interrupted uploads"),
        }
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = crate::db::init_db_at(&db_path)?;
            IntegrationRepository::sweep_interrupted_imports(&conn)?;
            IntegrationRepository::sweep_interrupted_plaud_sync(&conn)
        })
        .await??;
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
        let db_path = self.db_path.clone();
        let id_for_db = id.clone();
        let scope_text = scope.as_str().to_string();
        let key = tokio::task::spawn_blocking(move || {
            let conn = crate::db::init_db_at(&db_path)?;
            IntegrationRepository::insert_access_key(&conn, &id_for_db, &name, &scope_text, &hash)
        })
        .await??;
        Ok(IssuedAccessKey {
            info: key.try_into()?,
            secret,
        })
    }

    pub async fn list_access_keys(&self) -> Result<Vec<AccessKeyInfo>> {
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = crate::db::init_db_at(&db_path)?;
            IntegrationRepository::list_access_keys(&conn)?
                .into_iter()
                .map(TryInto::try_into)
                .collect()
        })
        .await?
    }

    pub async fn revoke_access_key(&self, id: String) -> Result<Option<AccessKeyInfo>> {
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = crate::db::init_db_at(&db_path)?;
            IntegrationRepository::revoke_access_key(&conn, &id)?
                .map(TryInto::try_into)
                .transpose()
        })
        .await?
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
        let db_path = self.db_path.clone();
        let id_for_db = id.clone();
        let scope_text = scope.as_str().to_string();
        let authenticated = tokio::task::spawn_blocking(move || -> Result<bool> {
            let conn = crate::db::init_db_at(&db_path)?;
            let Some(stored) =
                IntegrationRepository::active_access_key_hash(&conn, &id_for_db, &scope_text)?
            else {
                return Ok(false);
            };
            if stored.len() != candidate.len()
                || stored.ct_eq(candidate.as_slice()).unwrap_u8() != 1
            {
                return Ok(false);
            }
            IntegrationRepository::touch_access_key(&conn, &id_for_db)?;
            Ok(true)
        })
        .await??;
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
        let db_path = self.db_path.clone();
        let claim_provider = provider.clone();
        let claim_instance = source_instance.clone();
        let claim_external_id = external_id.clone();
        let claim_recorded_at = recorded_at.clone();
        let claim_filename = source_filename.clone();
        let claim = tokio::task::spawn_blocking(move || {
            let conn = crate::db::init_db_at(&db_path)?;
            IntegrationRepository::claim_external_import(
                &conn,
                &claim_provider,
                &claim_instance,
                &claim_external_id,
                claim_recorded_at.as_deref(),
                claim_filename.as_deref(),
            )
        })
        .await??;

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

        let db_path = self.db_path.clone();
        match result {
            Ok(result) => {
                let note_id = result.note_id;
                Ok(ExternalImportOutcome::Accepted { note_id })
            }
            Err(error) => {
                remove_staged(&staged_path_for_cleanup).await;
                let public_error = error.to_string();
                let persisted_error = public_error.clone();
                tokio::task::spawn_blocking(move || {
                    let conn = crate::db::init_db_at(&db_path)?;
                    IntegrationRepository::fail_external_import(&conn, import_id, &persisted_error)
                })
                .await??;
                Err(error)
            }
        }
    }

    pub async fn recent_imports(&self, limit: usize) -> Result<Vec<ExternalImportInfo>> {
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = crate::db::init_db_at(&db_path)?;
            Ok(
                IntegrationRepository::recent_external_imports(&conn, limit)?
                    .into_iter()
                    .map(Into::into)
                    .collect(),
            )
        })
        .await?
    }

    pub async fn plaud_state(&self) -> Result<PlaudSyncState> {
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = crate::db::init_db_at(&db_path)?;
            IntegrationRepository::plaud_state(&conn)
        })
        .await?
    }

    pub async fn plaud_status(&self) -> Result<PlaudStatus> {
        let state = self.plaud_state().await?;
        let Some(cli) = plaud::PlaudCli::discover() else {
            return Ok(PlaudStatus::from_state(state, false, false, None));
        };
        let (version, authenticated) =
            tokio::join!(cli.version(), async { cli.authenticated().await });
        Ok(PlaudStatus::from_state(
            state,
            true,
            authenticated,
            version.ok(),
        ))
    }

    pub async fn update_plaud_settings(
        &self,
        enabled: bool,
        interval_minutes: i64,
        import_after: Option<String>,
    ) -> Result<PlaudStatus> {
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = crate::db::init_db_at(&db_path)?;
            IntegrationRepository::update_plaud_settings(
                &conn,
                enabled,
                interval_minutes,
                import_after.as_deref(),
            )
        })
        .await??;
        self.plaud_status().await
    }

    pub async fn schedule_plaud_sync(&self, backfill: bool) -> Result<bool> {
        let db_path = self.db_path.clone();
        let claimed = tokio::task::spawn_blocking(move || {
            let conn = crate::db::init_db_at(&db_path)?;
            IntegrationRepository::start_plaud_sync(&conn)
        })
        .await??;
        if !claimed {
            return Ok(false);
        }
        let service = self.clone();
        tokio::spawn(async move {
            let result = service.run_plaud_sync(backfill).await;
            if let Err(error) = &result {
                tracing::error!(%error, "Plaud synchronization failed");
            }
            let db_path = service.db_path.clone();
            let error = result.as_ref().err().map(|error| error.to_string());
            let finish_result = tokio::task::spawn_blocking(move || -> Result<()> {
                let conn = crate::db::init_db_at(&db_path)?;
                IntegrationRepository::finish_plaud_sync(&conn, error.as_deref())
            })
            .await;
            match finish_result {
                Ok(Ok(())) => {}
                Ok(Err(finish_error)) => {
                    tracing::error!(%finish_error, "failed to persist Plaud sync completion");
                }
                Err(finish_error) => {
                    tracing::error!(%finish_error, "Plaud sync completion task panicked");
                }
            }
        });
        Ok(true)
    }

    pub fn spawn_plaud_scheduler(&self) {
        let service = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let Ok(state) = service.plaud_state().await else {
                    continue;
                };
                if !state.enabled || state.running || !plaud_sync_is_due(&state) {
                    continue;
                }
                if let Err(error) = service.schedule_plaud_sync(false).await {
                    tracing::error!(%error, "Failed to schedule Plaud synchronization");
                }
            }
        });
    }

    async fn run_plaud_sync(&self, backfill: bool) -> Result<()> {
        let cli = plaud::PlaudCli::discover()
            .context("Plaud CLI is not installed; install @plaud-ai/cli and run `plaud login`")?;
        if !cli.authenticated().await {
            anyhow::bail!("Plaud CLI is not authenticated; run `plaud login`");
        }
        let state = self.plaud_state().await?;
        let import_after = if backfill {
            None
        } else {
            state
                .import_after
                .as_deref()
                .map(parse_db_timestamp)
                .transpose()?
        };
        let import_boundary = plaud_import_boundary(backfill, import_after, chrono::Utc::now());

        for page in 1..=1000 {
            let ids = cli.list_ids(page).await?;
            if ids.is_empty() {
                break;
            }
            let page_len = ids.len();
            let mut reached_boundary = false;
            for id in ids {
                let recording = cli.file(&id).await?;
                let recorded_at = recording.start_at.as_ref().unwrap_or(&recording.created_at);
                let recorded_at = parse_plaud_timestamp(recorded_at)
                    .with_context(|| format!("Plaud recording {id} has an invalid timestamp"))?;
                if import_boundary.is_some_and(|boundary| recorded_at < boundary) {
                    reached_boundary = true;
                    continue;
                }
                if self.external_imported("plaud", "default", &id).await? {
                    continue;
                }
                let url = cli.audio_url(&id).await?;
                let (staged_path, filename) = self.download_plaud_audio(&id, url).await?;
                self.import_external(ExternalImportRequest {
                    provider: "plaud".to_string(),
                    source_instance: "default".to_string(),
                    external_id: id,
                    recorded_at: Some(recorded_at.format("%Y-%m-%d %H:%M:%S").to_string()),
                    source_filename: Some(filename),
                    title: Some(recording.name),
                    staged_path,
                })
                .await?;
            }
            if reached_boundary || page_len < 100 {
                break;
            }
            if page == 1000 {
                anyhow::bail!("Plaud synchronization exceeded the 100,000-recording safety limit");
            }
        }
        if let Some(next_cursor) = import_boundary {
            let db_path = self.db_path.clone();
            let next_cursor = next_cursor.format("%Y-%m-%d %H:%M:%S").to_string();
            tokio::task::spawn_blocking(move || {
                let conn = crate::db::init_db_at(&db_path)?;
                IntegrationRepository::update_plaud_cursor(&conn, &next_cursor)
            })
            .await??;
        }
        Ok(())
    }

    async fn external_imported(
        &self,
        provider: &str,
        source_instance: &str,
        external_id: &str,
    ) -> Result<bool> {
        let db_path = self.db_path.clone();
        let provider = provider.to_string();
        let source_instance = source_instance.to_string();
        let external_id = external_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = crate::db::init_db_at(&db_path)?;
            Ok(IntegrationRepository::accepted_external_import_note_id(
                &conn,
                &provider,
                &source_instance,
                &external_id,
            )?
            .is_some())
        })
        .await?
    }

    async fn download_plaud_audio(&self, id: &str, url: reqwest::Url) -> Result<(PathBuf, String)> {
        anyhow::ensure!(url.scheme() == "https", "Plaud audio URL must use HTTPS");
        anyhow::ensure!(
            url.username().is_empty(),
            "Plaud audio URL must not contain user information"
        );
        let extension = Path::new(url.path())
            .extension()
            .and_then(|extension| extension.to_str())
            .filter(|extension| {
                audetic_core::jobs_client::mime_type_for_extension(extension).is_some()
            })
            .unwrap_or("m4a")
            .to_ascii_lowercase();
        let filename = format!("plaud-{id}.{extension}");
        let uploads_dir = self.uploads_dir();
        tokio::fs::create_dir_all(&uploads_dir).await?;
        let path = uploads_dir.join(format!("plaud-{}", Uuid::new_v4().simple()));
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(PLAUD_DOWNLOAD_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let mut response = client.get(url).send().await?.error_for_status()?;
        if response
            .content_length()
            .is_some_and(|length| length > MAX_PLAUD_AUDIO_BYTES)
        {
            anyhow::bail!("Plaud audio exceeds the 1 GiB limit");
        }
        let download_result = async {
            let mut file = tokio::fs::File::create(&path).await?;
            let mut written = 0_u64;
            while let Some(chunk) = response.chunk().await? {
                written = written.saturating_add(chunk.len() as u64);
                anyhow::ensure!(
                    written <= MAX_PLAUD_AUDIO_BYTES,
                    "Plaud audio exceeds the 1 GiB limit"
                );
                file.write_all(&chunk).await?;
            }
            file.flush().await?;
            anyhow::ensure!(written > 0, "Plaud audio download was empty");
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if let Err(error) = download_result {
            remove_staged(&path).await;
            return Err(error);
        }
        Ok((path, filename))
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

async fn remove_staged(path: &Path) {
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
        state: PlaudSyncState,
        available: bool,
        authenticated: bool,
        version: Option<String>,
    ) -> Self {
        Self {
            available,
            authenticated,
            version,
            enabled: state.enabled,
            interval_minutes: state.interval_minutes,
            import_after: state.import_after,
            running: state.running,
            last_started_at: state.last_started_at,
            last_completed_at: state.last_completed_at,
            last_error: state.last_error,
        }
    }
}

fn parse_db_timestamp(value: &str) -> Result<chrono::DateTime<chrono::Utc>> {
    let naive = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")?;
    Ok(naive.and_utc())
}

fn parse_plaud_timestamp(value: &str) -> Result<chrono::DateTime<chrono::Utc>> {
    if let Ok(timestamp) = chrono::DateTime::parse_from_rfc3339(value) {
        return Ok(timestamp.with_timezone(&chrono::Utc));
    }
    let naive = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f")?;
    Ok(naive.and_utc())
}

fn plaud_import_boundary(
    backfill: bool,
    import_after: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    if backfill {
        return None;
    }
    let overlap_boundary = now - chrono::Duration::hours(24);
    Some(
        import_after
            .map(|boundary| boundary.max(overlap_boundary))
            .unwrap_or(overlap_boundary),
    )
}

fn plaud_sync_is_due(state: &PlaudSyncState) -> bool {
    let Some(last_started) = state.last_started_at.as_deref() else {
        return true;
    };
    let Ok(last_started) = parse_db_timestamp(last_started) else {
        return true;
    };
    chrono::Utc::now() - last_started >= chrono::Duration::minutes(state.interval_minutes)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn plaud_timestamp_parser_accepts_official_naive_utc_format() {
        assert_eq!(
            parse_plaud_timestamp("2026-09-30T13:01:11").unwrap(),
            chrono::DateTime::parse_from_rfc3339("2026-09-30T13:01:11Z")
                .unwrap()
                .with_timezone(&chrono::Utc)
        );
        assert_eq!(
            parse_plaud_timestamp("2026-09-12T13:35:45.446000").unwrap(),
            chrono::DateTime::parse_from_rfc3339("2026-09-12T13:35:45.446000Z")
                .unwrap()
                .with_timezone(&chrono::Utc)
        );
    }

    #[test]
    fn first_incremental_plaud_sync_only_scans_the_last_day() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-01T16:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(
            plaud_import_boundary(false, None, now),
            Some(now - chrono::Duration::hours(24))
        );
        assert_eq!(
            plaud_import_boundary(false, Some(now - chrono::Duration::hours(48)), now),
            Some(now - chrono::Duration::hours(24))
        );
        assert_eq!(plaud_import_boundary(true, None, now), None);
    }
}
