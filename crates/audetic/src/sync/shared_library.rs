//! Deep daemon-domain boundary for every local/shared library operation.
//!
//! HTTP handlers deliberately see intent-level requests and presentation
//! results only. Role routing, source precedence, persistence, and remote
//! transport selection stay behind this module.

use anyhow::{Context, Result};
use audetic_core::sync::{DeviceId, PayloadAvailability, RecordId, UploadState};
use futures_util::TryStreamExt;

use std::collections::BTreeMap;
use std::io::SeekFrom;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::db::shared_library::SharedLibraryRepository;
use crate::db::sync_outbox::SyncOutboxRepository;
use crate::history::{HistoryEntry, HistorySource, SearchParams};
use crate::meeting_artifacts::GenerateArtifactRequest;

use super::transition::{LibraryContext, LibraryObservation, LibraryRole, RoleCoordinator};
use super::transport::{
    PayloadBody, PayloadContentRange, PayloadMetadata, RemoteDictationLibrary,
    RemoteMeetingLibrary, StreamingPayloadResponse,
};

#[derive(Clone, Debug)]
pub struct MeetingPageRequest {
    pub query: Option<String>,
    pub offset: usize,
    pub limit: usize,
}

#[derive(Clone, Debug)]
pub struct PayloadRequest {
    pub id: RecordId,
    pub kind: super::protocol::RecordKind,
    pub range: Option<String>,
}

pub struct LibraryPayload {
    pub status: u16,
    pub metadata: PayloadMetadata,
    pub body: PayloadBody,
}

#[derive(Clone, Debug)]
pub struct MeetingTitleResult {
    pub meeting_id: RecordId,
    pub title: Option<String>,
    pub title_source: Option<String>,
    pub local_id: Option<i64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeleteResult {
    Deleted { local_id: Option<i64> },
    NotFound,
    InFlight,
}

#[derive(Debug)]
pub enum RetryMeetingResult {
    Ready {
        local_id: i64,
        record_id: RecordId,
        audio_path: PathBuf,
        duration_seconds: i64,
    },
    NotFound,
    WrongState(String),
    MissingAudio(String),
    StateChanged,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtifactDeleteResult {
    Deleted,
    NotFound,
    InFlight,
}

impl From<StreamingPayloadResponse> for LibraryPayload {
    fn from(value: StreamingPayloadResponse) -> Self {
        Self {
            status: value.status,
            metadata: value.metadata,
            body: value.body,
        }
    }
}

#[derive(Clone)]
pub struct SharedLibraryService {
    coordinator: RoleCoordinator,
    standalone_only: bool,
}

impl SharedLibraryService {
    pub(super) fn new(coordinator: RoleCoordinator) -> Self {
        Self {
            coordinator,
            standalone_only: false,
        }
    }

    pub(super) fn standalone(coordinator: RoleCoordinator) -> Self {
        Self {
            coordinator,
            standalone_only: true,
        }
    }

    fn context(&self) -> Result<LibraryContext> {
        let mut context = self
            .coordinator
            .library_context()
            .map_err(anyhow::Error::new)?;
        if self.standalone_only {
            context.role = LibraryRole::Standalone;
        }
        Ok(context)
    }

    pub async fn dictations(&self, params: &SearchParams) -> Result<Vec<HistoryEntry>> {
        let context = self.context()?;
        let result =
            DictationQueries::new(context.db_path.clone(), context.capabilities.dictations())
                .read(&context.role, params)
                .await?;
        self.observe(&context, result.hub_reachable, result.error.as_deref())
            .await?;
        Ok(result.entries)
    }

    pub async fn dictation(&self, id: RecordId) -> Result<Option<HistoryEntry>> {
        let mut offset = 0usize;
        loop {
            let mut params = SearchParams::new().with_limit(100);
            params.offset = offset;
            let page = self.dictations(&params).await?;
            if let Some(entry) = page.iter().find(|entry| entry.id == id) {
                return Ok(Some(entry.clone()));
            }
            if page.len() < 100 {
                return Ok(None);
            }
            offset = offset.saturating_add(page.len());
        }
    }

    pub async fn meetings(&self, request: MeetingPageRequest) -> Result<Vec<LibraryMeeting>> {
        let context = self.context()?;
        let result = MeetingQueries::new(context.db_path.clone(), context.capabilities.meetings())
            .read(
                &context.role,
                request.query.as_deref(),
                request.offset,
                request.limit,
            )
            .await?;
        self.observe(&context, result.hub_reachable, result.error.as_deref())
            .await?;
        Ok(result.meetings)
    }

    pub async fn meeting(&self, id: RecordId) -> Result<Option<LibraryMeeting>> {
        let context = self.context()?;
        let connection = crate::db::open_db_at(&context.db_path)?;
        if SyncOutboxRepository::deletion_masks(
            &connection,
            id,
            super::protocol::RecordKind::Meeting,
        )? {
            return Ok(None);
        }
        let local = crate::db::meetings::MeetingRepository::get_by_sync_id(&connection, id)?
            .map(|meeting| {
                let upload = SyncOutboxRepository::state_for_kind(
                    &connection,
                    meeting.sync_id,
                    super::protocol::RecordKind::Meeting,
                )?;
                let payload =
                    SyncOutboxRepository::payload_availability(&connection, meeting.sync_id)?;
                Ok::<_, anyhow::Error>(local_meeting(meeting, upload, payload))
            })
            .transpose()?;
        match &context.role {
            LibraryRole::Standalone => Ok(local),
            LibraryRole::HomeHub => {
                self.observe(&context, true, None).await?;
                Ok(SharedLibraryRepository::get_meeting(&connection, id)?
                    .map(shared_meeting)
                    .map(|shared| overlay_local_payload(local.as_ref(), shared))
                    .or(local))
            }
            LibraryRole::ConnectedDevice { hub } => {
                match context.capabilities.meetings().meeting(hub, id).await {
                    Ok(Some(shared)) => {
                        self.observe(&context, true, None).await?;
                        Ok(Some(overlay_local_payload(
                            local.as_ref(),
                            shared_meeting(shared),
                        )))
                    }
                    Ok(None) => {
                        self.observe(&context, true, None).await?;
                        Ok(local)
                    }
                    Err(error) => {
                        self.observe(&context, false, Some(&error.to_string()))
                            .await?;
                        let read_only = local
                            .as_ref()
                            .map(|meeting| {
                                SyncOutboxRepository::may_have_reached_hub(
                                    &connection,
                                    meeting.id,
                                    super::protocol::RecordKind::Meeting,
                                )
                            })
                            .transpose()?
                            .unwrap_or(false);
                        Ok(local.map(|mut meeting| {
                            meeting.access = if read_only {
                                LibraryItemAccess::LocalOfflineReadOnly
                            } else {
                                LibraryItemAccess::LocalOffline
                            };
                            meeting
                        }))
                    }
                }
            }
        }
    }

