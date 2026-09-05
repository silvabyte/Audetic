//! Operational/outbox mutations and the authoritative mutation adapter.

use audetic_core::sync::RecordId;

use crate::db::meeting_artifacts::{ArtifactDeleteOutcome, MeetingArtifactRepository};
use crate::db::meetings::{MeetingRepository, SoftDeleteOutcome};

use super::{
    DeleteMeetingResult, LibraryError, LibraryItemAccess, LibraryResult, MeetingTitleResult,
    SharedLibrary,
};
use crate::sync::transition::LibraryRole;

impl SharedLibrary {
    pub async fn update_meeting_title(
        &self,
        id: RecordId,
        title: String,
    ) -> LibraryResult<MeetingTitleResult> {
        let title = title.trim().to_owned();
        if title.is_empty() {
            return Err(LibraryError::Invalid(
                "Meeting Title cannot be blank".into(),
            ));
        }
        let current = self.meeting(id).await?;
        if current.access == LibraryItemAccess::Shared {
            let updated = self
                .update_authoritative_title(id, title, current.title_version, None)
                .await?;
            return Ok(MeetingTitleResult {
                meeting_id: id,
                title: updated.title,
                title_source: updated.title_source,
                local_id: updated.local_id,
            });
        }
        if current.access.read_only() {
            return Err(LibraryError::Unavailable(
                "Home Hub is unavailable; shared title edits are not queued offline".into(),
            ));
        }
        let local_id = current.local_id.ok_or_else(|| {
            LibraryError::internal(
                "resolving local meeting identity",
                anyhow::anyhow!("local meeting has no row identity"),
            )
        })?;
        let context = self.context()?;
        let connection = crate::db::open_db_at(&context.db_path)
            .map_err(|error| LibraryError::internal("opening meeting library", error))?;
        if !MeetingRepository::set_manual_title(&connection, local_id, &title)
            .map_err(|error| LibraryError::internal("updating local Meeting Title", error))?
        {
            return Err(LibraryError::NotFound(format!("Meeting {id} not found")));
        }
        let updated = MeetingRepository::get(&connection, local_id)
            .map_err(|error| LibraryError::internal("reading updated Meeting Title", error))?
            .ok_or_else(|| LibraryError::NotFound(format!("Meeting {id} not found")))?;
        Ok(MeetingTitleResult {
            meeting_id: id,
            title: updated.title,
            title_source: updated.title_source,
            local_id: Some(local_id),
        })
    }

    pub(super) async fn update_authoritative_title(
        &self,
        id: RecordId,
        title: String,
        expected_title_version: u64,
        title_source: Option<String>,
    ) -> LibraryResult<AuthoritativeTitle> {
        let context = self.context()?;
        let patch = crate::sync::protocol::MeetingTitlePatch {
            title,
            expected_title_version,
            title_source,
        };
        let local_target = if matches!(context.role, LibraryRole::ConnectedDevice { .. }) {
            let connection = crate::db::open_db_at(&context.db_path)
                .map_err(|error| LibraryError::internal("opening local title mirror", error))?;
            MeetingRepository::internal_id(&connection, id)
                .map_err(|error| LibraryError::internal("resolving local title mirror", error))?
        } else {
            None
        };
        let (updated, local_id) = match &context.role {
            LibraryRole::Standalone => {
                return Err(LibraryError::Conflict("Meeting is not shared".into()));
            }
            LibraryRole::HomeHub => {
                let receipt = crate::sync::library::HubLibrary::new(context.db_path.clone())
                    .update_meeting_title(id, &patch)
                    .map_err(map_authoritative_title_error)?
                    .ok_or_else(|| LibraryError::NotFound(format!("Meeting {id} not found")))?;
                (receipt.meeting, receipt.local_id)
            }
            LibraryRole::ConnectedDevice { hub } => {
                let updated = context
                    .capabilities
                    .mutations()
                    .update_meeting_title(hub, id, patch.clone())
                    .await
                    .map_err(map_remote_error)?;
                let local_id = if let Some(local_id) = local_target {
                    let title = updated.title.clone();
                    let title_source = updated.title_source.clone();
                    let updated_at = updated.updated_at.clone();
                    let updated_version = updated.title_version;
                    self.coordinator
                        .enrich_connected_library_receipt(
                            &context,
                            "authoritative Meeting Title update",
                            move |connection| {
                                let mirrored =
                                    MeetingRepository::mirror_authoritative_title_if_version(
                                    connection,
                                    local_id,
                                    id,
                                    expected_title_version,
                                    title.as_deref(),
                                    title_source.as_deref(),
                                    updated_version,
                                    &updated_at,
                                )?;
                                anyhow::ensure!(
                                    mirrored,
                                    "local Meeting Title changed while the authoritative update was in flight"
                                );
                                Ok(local_id)
                            },
                        )
                        .await
                } else {
                    None
                };
                (updated, local_id)
            }
        };
        Ok(AuthoritativeTitle {
            title: updated.title,
            title_source: updated.title_source,
            local_id,
        })
    }

