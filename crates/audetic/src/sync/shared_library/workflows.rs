//! Artifact and Meeting Title workflows that span read and mutation models.

use audetic_core::sync::RecordId;

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::db::meeting_artifacts::MeetingArtifactRepository;
use crate::db::meetings::MeetingRepository;
use crate::meeting_artifacts::GenerateArtifactRequest;

use super::{LibraryError, LibraryItemAccess, LibraryResult, PreparedMeetingRetry, SharedLibrary};

impl SharedLibrary {
    pub fn public_meeting_id(&self, local_id: i64) -> LibraryResult<RecordId> {
        let context = self.context()?;
        let connection = crate::db::open_db_at(&context.db_path)
            .map_err(|error| LibraryError::internal("opening meeting library", error))?;
        MeetingRepository::get(&connection, local_id)
            .map_err(|error| LibraryError::internal("reading local meeting identity", error))?
            .map(|meeting| meeting.sync_id)
            .ok_or_else(|| LibraryError::NotFound(format!("Meeting {local_id} not found")))
    }

    pub fn recent_meeting_titles(&self, limit: usize) -> LibraryResult<Vec<String>> {
        let context = self.context()?;
        let connection = crate::db::open_db_at(&context.db_path)
            .map_err(|error| LibraryError::internal("opening meeting library", error))?;
        MeetingRepository::recent_manual_titles(&connection, limit)
            .map_err(|error| LibraryError::internal("reading recent Meeting Titles", error))
    }

    pub async fn prepare_meeting_retry(&self, id: RecordId) -> LibraryResult<PreparedMeetingRetry> {
        let db_path = self.context()?.db_path;
        tokio::task::spawn_blocking(move || {
            let connection = crate::db::open_db_at(&db_path)
                .map_err(|error| LibraryError::internal("opening meeting library", error))?;
            let meeting = MeetingRepository::get_by_sync_id(&connection, id)
                .map_err(|error| LibraryError::internal("reading retry meeting", error))?
                .ok_or_else(|| LibraryError::NotFound(format!("Meeting {id} not found")))?;
            if meeting.status != crate::meeting::MeetingPhase::Error.as_str() {
                return Err(LibraryError::Conflict(format!(
                    "Meeting {id} is in state '{}'; only failed meetings can be retried",
                    meeting.status
                )));
            }
            let stored_path = PathBuf::from(&meeting.audio_path);
            let audio_path = if stored_path.exists() {
                stored_path
            } else {
                let mp3_sibling = stored_path.with_extension("mp3");
                if !mp3_sibling.exists() {
                    tracing::warn!(
                        meeting_id = %id,
                        audio_path = %stored_path.display(),
                        "meeting retry audio is missing"
                    );
                    return Err(LibraryError::Conflict(
                        "Meeting audio is no longer available for retry".into(),
                    ));
                }
                MeetingRepository::update_audio_path(
                    &connection,
                    meeting.id,
                    mp3_sibling.to_string_lossy().as_ref(),
                )
                .map_err(|error| LibraryError::internal("healing meeting audio location", error))?;
                mp3_sibling
            };
            if !MeetingRepository::begin_retry(&connection, meeting.id)
                .map_err(|error| LibraryError::internal("claiming meeting retry", error))?
            {
                return Err(LibraryError::Conflict(format!(
                    "Meeting {id} is no longer eligible for retry; its state changed"
                )));
            }
            Ok(PreparedMeetingRetry {
                local_id: meeting.id,
                record_id: id,
                audio_path,
                duration_seconds: meeting.duration_seconds.unwrap_or(0),
            })
        })
        .await
        .map_err(|error| LibraryError::internal("joining meeting retry preparation", error))?
    }