    pub async fn update_shared_title(
        &self,
        id: RecordId,
        title: String,
        expected_title_version: u64,
        title_source: Option<String>,
    ) -> Result<super::protocol::SharedMeeting> {
        let context = self.context()?;
        let patch = super::protocol::MeetingTitlePatch {
            title,
            expected_title_version,
            title_source,
        };
        match &context.role {
            LibraryRole::Standalone => anyhow::bail!("meeting is not shared"),
            LibraryRole::HomeHub => super::library::HubLibrary::new(context.db_path)
                .update_meeting_title(id, &patch)?
                .context("meeting not found"),
            LibraryRole::ConnectedDevice { hub } => context
                .capabilities
                .mutations()
                .update_meeting_title(hub, id, patch)
                .await
                .map_err(anyhow::Error::new),
        }
    }

    pub async fn update_meeting_title(
        &self,
        id: RecordId,
        title: String,
    ) -> Result<MeetingTitleResult> {
        let title = title.trim().to_owned();
        if title.is_empty() {
            anyhow::bail!("Meeting Title cannot be blank");
        }
        let current = self.meeting(id).await?.context("meeting not found")?;
        if current.access == LibraryItemAccess::Shared {
            let updated = self
                .update_shared_title(id, title, current.title_version, None)
                .await?;
            return Ok(MeetingTitleResult {
                meeting_id: id,
                title: updated.title,
                title_source: updated.title_source,
                local_id: None,
            });
        }
        if current.access.read_only() {
            anyhow::bail!("Home Hub is unavailable; shared title edits are not queued offline");
        }
        let local_id = current
            .local_id
            .context("local meeting has no row identity")?;
        let context = self.context()?;
        let connection = crate::db::open_db_at(&context.db_path)?;
        if !crate::db::meetings::MeetingRepository::set_manual_title(&connection, local_id, &title)?
        {
            anyhow::bail!("meeting not found");
        }
        let updated = crate::db::meetings::MeetingRepository::get(&connection, local_id)?
            .context("meeting not found")?;
        Ok(MeetingTitleResult {
            meeting_id: id,
            title: updated.title,
            title_source: updated.title_source,
            local_id: Some(local_id),
        })
    }

    pub async fn regenerate_meeting_title(&self, id: RecordId) -> Result<Option<i64>> {
        let meeting = self.meeting(id).await?.context("meeting not found")?;
        if meeting.access.read_only() {
            anyhow::bail!("Home Hub is unavailable; generated titles are not queued offline");
        }
        if meeting.access == LibraryItemAccess::Shared {
            let transcript = meeting
                .transcript_text
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .context("meeting has no transcript")?;
            let context = self.context()?;
            let generated = crate::meeting::title::generate_shared_meeting_title(
                id,
                transcript,
                &context.db_path,
            )
            .await?;
            self.update_shared_title(
                id,
                generated,
                meeting.title_version,
                Some("generated".into()),
            )
            .await?;
            return Ok(None);
        }
        let local_id = meeting
            .local_id
            .context("local meeting has no row identity")?;
        let db_path = self.context()?.db_path;
        crate::meeting::title::prepare_title_regeneration_at(&db_path, local_id)?;
        crate::meeting::title::spawn_title_generation_at(local_id, db_path);
        Ok(Some(local_id))
    }

    pub fn public_meeting_id(&self, local_id: i64) -> Result<RecordId> {
        let context = self.context()?;
        let connection = crate::db::open_db_at(&context.db_path)?;
        crate::db::meetings::MeetingRepository::get(&connection, local_id)?
            .map(|meeting| meeting.sync_id)
            .with_context(|| format!("meeting {local_id} not found"))
    }

    pub fn recent_meeting_titles(&self, limit: usize) -> Result<Vec<String>> {
        let context = self.context()?;
        let connection = crate::db::open_db_at(&context.db_path)?;
        crate::db::meetings::MeetingRepository::recent_manual_titles(&connection, limit)
    }

    pub async fn prepare_meeting_retry(&self, id: RecordId) -> Result<RetryMeetingResult> {
        let db_path = self.context()?.db_path;
        tokio::task::spawn_blocking(move || {
            let connection = crate::db::open_db_at(&db_path)?;
            let Some(meeting) =
                crate::db::meetings::MeetingRepository::get_by_sync_id(&connection, id)?
            else {
                return Ok(RetryMeetingResult::NotFound);
            };
            if meeting.status != crate::meeting::MeetingPhase::Error.as_str() {
                return Ok(RetryMeetingResult::WrongState(meeting.status));
            }

            let stored_path = PathBuf::from(&meeting.audio_path);
            let audio_path = if stored_path.exists() {
                stored_path
            } else {
                let mp3_sibling = stored_path.with_extension("mp3");
                if !mp3_sibling.exists() {
                    return Ok(RetryMeetingResult::MissingAudio(meeting.audio_path));
                }
                crate::db::meetings::MeetingRepository::update_audio_path(
                    &connection,
                    meeting.id,
                    mp3_sibling.to_string_lossy().as_ref(),
                )?;
                mp3_sibling
            };

            if !crate::db::meetings::MeetingRepository::begin_retry(&connection, meeting.id)? {
                return Ok(RetryMeetingResult::StateChanged);
            }
            Ok(RetryMeetingResult::Ready {
                local_id: meeting.id,
                record_id: id,
                audio_path,
                duration_seconds: meeting.duration_seconds.unwrap_or(0),
            })
        })
        .await
        .context("meeting retry preparation task panicked")?
    }

    pub async fn artifacts(
        &self,
        meeting_id: RecordId,
    ) -> Result<Vec<crate::db::meeting_artifacts::MeetingArtifact>> {
        let meeting = self
            .meeting(meeting_id)
            .await?
            .context("meeting not found")?;
        let context = self.context()?;
        let connection = crate::db::open_db_at(&context.db_path)?;
        if meeting.access == LibraryItemAccess::Shared {
            let mut artifacts = meeting
                .artifacts
                .into_iter()
                .filter(|artifact| {
                    !SyncOutboxRepository::deletion_masks(
                        &connection,
                        artifact.record_id,
                        super::protocol::RecordKind::Artifact,
                    )
                    .unwrap_or(false)
                })
                .map(shared_artifact)
                .map(|artifact| (artifact.id, artifact))
                .collect::<BTreeMap<_, _>>();
            for artifact in
                crate::db::meeting_artifacts::MeetingArtifactRepository::list_shared_runs(
                    &connection,
                    meeting_id,
                )?
            {
                artifacts.insert(artifact.id, artifact);
            }
            let mut artifacts = artifacts.into_values().collect::<Vec<_>>();
            artifacts.sort_by(|left, right| right.created_at.cmp(&left.created_at));
            return Ok(artifacts);
        }
        let local_id = meeting
            .local_id
            .context("local meeting has no row identity")?;
        crate::db::meeting_artifacts::MeetingArtifactRepository::list_for_live_meeting(
            &connection,
            local_id,
        )
    }

    pub async fn artifact(
        &self,
        meeting_id: RecordId,
        artifact_id: RecordId,
    ) -> Result<Option<crate::db::meeting_artifacts::MeetingArtifact>> {
        Ok(self
            .artifacts(meeting_id)
            .await?
            .into_iter()
            .find(|artifact| artifact.id == artifact_id))
    }