    pub async fn delete_meeting(&self, id: RecordId) -> LibraryResult<DeleteMeetingResult> {
        let meeting = self.meeting(id).await?;
        if !crate::meeting::MeetingPhase::is_terminal(&meeting.status) {
            return Err(LibraryError::Conflict(format!(
                "Meeting {id} is still in progress; stop or cancel it before deleting"
            )));
        }
        if meeting.access.read_only() {
            return Err(LibraryError::Unavailable(
                "Home Hub is unavailable; shared deletions are not queued offline".into(),
            ));
        }
        let context = self.context()?;
        let connection = crate::db::open_db_at(&context.db_path)
            .map_err(|error| LibraryError::internal("opening meeting library", error))?;
        if meeting.access == LibraryItemAccess::Shared {
            self.delete_authoritative_record(id, crate::sync::protocol::RecordKind::Meeting)
                .await?;
            if let Some(local_id) = meeting.local_id {
                return match MeetingRepository::soft_delete_after_hub_delete(&connection, local_id)
                    .map_err(|error| {
                        LibraryError::internal("cleaning up deleted local meeting", error)
                    })? {
                    SoftDeleteOutcome::Deleted | SoftDeleteOutcome::NotFound => {
                        Ok(DeleteMeetingResult {
                            local_id: Some(local_id),
                        })
                    }
                    SoftDeleteOutcome::InFlight => Err(LibraryError::Conflict(format!(
                        "Meeting {id} is still in progress"
                    ))),
                    SoftDeleteOutcome::RequiresHub => Err(LibraryError::internal(
                        "cleaning up deleted local meeting",
                        anyhow::anyhow!("local cleanup unexpectedly requires Home Hub"),
                    )),
                };
            }
            MeetingRepository::cleanup_deleted_sync_work(&connection, id).map_err(|error| {
                LibraryError::internal("cleaning up deleted meeting sync work", error)
            })?;
            return Ok(DeleteMeetingResult { local_id: None });
        }
        let local_id = meeting.local_id.ok_or_else(|| {
            LibraryError::internal(
                "resolving local meeting identity",
                anyhow::anyhow!("local meeting has no row identity"),
            )
        })?;
        match MeetingRepository::soft_delete(&connection, local_id)
            .map_err(|error| LibraryError::internal("deleting local meeting", error))?
        {
            SoftDeleteOutcome::Deleted => Ok(DeleteMeetingResult {
                local_id: Some(local_id),
            }),
            SoftDeleteOutcome::NotFound => {
                Err(LibraryError::NotFound(format!("Meeting {id} not found")))
            }
            SoftDeleteOutcome::InFlight => Err(LibraryError::Conflict(format!(
                "Meeting {id} is still in progress; stop or cancel it before deleting"
            ))),
            SoftDeleteOutcome::RequiresHub => Err(LibraryError::Unavailable(
                "Home Hub deletion is required".into(),
            )),
        }
    }