    pub async fn regenerate_meeting_title(&self, id: RecordId) -> LibraryResult<Option<i64>> {
        let meeting = self.meeting(id).await?;
        if meeting.access.read_only() {
            return Err(LibraryError::Unavailable(
                "Home Hub is unavailable; generated titles are not queued offline".into(),
            ));
        }
        if meeting.access == LibraryItemAccess::Shared {
            let transcript = meeting
                .transcript_text
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| LibraryError::Conflict("Meeting has no transcript".into()))?;
            let context = self.context()?;
            let generated = crate::meeting::title::generate_shared_meeting_title(
                id,
                transcript,
                &context.db_path,
            )
            .await
            .map_err(|error| LibraryError::internal("generating Meeting Title", error))?;
            self.update_authoritative_title(
                id,
                generated,
                meeting.title_version,
                Some("generated".into()),
            )
            .await?;
            return Ok(None);
        }
        let local_id = meeting.local_id.ok_or_else(|| {
            LibraryError::internal(
                "resolving local meeting identity",
                anyhow::anyhow!("local meeting has no row identity"),
            )
        })?;
        let db_path = self.context()?.db_path;
        crate::meeting::title::prepare_title_regeneration_at(&db_path, local_id).map_err(
            |error| LibraryError::internal("preparing Meeting Title regeneration", error),
        )?;
        crate::meeting::title::spawn_title_generation_at(local_id, db_path);
        Ok(Some(local_id))
    }

    pub async fn artifacts(
        &self,
        meeting_id: RecordId,
    ) -> LibraryResult<Vec<crate::db::meeting_artifacts::MeetingArtifact>> {
        let meeting = self.meeting(meeting_id).await?;
        let context = self.context()?;
        let connection = crate::db::open_db_at(&context.db_path)
            .map_err(|error| LibraryError::internal("opening artifact library", error))?;
        if meeting.access == LibraryItemAccess::Shared {
            let mut artifacts = meeting
                .artifacts
                .into_iter()
                .filter(|artifact| {
                    !crate::db::sync_outbox::SyncOutboxRepository::deletion_masks(
                        &connection,
                        artifact.record_id,
                        crate::sync::protocol::RecordKind::Artifact,
                    )
                    .unwrap_or(false)
                })
                .map(super::queries::shared_artifact)
                .map(|artifact| (artifact.id, artifact))
                .collect::<BTreeMap<_, _>>();
            for artifact in MeetingArtifactRepository::list_shared_runs(&connection, meeting_id)
                .map_err(|error| LibraryError::internal("reading shared artifact runs", error))?
            {
                artifacts.insert(artifact.id, artifact);
            }
            let mut artifacts = artifacts.into_values().collect::<Vec<_>>();
            artifacts.sort_by(|left, right| right.created_at.cmp(&left.created_at));
            return Ok(artifacts);
        }
        let local_id = meeting.local_id.ok_or_else(|| {
            LibraryError::internal(
                "resolving local meeting identity",
                anyhow::anyhow!("local meeting has no row identity"),
            )
        })?;
        MeetingArtifactRepository::list_for_live_meeting(&connection, local_id)
            .map_err(|error| LibraryError::internal("reading local meeting artifacts", error))
    }

    pub async fn artifact(
        &self,
        meeting_id: RecordId,
        artifact_id: RecordId,
    ) -> LibraryResult<crate::db::meeting_artifacts::MeetingArtifact> {
        self.artifacts(meeting_id)
            .await?
            .into_iter()
            .find(|artifact| artifact.id == artifact_id)
            .ok_or_else(|| LibraryError::NotFound(format!("Artifact {artifact_id} not found")))
    }

    pub async fn generate_artifact(
        &self,
        meeting_id: RecordId,
        request: GenerateArtifactRequest,
    ) -> LibraryResult<crate::db::meeting_artifacts::MeetingArtifact> {
        let meeting = self.meeting(meeting_id).await?;
        if meeting.access.read_only() {
            return Err(LibraryError::Unavailable(
                "Home Hub is unavailable; shared artifacts are not queued offline".into(),
            ));
        }
        let context = self.context()?;
        if meeting.access == LibraryItemAccess::Shared {
            let transcript = meeting
                .transcript_text
                .as_deref()
                .ok_or_else(|| LibraryError::Conflict("Meeting has no transcript".into()))?;
            return crate::meeting_artifacts::generate_shared_meeting_artifact(
                &context.db_path,
                meeting_id,
                meeting.title.as_deref(),
                transcript,
                request,
            )
            .await
            .map_err(|error| LibraryError::internal("generating shared meeting artifact", error));
        }
        let local_id = meeting.local_id.ok_or_else(|| {
            LibraryError::internal(
                "resolving local meeting identity",
                anyhow::anyhow!("local meeting has no row identity"),
            )
        })?;
        crate::meeting_artifacts::generate_meeting_artifact_at(&context.db_path, local_id, request)
            .await
            .map_err(|error| LibraryError::internal("generating meeting artifact", error))
    }
}