    pub async fn generate_artifact(
        &self,
        meeting_id: RecordId,
        request: GenerateArtifactRequest,
    ) -> Result<crate::db::meeting_artifacts::MeetingArtifact> {
        let meeting = self
            .meeting(meeting_id)
            .await?
            .context("meeting not found")?;
        if meeting.access.read_only() {
            anyhow::bail!("Home Hub is unavailable; shared artifacts are not queued offline");
        }
        let context = self.context()?;
        if meeting.access == LibraryItemAccess::Shared {
            return crate::meeting_artifacts::generate_shared_meeting_artifact(
                &context.db_path,
                meeting_id,
                meeting.title.as_deref(),
                meeting
                    .transcript_text
                    .as_deref()
                    .context("meeting has no transcript")?,
                request,
            )
            .await;
        }
        crate::meeting_artifacts::generate_meeting_artifact_at(
            &context.db_path,
            meeting
                .local_id
                .context("local meeting has no row identity")?,
            request,
        )
        .await
    }

    pub async fn delete_artifact(
        &self,
        meeting_id: RecordId,
        artifact_id: RecordId,
    ) -> Result<ArtifactDeleteResult> {
        use crate::db::meeting_artifacts::{ArtifactDeleteOutcome, MeetingArtifactRepository};

        let meeting = self
            .meeting(meeting_id)
            .await?
            .context("meeting not found")?;
        let context = self.context()?;
        let connection = crate::db::open_db_at(&context.db_path)?;
        let local = MeetingArtifactRepository::get_by_sync_id(&connection, artifact_id)?
            .filter(|artifact| artifact.meeting_id == meeting_id);
        let shared_run = MeetingArtifactRepository::list_shared_runs(&connection, meeting_id)?
            .into_iter()
            .find(|artifact| artifact.id == artifact_id);
        let exists = local.is_some()
            || shared_run.is_some()
            || meeting
                .artifacts
                .iter()
                .any(|artifact| artifact.record_id == artifact_id);
        if !exists {
            return Ok(ArtifactDeleteResult::NotFound);
        }
        let status = local
            .as_ref()
            .map(|artifact| artifact.status)
            .or_else(|| shared_run.as_ref().map(|artifact| artifact.status));
        if status.is_some_and(|status| {
            matches!(
                status,
                crate::db::meeting_artifacts::ArtifactStatus::Pending
                    | crate::db::meeting_artifacts::ArtifactStatus::Running
            )
        }) {
            return Ok(ArtifactDeleteResult::InFlight);
        }
        if meeting.access == LibraryItemAccess::Shared {
            self.delete_shared_record(artifact_id, super::protocol::RecordKind::Artifact)
                .await?;
            if let Some(local) = local {
                MeetingArtifactRepository::delete_for_live_meeting(
                    &connection,
                    local.local_meeting_id,
                    local.local_id,
                )?;
            }
            MeetingArtifactRepository::cleanup_after_shared_delete(
                &connection,
                meeting_id,
                artifact_id,
            )?;
            return Ok(ArtifactDeleteResult::Deleted);
        }
        if meeting.access.read_only() {
            anyhow::bail!(
                "Home Hub is unavailable; shared artifact deletions are not queued offline"
            );
        }
        let local = local.context("artifact not found")?;
        match MeetingArtifactRepository::delete_for_live_meeting_guarded(
            &connection,
            local.local_meeting_id,
            local.local_id,
        )? {
            ArtifactDeleteOutcome::Deleted => Ok(ArtifactDeleteResult::Deleted),
            ArtifactDeleteOutcome::NotFound => Ok(ArtifactDeleteResult::NotFound),
            ArtifactDeleteOutcome::InFlight => Ok(ArtifactDeleteResult::InFlight),
            ArtifactDeleteOutcome::RequiresHub => {
                self.delete_shared_record(artifact_id, super::protocol::RecordKind::Artifact)
                    .await?;
                if !MeetingArtifactRepository::delete_for_live_meeting(
                    &connection,
                    local.local_meeting_id,
                    local.local_id,
                )? {
                    anyhow::bail!("artifact disappeared during local cleanup");
                }
                Ok(ArtifactDeleteResult::Deleted)
            }
        }
    }

    pub async fn delete_shared_record(
        &self,
        id: RecordId,
        kind: super::protocol::RecordKind,
    ) -> Result<()> {
        let context = self.context()?;
        match &context.role {
            LibraryRole::Standalone => anyhow::bail!("record is not shared"),
            LibraryRole::HomeHub => super::library::HubLibrary::new(context.db_path)
                .delete(id, kind)
                .map(|_| ())
                .map_err(anyhow::Error::new),
            LibraryRole::ConnectedDevice { hub } => context
                .capabilities
                .mutations()
                .delete_record(hub, id, kind)
                .await
                .map_err(anyhow::Error::new),
        }
    }

    pub async fn delete_meeting(&self, id: RecordId) -> Result<DeleteResult> {
        use crate::db::meetings::{MeetingRepository, SoftDeleteOutcome};

        let meeting = match self.meeting(id).await? {
            Some(meeting) => meeting,
            None => return Ok(DeleteResult::NotFound),
        };
        if !crate::meeting::MeetingPhase::is_terminal(&meeting.status) {
            return Ok(DeleteResult::InFlight);
        }
        if meeting.access.read_only() {
            anyhow::bail!("Home Hub is unavailable; shared deletions are not queued offline");
        }
        let context = self.context()?;
        let connection = crate::db::open_db_at(&context.db_path)?;
        if meeting.access == LibraryItemAccess::Shared {
            self.delete_shared_record(id, super::protocol::RecordKind::Meeting)
                .await?;
            if let Some(local_id) = meeting.local_id {
                return match MeetingRepository::soft_delete_after_hub_delete(&connection, local_id)?
                {
                    SoftDeleteOutcome::Deleted | SoftDeleteOutcome::NotFound => {
                        Ok(DeleteResult::Deleted {
                            local_id: Some(local_id),
                        })
                    }
                    SoftDeleteOutcome::InFlight | SoftDeleteOutcome::RequiresHub => {
                        anyhow::bail!("local meeting cleanup was refused")
                    }
                };
            }
            MeetingRepository::cleanup_deleted_sync_work(&connection, id)?;
            return Ok(DeleteResult::Deleted { local_id: None });
        }
        let local_id = meeting
            .local_id
            .context("local meeting has no row identity")?;
        match MeetingRepository::soft_delete(&connection, local_id)? {
            SoftDeleteOutcome::Deleted => Ok(DeleteResult::Deleted {
                local_id: Some(local_id),
            }),
            SoftDeleteOutcome::NotFound => Ok(DeleteResult::NotFound),
            SoftDeleteOutcome::InFlight => Ok(DeleteResult::InFlight),
            SoftDeleteOutcome::RequiresHub => {
                anyhow::bail!("Home Hub deletion is required")
            }
        }
    }