    pub async fn delete_artifact(
        &self,
        meeting_id: RecordId,
        artifact_id: RecordId,
    ) -> LibraryResult<()> {
        let meeting = self.meeting(meeting_id).await?;
        let context = self.context()?;
        let connection = crate::db::open_db_at(&context.db_path)
            .map_err(|error| LibraryError::internal("opening artifact library", error))?;
        let local = MeetingArtifactRepository::get_by_sync_id(&connection, artifact_id)
            .map_err(|error| LibraryError::internal("reading local artifact", error))?
            .filter(|artifact| artifact.meeting_id == meeting_id);
        let shared_run = MeetingArtifactRepository::list_shared_runs(&connection, meeting_id)
            .map_err(|error| LibraryError::internal("reading shared artifact runs", error))?
            .into_iter()
            .find(|artifact| artifact.id == artifact_id);
        let exists = local.is_some()
            || shared_run.is_some()
            || meeting
                .artifacts
                .iter()
                .any(|artifact| artifact.record_id == artifact_id);
        if !exists {
            return Err(LibraryError::NotFound(format!(
                "Artifact {artifact_id} not found"
            )));
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
            return Err(LibraryError::Conflict(
                "Artifact is still being generated; wait for it to finish before deleting".into(),
            ));
        }
        if meeting.access == LibraryItemAccess::Shared {
            self.delete_authoritative_record(
                artifact_id,
                crate::sync::protocol::RecordKind::Artifact,
            )
            .await?;
            if let Some(local) = local {
                MeetingArtifactRepository::delete_for_live_meeting(
                    &connection,
                    local.local_meeting_id,
                    local.local_id,
                )
                .map_err(|error| LibraryError::internal("deleting local artifact mirror", error))?;
            }
            MeetingArtifactRepository::cleanup_after_shared_delete(
                &connection,
                meeting_id,
                artifact_id,
            )
            .map_err(|error| LibraryError::internal("cleaning up shared artifact run", error))?;
            return Ok(());
        }
        if meeting.access.read_only() {
            return Err(LibraryError::Unavailable(
                "Home Hub is unavailable; shared artifact deletions are not queued offline".into(),
            ));
        }
        let local = local
            .ok_or_else(|| LibraryError::NotFound(format!("Artifact {artifact_id} not found")))?;
        match MeetingArtifactRepository::delete_for_live_meeting_guarded(
            &connection,
            local.local_meeting_id,
            local.local_id,
        )
        .map_err(|error| LibraryError::internal("deleting local artifact", error))?
        {
            ArtifactDeleteOutcome::Deleted => Ok(()),
            ArtifactDeleteOutcome::NotFound => Err(LibraryError::NotFound(format!(
                "Artifact {artifact_id} not found"
            ))),
            ArtifactDeleteOutcome::InFlight => Err(LibraryError::Conflict(
                "Artifact is still being generated; wait for it to finish before deleting".into(),
            )),
            ArtifactDeleteOutcome::RequiresHub => {
                self.delete_authoritative_record(
                    artifact_id,
                    crate::sync::protocol::RecordKind::Artifact,
                )
                .await?;
                if !MeetingArtifactRepository::delete_for_live_meeting(
                    &connection,
                    local.local_meeting_id,
                    local.local_id,
                )
                .map_err(|error| LibraryError::internal("cleaning up local artifact", error))?
                {
                    return Err(LibraryError::NotFound(format!(
                        "Artifact {artifact_id} not found"
                    )));
                }
                Ok(())
            }
        }
    }

    async fn delete_authoritative_record(
        &self,
        id: RecordId,
        kind: crate::sync::protocol::RecordKind,
    ) -> LibraryResult<()> {
        let context = self.context()?;
        match &context.role {
            LibraryRole::Standalone => Err(LibraryError::Conflict("Record is not shared".into())),
            LibraryRole::HomeHub => crate::sync::library::HubLibrary::new(context.db_path)
                .delete(id, kind)
                .map(|_| ())
                .map_err(|error| match error {
                    crate::db::shared_library::ApplySnapshotError::VersionConflict => {
                        LibraryError::Conflict("Record version conflict".into())
                    }
                    crate::db::shared_library::ApplySnapshotError::Tombstoned => {
                        LibraryError::NotFound(format!("Record {id} not found"))
                    }
                    error => LibraryError::internal("deleting authoritative record", error),
                }),
            LibraryRole::ConnectedDevice { hub } => context
                .capabilities
                .mutations()
                .delete_record(hub, id, kind)
                .await
                .map_err(map_remote_error),
        }
    }
}

pub(super) struct AuthoritativeTitle {
    title: Option<String>,
    title_source: Option<String>,
    local_id: Option<i64>,
}

pub(super) fn map_remote_error(error: crate::sync::transport::HubTransferError) -> LibraryError {
    match error {
        crate::sync::transport::HubTransferError::Http {
            status: 404,
            message,
            ..
        } => LibraryError::NotFound(message),
        crate::sync::transport::HubTransferError::Http {
            status: 409,
            message,
            ..
        } => LibraryError::Conflict(message),
        crate::sync::transport::HubTransferError::Http {
            status: 400,
            message,
            ..
        } => LibraryError::Invalid(message),
        error if error.is_retryable() => LibraryError::Unavailable(
            "Home Hub is unavailable; try the operation again later".into(),
        ),
        error => LibraryError::internal("communicating with Home Hub", error),
    }
}

fn map_authoritative_title_error(
    error: crate::db::shared_library::MeetingTitleUpdateError,
) -> LibraryError {
    use crate::db::shared_library::MeetingTitleUpdateError;
    match error {
        MeetingTitleUpdateError::InvalidTitle => {
            LibraryError::Invalid("Meeting Title cannot be blank".into())
        }
        MeetingTitleUpdateError::InvalidSource => {
            LibraryError::Invalid("Invalid Meeting Title source".into())
        }
        MeetingTitleUpdateError::Conflict => {
            LibraryError::Conflict("Meeting Title version conflict".into())
        }
        error => LibraryError::internal("updating authoritative Meeting Title", error),
    }
}