    pub async fn payload(&self, request: PayloadRequest) -> Result<Option<LibraryPayload>> {
        let context = self.context()?;
        if let Some(path) = operational_payload_path(&context.db_path, request.id, request.kind)? {
            return open_local_payload(path, request.range.as_deref())
                .await
                .map(Some);
        }
        match &context.role {
            LibraryRole::Standalone => Ok(None),
            LibraryRole::HomeHub => {
                let blob = super::library::HubLibrary::new(context.db_path)
                    .payload(request.id, request.kind)?;
                match blob {
                    Some(blob) => open_local_payload(blob.canonical_path, request.range.as_deref())
                        .await
                        .map(Some),
                    None => Ok(None),
                }
            }
            LibraryRole::ConnectedDevice { hub } => context
                .capabilities
                .payloads()
                .stream_payload(hub, request.id, request.kind, request.range.as_deref())
                .await
                .map(LibraryPayload::from)
                .map(Some)
                .map_err(anyhow::Error::new),
        }
    }

    async fn observe(
        &self,
        context: &LibraryContext,
        reachable: bool,
        error: Option<&str>,
    ) -> Result<()> {
        let observation = if reachable {
            LibraryObservation::Reachable
        } else {
            LibraryObservation::Unreachable(error.unwrap_or("Home Hub unavailable").to_owned())
        };
        self.coordinator
            .record_library_observation(context, observation)
            .await
            .map_err(anyhow::Error::new)
    }
}

fn shared_artifact(
    value: super::protocol::SharedArtifact,
) -> crate::db::meeting_artifacts::MeetingArtifact {
    crate::db::meeting_artifacts::MeetingArtifact {
        id: value.record_id,
        meeting_id: value.parent_record_id,
        local_id: 0,
        local_meeting_id: 0,
        origin_device_id: value.origin_device_id,
        sync_version: value.local_version,
        kind: value.artifact_kind,
        title: value.title,
        template_id: value.template_id,
        agent_profile_id: None,
        status: crate::db::meeting_artifacts::ArtifactStatus::Completed,
        content_markdown: Some(value.content_markdown),
        error: None,
        stdout: None,
        stderr: None,
        created_at: value.created_at,
        updated_at: value.updated_at,
        completed_at: Some(value.completed_at),
    }
}

fn operational_payload_path(
    db_path: &std::path::Path,
    id: RecordId,
    kind: super::protocol::RecordKind,
) -> Result<Option<PathBuf>> {
    let connection = crate::db::open_db_at(db_path)?;
    let stored = match kind {
        super::protocol::RecordKind::Dictation => {
            crate::db::get_workflow_by_sync_id(&connection, id)?.map(|workflow| {
                match workflow.data {
                    crate::db::WorkflowData::VoiceToText(data) => data.audio_path,
                }
            })
        }
        super::protocol::RecordKind::Meeting => {
            crate::db::meetings::MeetingRepository::get_by_sync_id(&connection, id)?
                .map(|meeting| meeting.audio_path)
        }
        super::protocol::RecordKind::Artifact => None,
    };
    stored
        .map(|path| super::payload::resolve_operational_audio(std::path::Path::new(&path)))
        .transpose()
        .map(Option::flatten)
        .map_err(Into::into)
}

async fn open_local_payload(path: PathBuf, range: Option<&str>) -> Result<LibraryPayload> {
    let mut file = tokio::fs::File::open(&path)
        .await
        .with_context(|| format!("opening Recording Payload {}", path.display()))?;
    let complete_length = file.metadata().await?.len();
    let content_type = http::HeaderValue::from_str(&super::payload::media_type_for(&path)).ok();
    let accept_ranges = Some(http::HeaderValue::from_static("bytes"));
    let (status, content_length, content_range, body): (u16, u64, Option<_>, PayloadBody) =
        match requested_range(range, complete_length) {
            RequestedRange::Full => {
                let body = tokio_util::io::ReaderStream::new(file).map_err(|error| {
                    super::transport::HubTransferError::Transport(error.to_string())
                });
                (200, complete_length, None, Box::pin(body))
            }
            RequestedRange::Bytes { start, end } => {
                file.seek(SeekFrom::Start(start)).await?;
                let length = end - start + 1;
                let body = tokio_util::io::ReaderStream::new(file.take(length)).map_err(|error| {
                    super::transport::HubTransferError::Transport(error.to_string())
                });
                (
                    206,
                    length,
                    Some(PayloadContentRange::Bytes {
                        start,
                        end,
                        complete_length,
                    }),
                    Box::pin(body),
                )
            }
            RequestedRange::Unsatisfied => (
                416,
                0,
                Some(PayloadContentRange::Unsatisfied { complete_length }),
                Box::pin(futures_util::stream::empty()),
            ),
        };
    Ok(LibraryPayload {
        status,
        metadata: PayloadMetadata {
            content_type,
            content_length: Some(content_length),
            content_range,
            accept_ranges,
        },
        body,
    })
}

enum RequestedRange {
    Full,
    Bytes { start: u64, end: u64 },
    Unsatisfied,
}

fn requested_range(value: Option<&str>, length: u64) -> RequestedRange {
    let Some(value) = value.and_then(|value| value.strip_prefix("bytes=")) else {
        return RequestedRange::Full;
    };
    if value.contains(',') || length == 0 {
        return RequestedRange::Unsatisfied;
    }
    let Some((start, end)) = value.split_once('-') else {
        return RequestedRange::Unsatisfied;
    };
    if start.is_empty() {
        let Ok(suffix) = end.parse::<u64>() else {
            return RequestedRange::Unsatisfied;
        };
        if suffix == 0 {
            return RequestedRange::Unsatisfied;
        }
        let start = length.saturating_sub(suffix);
        return RequestedRange::Bytes {
            start,
            end: length - 1,
        };
    }
    let Ok(start) = start.parse::<u64>() else {
        return RequestedRange::Unsatisfied;
    };
    if start >= length {
        return RequestedRange::Unsatisfied;
    }
    let end = if end.is_empty() {
        length - 1
    } else {
        match end.parse::<u64>() {
            Ok(end) if end >= start => end.min(length - 1),
            _ => return RequestedRange::Unsatisfied,
        }
    };
    RequestedRange::Bytes { start, end }
}

struct LibraryReadResult {
    pub entries: Vec<HistoryEntry>,
    pub hub_reachable: bool,
    pub error: Option<String>,
}

struct DictationQueries {
    db_path: PathBuf,
    remote: Arc<dyn RemoteDictationLibrary>,
}

#[derive(Clone, Debug)]
pub struct LibraryMeeting {
    pub id: RecordId,
    pub local_id: Option<i64>,
    pub origin_device_id: DeviceId,
    pub title: Option<String>,
    pub title_source: Option<String>,
    pub title_version: u64,
    pub source_filename: Option<String>,
    pub status: String,
    pub transcript_text: Option<String>,
    pub transcript_segments: Option<Vec<audetic_core::jobs_client::Segment>>,
    pub duration_seconds: Option<i64>,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub error: Option<String>,
    pub created_at: String,
    pub upload_state: Option<UploadState>,
    pub payload_availability: PayloadAvailability,
    pub access: LibraryItemAccess,
    pub artifacts: Vec<super::protocol::SharedArtifact>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LibraryItemAccess {
    Local,
    Shared,
    LocalOffline,
    LocalOfflineReadOnly,
}

impl LibraryItemAccess {
    pub const fn source(self) -> &'static str {
        match self {
            Self::Local | Self::LocalOffline | Self::LocalOfflineReadOnly => "local",
            Self::Shared => "shared",
        }
    }

    pub const fn offline(self) -> bool {
        matches!(self, Self::LocalOffline | Self::LocalOfflineReadOnly)
    }

    pub const fn read_only(self) -> bool {
        matches!(self, Self::LocalOfflineReadOnly)
    }
}

struct MeetingQueries {
    db_path: PathBuf,
    remote: Arc<dyn RemoteMeetingLibrary>,
}
impl MeetingQueries {
    pub fn new(db_path: PathBuf, remote: Arc<dyn RemoteMeetingLibrary>) -> Self {
        Self { db_path, remote }
    }
    pub async fn read(
        &self,
        role: &LibraryRole,
        query: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<LibraryReadResultMeetings> {
        let connection = crate::db::open_db_at(&self.db_path)?;
        let limit = limit.clamp(1, super::protocol::MAX_MEETING_PAGE);
        let fetch = offset.saturating_add(limit);
        let mut entries = BTreeMap::new();
        let local_fetch = if query.is_some() { usize::MAX } else { fetch };
        for meeting in crate::db::meetings::MeetingRepository::list(&connection, local_fetch)? {
            if query.is_some_and(|query| {
                !contains_case_insensitive(meeting.title.as_deref().unwrap_or(""), query)
                    && !contains_case_insensitive(
                        meeting.transcript_text.as_deref().unwrap_or(""),
                        query,
                    )
            }) {
                continue;
            }
            let upload = SyncOutboxRepository::state_for_kind(
                &connection,
                meeting.sync_id,
                super::protocol::RecordKind::Meeting,
            )?;
            let payload = SyncOutboxRepository::payload_availability(&connection, meeting.sync_id)?;
            entries.insert(meeting.sync_id, local_meeting(meeting, upload, payload));
        }
        let (reachable, error) = match role {
            LibraryRole::Standalone => (false, None),
            LibraryRole::HomeHub => {
                for meeting in
                    SharedLibraryRepository::page_meetings(&connection, query, None, fetch)?
                {
                    let id = meeting.record_id;
                    if SyncOutboxRepository::deletion_masks(
                        &connection,
                        id,
                        super::protocol::RecordKind::Meeting,
                    )? {
                        continue;
                    }
                    let shared = shared_meeting(meeting);
                    let shared = overlay_local_payload(entries.get(&id), shared);
                    entries.insert(id, shared);
                }
                (true, None)
            }
            LibraryRole::ConnectedDevice { hub } => {
                let mut cursor = None;
                let mut fetched = 0;
                let mut failure = None;
                loop {
                    match self
                        .remote
                        .page_meetings(
                            hub,
                            query,
                            cursor.as_deref(),
                            fetch
                                .saturating_sub(fetched)
                                .clamp(1, super::protocol::MAX_MEETING_PAGE),
                        )
                        .await
                    {
                        Ok(page) => {
                            let len = page.items.len();
                            fetched += len;
                            for meeting in page.items {
                                let id = meeting.record_id;
                                if SyncOutboxRepository::deletion_masks(
                                    &connection,
                                    id,
                                    super::protocol::RecordKind::Meeting,
                                )? {
                                    continue;
                                }
                                let shared = shared_meeting(meeting);
                                let shared = overlay_local_payload(entries.get(&id), shared);
                                entries.insert(id, shared);
                            }
                            cursor = page.next_cursor;
                            if fetched >= fetch || cursor.is_none() || len == 0 {
                                break;
                            }
                        }
                        Err(err) => {
                            failure = Some(err.to_string());
                            break;
                        }
                    }
                }
                if failure.is_some() {
                    entries.retain(|_, value| value.access.source() == "local");
                    for value in entries.values_mut() {
                        value.access = if SyncOutboxRepository::may_have_reached_hub(
                            &connection,
                            value.id,
                            super::protocol::RecordKind::Meeting,
                        )? {
                            LibraryItemAccess::LocalOfflineReadOnly
                        } else {
                            LibraryItemAccess::LocalOffline
                        };
                    }
                    (false, failure)
                } else {
                    (true, None)
                }
            }
        };
        let mut meetings: Vec<_> = entries.into_values().collect();
        meetings.sort_by(|a, b| {
            b.started_at
                .cmp(&a.started_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        let meetings = meetings.into_iter().skip(offset).take(limit).collect();
        Ok(LibraryReadResultMeetings {
            meetings,
            hub_reachable: reachable,
            error,
        })
    }
}
pub struct LibraryReadResultMeetings {
    pub meetings: Vec<LibraryMeeting>,
    pub hub_reachable: bool,
    pub error: Option<String>,
}
fn local_meeting(
    value: crate::db::meetings::MeetingRecord,
    upload_state: Option<UploadState>,
    outbox_payload: Option<PayloadAvailability>,
) -> LibraryMeeting {
    let operational_payload =
        crate::sync::payload::resolve_operational_audio(std::path::Path::new(&value.audio_path))
            .ok()
            .flatten()
            .is_some();
    LibraryMeeting {
        id: value.sync_id,
        local_id: Some(value.id),
        origin_device_id: value.origin_device_id,
        title: value.title,
        title_source: value.title_source,
        title_version: value.title_version.try_into().unwrap_or_default(),
        source_filename: value.source_filename,
        status: value.status,
        transcript_text: value.transcript_text,
        transcript_segments: value.transcript_segments,
        duration_seconds: value.duration_seconds,
        started_at: value.started_at,
        completed_at: value.completed_at,
        error: value.error,
        created_at: value.created_at,
        upload_state,
        payload_availability: if operational_payload {
            PayloadAvailability::Available
        } else {
            outbox_payload.unwrap_or(PayloadAvailability::Unavailable)
        },
        access: LibraryItemAccess::Local,
        artifacts: vec![],
    }
}
fn shared_meeting(value: super::protocol::SharedMeeting) -> LibraryMeeting {
    LibraryMeeting {
        id: value.record_id,
        local_id: None,
        origin_device_id: value.origin_device_id,
        title: value.title,
        title_source: value.title_source,
        title_version: value.title_version,
        source_filename: value.source_filename,
        status: value.status,
        transcript_text: Some(value.transcript_text),
        transcript_segments: value.transcript_segments,
        duration_seconds: value.duration_seconds.try_into().ok(),
        started_at: value.created_at.clone(),
        completed_at: Some(value.completed_at),
        error: None,
        created_at: value.created_at,
        upload_state: Some(UploadState::Synced),
        payload_availability: value.recording_payload.availability,
        access: LibraryItemAccess::Shared,
        artifacts: value.artifacts,
    }
}

fn overlay_local_payload(
    local: Option<&LibraryMeeting>,
    mut shared: LibraryMeeting,
) -> LibraryMeeting {
    if let Some(local) = local {
        shared.local_id = local.local_id;
        shared.payload_availability =
            merge_payload_availability(local.payload_availability, shared.payload_availability);
        shared.upload_state = local.upload_state;
    }
    shared
}

fn contains_case_insensitive(value: &str, query: &str) -> bool {
    value.to_lowercase().contains(&query.to_lowercase())
}

impl DictationQueries {
    pub fn new(db_path: PathBuf, remote: Arc<dyn RemoteDictationLibrary>) -> Self {
        Self { db_path, remote }
    }

    pub async fn read(
        &self,
        role: &LibraryRole,
        params: &SearchParams,
    ) -> Result<LibraryReadResult> {
        let connection = crate::db::open_db_at(&self.db_path)?;
        let offset = params.offset;
        let limit = params.limit.clamp(1, 100);
        let fetch = offset.saturating_add(limit);
        let local = crate::db::list_visible_workflows(
            &connection,
            params.query.as_deref(),
            params.from.as_deref(),
            params.to.as_deref(),
            0,
            fetch,
        )?;
        let mut entries = BTreeMap::new();
        for workflow in local {
            let mut entry = HistoryEntry::from(workflow);
            entry.upload_state = SyncOutboxRepository::state_for(&connection, entry.id)?;
            if entry.payload_availability == PayloadAvailability::Unavailable {
                entry.payload_availability =
                    SyncOutboxRepository::payload_availability(&connection, entry.id)?
                        .unwrap_or(PayloadAvailability::Unavailable);
            }
            entries.insert(entry.id, entry);
        }

        let (reachable, error) = match role {
            LibraryRole::Standalone => (false, None),
            LibraryRole::HomeHub => {
                for shared in SharedLibraryRepository::page_dictations(
                    &connection,
                    params.query.as_deref(),
                    params.from.as_deref(),
                    params.to.as_deref(),
                    None,
                    fetch,
                )? {
                    let id = shared.record_id;
                    if SyncOutboxRepository::deletion_masks(
                        &connection,
                        id,
                        super::protocol::RecordKind::Dictation,
                    )? {
                        continue;
                    }
                    let mut entry = shared_entry(shared, LibraryItemAccess::Shared);
                    if let Some(local) = entries.get(&id) {
                        entry.payload_availability = merge_payload_availability(
                            local.payload_availability,
                            entry.payload_availability,
                        );
                    }
                    entries.insert(id, entry);
                }
                (true, None)
            }
            LibraryRole::ConnectedDevice { hub } => {
                let mut cursor = None;
                let mut failure = None;
                let mut fetched_from_hub = 0usize;
                loop {
                    let page_limit = fetch.saturating_sub(fetched_from_hub).clamp(1, 100);
                    match self
                        .remote
                        .page_dictations(
                            hub,
                            params.query.as_deref(),
                            params.from.as_deref(),
                            params.to.as_deref(),
                            cursor.as_deref(),
                            page_limit,
                        )
                        .await
                    {
                        Ok(page) => {
                            let page_len = page.items.len();
                            fetched_from_hub = fetched_from_hub.saturating_add(page_len);
                            for shared in page.items {
                                let id = shared.record_id;
                                if SyncOutboxRepository::deletion_masks(
                                    &connection,
                                    id,
                                    super::protocol::RecordKind::Dictation,
                                )? {
                                    continue;
                                }
                                let mut entry = shared_entry(shared, LibraryItemAccess::Shared);
                                if let Some(local) = entries.get(&id) {
                                    entry.payload_availability = merge_payload_availability(
                                        local.payload_availability,
                                        entry.payload_availability,
                                    );
                                }
                                entries.insert(id, entry);
                            }
                            cursor = page.next_cursor;
                            if fetched_from_hub >= fetch || cursor.is_none() || page_len == 0 {
                                break;
                            }
                        }
                        Err(error) => {
                            failure = Some(error.to_string());
                            break;
                        }
                    }
                }
                if let Some(error) = failure {
                    entries.retain(|_, entry| entry.source == HistorySource::Local);
                    for entry in entries.values_mut() {
                        entry.offline = true;
                    }
                    (false, Some(error))
                } else {
                    (true, None)
                }
            }
        };
        let mut entries: Vec<_> = entries.into_values().collect();
        entries.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| right.id.cmp(&left.id))
        });
        entries = entries.into_iter().skip(offset).take(limit).collect();
        Ok(LibraryReadResult {
            entries,
            hub_reachable: reachable,
            error,
        })
    }
}

fn merge_payload_availability(
    local: PayloadAvailability,
    shared: PayloadAvailability,
) -> PayloadAvailability {
    if shared == PayloadAvailability::Available || local == PayloadAvailability::Available {
        PayloadAvailability::Available
    } else if local == PayloadAvailability::NeedsAttention {
        PayloadAvailability::NeedsAttention
    } else if local == PayloadAvailability::Pending {
        PayloadAvailability::Pending
    } else {
        shared
    }
}

fn shared_entry(
    shared: super::protocol::SharedDictation,
    access: LibraryItemAccess,
) -> HistoryEntry {
    HistoryEntry {
        id: shared.record_id,
        text: shared.text,
        created_at: shared.created_at,
        origin_device_id: shared.origin_device_id,
        source: HistorySource::Shared,
        upload_state: Some(UploadState::Synced),
        payload_availability: shared.recording_payload.availability,
        offline: access.offline(),
        read_only: access.read_only(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{VoiceToTextData, Workflow, WorkflowData, WorkflowType};
    use crate::sync::protocol::{
        DictationPage, DictationPayload, DictationSnapshot, MeetingPage, RecordKind, SharedMeeting,
    };
    use crate::sync::transport::{HubTransferError, RemoteDictationLibrary, RemoteMeetingLibrary};
    use async_trait::async_trait;
    use audetic_core::sync::{HubConnection, HubId, SyncRole};
    use futures_util::TryStreamExt;

    #[tokio::test]
    async fn local_payload_stream_opens_and_bounds_the_requested_range() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("payload.wav");
        std::fs::write(&path, b"0123456789").unwrap();

        let payload = open_local_payload(path, Some("bytes=2-5")).await.unwrap();
        assert_eq!(payload.status, 206);
        assert_eq!(payload.metadata.content_length, Some(4));
        assert_eq!(
            payload.metadata.content_range,
            Some(super::super::transport::PayloadContentRange::Bytes {
                start: 2,
                end: 5,
                complete_length: 10,
            })
        );
        let chunks = payload.body.try_collect::<Vec<_>>().await.unwrap();
        assert_eq!(chunks.concat(), b"2345");
    }

    #[tokio::test]
    async fn local_payload_stream_reports_unsatisfied_range_without_exposing_a_path() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("payload.wav");
        std::fs::write(&path, b"short").unwrap();

        let payload = open_local_payload(path, Some("bytes=20-30")).await.unwrap();
        assert_eq!(payload.status, 416);
        assert_eq!(
            payload.metadata.content_range,
            Some(super::super::transport::PayloadContentRange::Unsatisfied { complete_length: 5 })
        );
        assert!(payload
            .body
            .try_collect::<Vec<_>>()
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn direct_meeting_lookup_is_not_limited_by_the_first_page() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("meetings.sqlite");
        let connection = crate::db::migrate_db_at(&path).unwrap();
        let mut target = None;
        for index in 0..120 {
            let local_id = crate::db::meetings::MeetingRepository::insert(
                &connection,
                Some(&format!("Meeting {index}")),
                "/missing.wav",
            )
            .unwrap();
            crate::db::meetings::MeetingRepository::complete(
                &connection,
                local_id,
                "/missing.txt",
                "transcript",
                None,
                1,
            )
            .unwrap();
            if index == 0 {
                target = Some(
                    crate::db::meetings::MeetingRepository::get(&connection, local_id)
                        .unwrap()
                        .unwrap()
                        .sync_id,
                );
            }
        }
        drop(connection);

        let service = crate::sync::SyncService::local_library(path);
        let meeting = service.meeting(target.unwrap()).await.unwrap();
        assert!(meeting.is_some());
    }

    #[test]
    fn local_payload_failure_is_visible_until_the_hub_has_an_available_blob() {
        assert_eq!(
            merge_payload_availability(
                PayloadAvailability::NeedsAttention,
                PayloadAvailability::Pending,
            ),
            PayloadAvailability::NeedsAttention
        );
        assert_eq!(
            merge_payload_availability(
                PayloadAvailability::NeedsAttention,
                PayloadAvailability::Available,
            ),
            PayloadAvailability::Available
        );
    }

    struct OfflineHub;

    #[async_trait]
    impl RemoteDictationLibrary for OfflineHub {
        async fn page_dictations(
            &self,
            _hub: &HubConnection,
            _query: Option<&str>,
            _from: Option<&str>,
            _to: Option<&str>,
            _cursor: Option<&str>,
            _limit: usize,
        ) -> std::result::Result<DictationPage, HubTransferError> {
            Err(HubTransferError::Retryable("hub offline".to_owned()))
        }
    }

    #[async_trait]
    impl RemoteMeetingLibrary for OfflineHub {
        async fn page_meetings(
            &self,
            _hub: &HubConnection,
            _query: Option<&str>,
            _cursor: Option<&str>,
            _limit: usize,
        ) -> std::result::Result<MeetingPage, HubTransferError> {
            Err(HubTransferError::Retryable("hub offline".to_owned()))
        }

        async fn meeting(
            &self,
            _hub: &HubConnection,
            _id: RecordId,
        ) -> std::result::Result<Option<SharedMeeting>, HubTransferError> {
            Err(HubTransferError::Retryable("hub offline".to_owned()))
        }
    }

    struct LocalHub {
        library: super::super::library::HubLibrary,
    }

    #[async_trait]
    impl RemoteDictationLibrary for LocalHub {
        async fn page_dictations(
            &self,
            _hub: &HubConnection,
            query: Option<&str>,
            from: Option<&str>,
            to: Option<&str>,
            cursor: Option<&str>,
            limit: usize,
        ) -> std::result::Result<super::super::protocol::DictationPage, HubTransferError> {
            self.library
                .page_dictations(query, from, to, cursor, limit)
                .map_err(|error| HubTransferError::Retryable(error.to_string()))
        }
    }

    fn role(role: SyncRole) -> LibraryRole {
        match role {
            SyncRole::Standalone => LibraryRole::Standalone,
            SyncRole::HomeHub => LibraryRole::HomeHub,
            SyncRole::ConnectedDevice => LibraryRole::ConnectedDevice {
                hub: HubConnection {
                    base_url: "https://hub.example.ts.net:8443/audetic/".into(),
                    hub_id: HubId::new(),
                    owner_login: "owner@example.com".into(),
                },
            },
        }
    }

    #[tokio::test]
    async fn home_hub_merge_dedupes_sorts_searches_and_paginates_by_uuid() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("db.sqlite");
        let mut conn = crate::db::migrate_db_at(&path).unwrap();
        let workflow = Workflow::new(
            WorkflowType::VoiceToText,
            WorkflowData::VoiceToText(VoiceToTextData {
                text: "alpha local".into(),
                audio_path: "/missing".into(),
            }),
        );
        let (_, local_id) = crate::db::insert_workflow_record(&conn, &workflow).unwrap();
        conn.execute(
            "UPDATE workflows SET created_at = '2026-09-04T10:00:00Z' WHERE sync_id = ?1",
            [local_id.to_string()],
        )
        .unwrap();
        let local = crate::db::get_workflow_by_sync_id(&conn, local_id)
            .unwrap()
            .unwrap();
        let local_created = local.created_at.unwrap();
        SharedLibraryRepository::apply_snapshot(
            &mut conn,
            &DictationSnapshot {
                kind: RecordKind::Dictation,
                schema_version: 1,
                record_id: local_id,
                origin_device_id: local.origin_device_id.unwrap(),
                local_version: 1,
                created_at: local_created.clone(),
                updated_at: local_created,
                payload: DictationPayload {
                    text: "alpha local".into(),
                    recording_payload: Default::default(),
                },
            },
        )
        .unwrap();
        SharedLibraryRepository::apply_snapshot(
            &mut conn,
            &DictationSnapshot {
                kind: RecordKind::Dictation,
                schema_version: 1,
                record_id: audetic_core::sync::RecordId::new(),
                origin_device_id: audetic_core::sync::DeviceId::new(),
                local_version: 1,
                created_at: "2026-09-04T11:00:00Z".into(),
                updated_at: "2026-09-04T11:00:00Z".into(),
                payload: DictationPayload {
                    text: "alpha remote".into(),
                    recording_payload: Default::default(),
                },
            },
        )
        .unwrap();
        drop(conn);

        let reader = DictationQueries::new(path, Arc::new(OfflineHub));
        let first = reader
            .read(
                &role(SyncRole::HomeHub),
                &SearchParams::new().with_query("alpha").with_limit(1),
            )
            .await
            .unwrap();
        assert_eq!(first.entries.len(), 1);
        assert_eq!(first.entries[0].text, "alpha remote");
        let mut second_params = SearchParams::new().with_query("alpha").with_limit(1);
        second_params.offset = 1;
        let second = reader
            .read(&role(SyncRole::HomeHub), &second_params)
            .await
            .unwrap();
        assert_eq!(second.entries.len(), 1);
        assert_eq!(second.entries[0].id, local_id);
        assert_eq!(second.entries[0].source, HistorySource::Shared);
    }

    #[tokio::test]
    async fn shared_meeting_overlay_keeps_origin_audio_available_without_duplicates() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("meetings.sqlite");
        let audio_path = temp.path().join("meeting.wav");
        std::fs::write(&audio_path, b"audio").unwrap();
        let mut conn = crate::db::migrate_db_at(&path).unwrap();
        let local_id = crate::db::meetings::MeetingRepository::insert(
            &conn,
            Some("Local title"),
            audio_path.to_str().unwrap(),
        )
        .unwrap();
        crate::db::meetings::MeetingRepository::complete(
            &conn,
            local_id,
            "/tmp/transcript.txt",
            "portable transcript",
            None,
            30,
        )
        .unwrap();
        let local = crate::db::meetings::MeetingRepository::get(&conn, local_id)
            .unwrap()
            .unwrap();
        SharedLibraryRepository::apply_meeting_snapshot(&mut conn, &local.snapshot().unwrap())
            .unwrap();
        drop(conn);

        let result = MeetingQueries::new(path, Arc::new(OfflineHub))
            .read(&role(SyncRole::HomeHub), None, 0, 10)
            .await
            .unwrap();
        assert_eq!(result.meetings.len(), 1);
        assert_eq!(result.meetings[0].id, local.sync_id);
        assert_eq!(result.meetings[0].access, LibraryItemAccess::Shared);
        assert_eq!(result.meetings[0].local_id, Some(local_id));
        assert_eq!(
            result.meetings[0].payload_availability,
            PayloadAvailability::Available
        );
    }

    #[tokio::test]
    async fn meeting_pages_are_capped_at_one_hundred_without_losing_deep_offsets() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("meeting-pages.sqlite");
        let conn = crate::db::migrate_db_at(&path).unwrap();
        for index in 0..120 {
            let id = crate::db::meetings::MeetingRepository::insert(
                &conn,
                Some(&format!("Meeting {index:03}")),
                "/missing.mp3",
            )
            .unwrap();
            crate::db::meetings::MeetingRepository::complete(
                &conn,
                id,
                "/missing.txt",
                "transcript",
                None,
                10,
            )
            .unwrap();
        }
        drop(conn);
        let reader = MeetingQueries::new(path, Arc::new(OfflineHub));

        let first = reader
            .read(&role(SyncRole::Standalone), None, 0, 500)
            .await
            .unwrap();
        assert_eq!(first.meetings.len(), 100);
        let deep = reader
            .read(&role(SyncRole::Standalone), None, 100, 500)
            .await
            .unwrap();
        assert_eq!(deep.meetings.len(), 20);
    }

    #[tokio::test]
    async fn connected_live_only_falls_back_to_local_rows_with_offline_state() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("db.sqlite");
        let conn = crate::db::migrate_db_at(&path).unwrap();
        crate::db::insert_workflow(
            &conn,
            &Workflow::new(
                WorkflowType::VoiceToText,
                WorkflowData::VoiceToText(VoiceToTextData {
                    text: "offline local".into(),
                    audio_path: "/missing".into(),
                }),
            ),
        )
        .unwrap();
        drop(conn);
        let result = DictationQueries::new(path, Arc::new(OfflineHub))
            .read(&role(SyncRole::ConnectedDevice), &SearchParams::new())
            .await
            .unwrap();
        assert!(!result.hub_reachable);
        assert_eq!(result.entries.len(), 1);
        assert!(result.entries[0].offline);
    }

    #[tokio::test]
    async fn accepted_local_meeting_is_read_only_while_connected_hub_is_offline() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("meeting-offline.db");
        let conn = crate::db::migrate_db_at(&path).unwrap();
        let local_id = crate::db::meetings::MeetingRepository::insert(
            &conn,
            Some("Accepted meeting"),
            "/missing.wav",
        )
        .unwrap();
        crate::db::meetings::MeetingRepository::complete(
            &conn,
            local_id,
            "/missing.txt",
            "already shared transcript",
            None,
            30,
        )
        .unwrap();
        let meeting = crate::db::meetings::MeetingRepository::get(&conn, local_id)
            .unwrap()
            .unwrap();
        crate::db::sync_outbox::SyncOutboxRepository::enqueue_snapshot(
            &conn,
            &meeting.snapshot().unwrap().into(),
        )
        .unwrap();
        conn.execute(
            "UPDATE sync_outbox_items SET state = 'synced', accepted_hub_revision = 1 \
             WHERE record_id = ?1 AND kind = 'meeting'",
            [meeting.sync_id.to_string()],
        )
        .unwrap();
        drop(conn);

        let result = MeetingQueries::new(path, Arc::new(OfflineHub))
            .read(&role(SyncRole::ConnectedDevice), None, 0, 10)
            .await
            .unwrap();

        assert_eq!(result.meetings.len(), 1);
        assert!(result.meetings[0].access.offline());
        assert!(result.meetings[0].access.read_only());
        assert_eq!(result.meetings[0].upload_state, Some(UploadState::Synced));
    }

    #[tokio::test]
    async fn connected_merge_fetches_each_source_deep_enough_before_offset_and_limit() {
        let temp = tempfile::tempdir().unwrap();
        let local_path = temp.path().join("local.db");
        let hub_path = temp.path().join("hub.db");
        let local = crate::db::migrate_db_at(&local_path).unwrap();
        crate::db::migrate_db_at(&hub_path).unwrap();
        let hub_library = super::super::library::HubLibrary::new(hub_path);
        let mut accepted_duplicates = Vec::new();

        for index in 0..30 {
            let (_, record_id) = crate::db::insert_workflow_record(
                &local,
                &Workflow::new(
                    WorkflowType::VoiceToText,
                    WorkflowData::VoiceToText(VoiceToTextData {
                        text: format!("local-{index:02}"),
                        audio_path: "/missing".into(),
                    }),
                ),
            )
            .unwrap();
            let created_at = format!("2026-09-03T{:02}:00:00Z", index % 24);
            local
                .execute(
                    "UPDATE workflows SET created_at = ?2 WHERE sync_id = ?1",
                    rusqlite::params![record_id.to_string(), created_at],
                )
                .unwrap();
            if index < 15 {
                let stored = crate::db::get_workflow_by_sync_id(&local, record_id)
                    .unwrap()
                    .unwrap();
                let WorkflowData::VoiceToText(data) = stored.data;
                accepted_duplicates.push(DictationSnapshot {
                    kind: RecordKind::Dictation,
                    schema_version: 1,
                    record_id,
                    origin_device_id: stored.origin_device_id.unwrap(),
                    local_version: 1,
                    created_at: created_at.clone(),
                    updated_at: created_at,
                    payload: DictationPayload {
                        text: data.text,
                        recording_payload: Default::default(),
                    },
                });
            }
        }
        hub_library.apply_snapshots(accepted_duplicates).unwrap();

        let remote_origin = audetic_core::sync::DeviceId::new();
        let remote: Vec<_> = (0..40)
            .map(|index| DictationSnapshot {
                kind: RecordKind::Dictation,
                schema_version: 1,
                record_id: audetic_core::sync::RecordId::new(),
                origin_device_id: remote_origin,
                local_version: 1,
                created_at: format!("2026-09-05T{:02}:{:02}:00Z", index / 2, (index % 2) * 30),
                updated_at: format!("2026-09-05T{:02}:{:02}:00Z", index / 2, (index % 2) * 30),
                payload: DictationPayload {
                    text: format!("remote-{index:02}"),
                    recording_payload: Default::default(),
                },
            })
            .collect();
        for batch in remote.chunks(super::super::protocol::MAX_SNAPSHOT_BATCH) {
            hub_library.apply_snapshots(batch.to_vec()).unwrap();
        }
        drop(local);

        let mut params = SearchParams::new().with_limit(20);
        params.offset = 10;
        let result = DictationQueries::new(
            local_path,
            Arc::new(LocalHub {
                library: hub_library,
            }),
        )
        .read(&role(SyncRole::ConnectedDevice), &params)
        .await
        .unwrap();

        assert_eq!(result.entries.len(), 20);
        assert!(result
            .entries
            .iter()
            .all(|entry| entry.source == HistorySource::Shared));
        assert_eq!(result.entries[0].text, "remote-29");
        assert_eq!(result.entries[19].text, "remote-10");
    }
}
