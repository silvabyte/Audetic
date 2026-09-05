//! Canonical merge read model and pagination ownership.

use audetic_core::sync::{PayloadAvailability, RecordId, UploadState};

use std::collections::{BTreeMap, HashSet};

use crate::db::shared_library::SharedLibraryRepository;
use crate::db::sync_outbox::SyncOutboxRepository;
use crate::history::{HistoryEntry, HistorySource, SearchParams};

use super::{
    LibraryError, LibraryItemAccess, LibraryMeeting, LibraryResult, MeetingPageRequest,
    SharedLibrary,
};
use crate::sync::transition::LibraryRole;

struct ReadResult<T> {
    values: T,
    hub_reachable: bool,
    error: Option<String>,
}

impl SharedLibrary {
    pub async fn dictations(&self, params: &SearchParams) -> LibraryResult<Vec<HistoryEntry>> {
        let context = self.context()?;
        let connection = crate::db::open_db_at(&context.db_path)
            .map_err(|error| LibraryError::internal("opening dictation library", error))?;
        let offset = params.offset;
        let limit = params.limit.clamp(1, 100);
        let target = offset.saturating_add(limit);
        let local = crate::db::list_visible_workflows(
            &connection,
            params.query.as_deref(),
            params.from.as_deref(),
            params.to.as_deref(),
            0,
            target,
        )
        .map_err(|error| LibraryError::internal("reading local dictations", error))?;
        let mut entries = BTreeMap::new();
        for workflow in local {
            let mut entry = HistoryEntry::from(workflow);
            entry.upload_state = SyncOutboxRepository::state_for(&connection, entry.id)
                .map_err(|error| LibraryError::internal("reading dictation upload state", error))?;
            if entry.payload_availability == PayloadAvailability::Unavailable {
                entry.payload_availability =
                    SyncOutboxRepository::payload_availability(&connection, entry.id)
                        .map_err(|error| {
                            LibraryError::internal("reading dictation payload state", error)
                        })?
                        .unwrap_or(PayloadAvailability::Unavailable);
            }
            entries.insert(entry.id, entry);
        }

        let result = match &context.role {
            LibraryRole::Standalone => ReadResult {
                values: entries,
                hub_reachable: false,
                error: None,
            },
            LibraryRole::HomeHub => {
                let shared = SharedLibraryRepository::page_dictations(
                    &connection,
                    params.query.as_deref(),
                    params.from.as_deref(),
                    params.to.as_deref(),
                    None,
                    target,
                )
                .map_err(|error| {
                    LibraryError::internal("reading authoritative dictations", error)
                })?;
                merge_dictations(&mut entries, shared);
                ReadResult {
                    values: entries,
                    hub_reachable: true,
                    error: None,
                }
            }
            LibraryRole::ConnectedDevice { hub } => {
                let mut cursor = None;
                let mut unmasked = 0usize;
                let mut seen_cursors = HashSet::new();
                let mut seen_ids = HashSet::new();
                let mut failure = None;
                while unmasked < target {
                    let page_limit = target.saturating_sub(unmasked).clamp(1, 100);
                    match context
                        .capabilities
                        .dictations()
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
                            let visible = page
                                .items
                                .into_iter()
                                .filter_map(|shared| {
                                    if !seen_ids.insert(shared.record_id) {
                                        return None;
                                    }
                                    let masked = SyncOutboxRepository::deletion_masks(
                                        &connection,
                                        shared.record_id,
                                        crate::sync::protocol::RecordKind::Dictation,
                                    )
                                    .map_err(|error| {
                                        LibraryError::internal(
                                            "reading dictation deletion mask",
                                            error,
                                        )
                                    });
                                    match masked {
                                        Ok(false) => Some(Ok(shared)),
                                        Ok(true) => None,
                                        Err(error) => Some(Err(error)),
                                    }
                                })
                                .collect::<LibraryResult<Vec<_>>>()?;
                            unmasked = unmasked.saturating_add(visible.len());
                            merge_dictations(&mut entries, visible);
                            match advance_remote_cursor(
                                page.next_cursor,
                                &mut seen_cursors,
                                "dictation",
                            ) {
                                Ok(Some(next)) => cursor = Some(next),
                                Ok(None) => break,
                                Err(error) => {
                                    self.observe(&context, false, Some(&error.to_string()))
                                        .await?;
                                    return Err(error);
                                }
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
                    ReadResult {
                        values: entries,
                        hub_reachable: false,
                        error: Some(error),
                    }
                } else {
                    ReadResult {
                        values: entries,
                        hub_reachable: true,
                        error: None,
                    }
                }
            }
        };
        let mut entries = result.values.into_values().collect::<Vec<_>>();
        entries.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| right.id.cmp(&left.id))
        });
        entries = entries.into_iter().skip(offset).take(limit).collect();
        self.observe(&context, result.hub_reachable, result.error.as_deref())
            .await?;
        Ok(entries)
    }

    pub async fn dictation(&self, id: RecordId) -> LibraryResult<HistoryEntry> {
        let context = self.context()?;
        let connection = crate::db::open_db_at(&context.db_path)
            .map_err(|error| LibraryError::internal("opening dictation library", error))?;
        if SyncOutboxRepository::deletion_masks(
            &connection,
            id,
            crate::sync::protocol::RecordKind::Dictation,
        )
        .map_err(|error| LibraryError::internal("reading dictation deletion mask", error))?
        {
            return Err(LibraryError::NotFound(format!(
                "Transcription {id} not found"
            )));
        }
        let local = crate::db::get_workflow_by_sync_id(&connection, id)
            .map_err(|error| LibraryError::internal("reading local dictation", error))?
            .map(HistoryEntry::from);
        match &context.role {
            LibraryRole::Standalone => {
                local.ok_or_else(|| LibraryError::NotFound(format!("Transcription {id} not found")))
            }
            LibraryRole::HomeHub => {
                let shared = SharedLibraryRepository::get(&connection, id)
                    .map_err(|error| {
                        LibraryError::internal("reading authoritative dictation", error)
                    })?
                    .map(|shared| shared_entry(shared, LibraryItemAccess::Shared));
                self.observe(&context, true, None).await?;
                Ok(overlay_dictation_payload(local.as_ref(), shared)
                    .or(local)
                    .ok_or_else(|| {
                        LibraryError::NotFound(format!("Transcription {id} not found"))
                    })?)
            }
            LibraryRole::ConnectedDevice { hub } => {
                let mut cursor = None;
                let mut seen_cursors = HashSet::new();
                let mut seen_ids = HashSet::new();
                loop {
                    match context
                        .capabilities
                        .dictations()
                        .page_dictations(hub, None, None, None, cursor.as_deref(), 100)
                        .await
                    {
                        Ok(page) => {
                            if let Some(shared) = page
                                .items
                                .into_iter()
                                .filter(|item| seen_ids.insert(item.record_id))
                                .find(|item| item.record_id == id)
                            {
                                self.observe(&context, true, None).await?;
                                return overlay_dictation_payload(
                                    local.as_ref(),
                                    Some(shared_entry(shared, LibraryItemAccess::Shared)),
                                )
                                .ok_or_else(|| {
                                    LibraryError::internal(
                                        "merging authoritative dictation",
                                        anyhow::anyhow!(
                                            "authoritative row disappeared during merge"
                                        ),
                                    )
                                });
                            }
                            match advance_remote_cursor(
                                page.next_cursor,
                                &mut seen_cursors,
                                "dictation",
                            ) {
                                Ok(Some(next)) => cursor = Some(next),
                                Ok(None) => {
                                    self.observe(&context, true, None).await?;
                                    return local.ok_or_else(|| {
                                        LibraryError::NotFound(format!(
                                            "Transcription {id} not found"
                                        ))
                                    });
                                }
                                Err(error) => {
                                    self.observe(&context, false, Some(&error.to_string()))
                                        .await?;
                                    return Err(error);
                                }
                            }
                        }
                        Err(error) => {
                            self.observe(&context, false, Some(&error.to_string()))
                                .await?;
                            return local
                                .map(|mut entry| {
                                    entry.offline = true;
                                    entry
                                })
                                .ok_or_else(|| {
                                    LibraryError::Unavailable(
                                        "Home Hub is unavailable; transcription lookup could not be completed"
                                            .into(),
                                    )
                                });
                        }
                    }
                }
            }
        }
    }

    pub async fn meetings(
        &self,
        request: MeetingPageRequest,
    ) -> LibraryResult<Vec<LibraryMeeting>> {
        let context = self.context()?;
        let connection = crate::db::open_db_at(&context.db_path)
            .map_err(|error| LibraryError::internal("opening meeting library", error))?;
        let limit = request
            .limit
            .clamp(1, crate::sync::protocol::MAX_MEETING_PAGE);
        let target = request.offset.saturating_add(limit);
        let local_fetch = if request.query.is_some() {
            usize::MAX
        } else {
            target
        };
        let mut entries = BTreeMap::new();
        for meeting in crate::db::meetings::MeetingRepository::list(&connection, local_fetch)
            .map_err(|error| LibraryError::internal("reading local meetings", error))?
        {
            if request.query.as_deref().is_some_and(|query| {
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
                crate::sync::protocol::RecordKind::Meeting,
            )
            .map_err(|error| LibraryError::internal("reading meeting upload state", error))?;
            let payload = SyncOutboxRepository::payload_availability(&connection, meeting.sync_id)
                .map_err(|error| LibraryError::internal("reading meeting payload state", error))?;
            entries.insert(meeting.sync_id, local_meeting(meeting, upload, payload));
        }

        let result = match &context.role {
            LibraryRole::Standalone => ReadResult {
                values: entries,
                hub_reachable: false,
                error: None,
            },
            LibraryRole::HomeHub => {
                let shared = SharedLibraryRepository::page_meetings(
                    &connection,
                    request.query.as_deref(),
                    None,
                    target,
                )
                .map_err(|error| LibraryError::internal("reading authoritative meetings", error))?;
                merge_meetings(&mut entries, shared);
                ReadResult {
                    values: entries,
                    hub_reachable: true,
                    error: None,
                }
            }
            LibraryRole::ConnectedDevice { hub } => {
                let mut cursor = None;
                let mut unmasked = 0usize;
                let mut seen_cursors = HashSet::new();
                let mut seen_ids = HashSet::new();
                let mut failure = None;
                while unmasked < target {
                    let page_limit = target
                        .saturating_sub(unmasked)
                        .clamp(1, crate::sync::protocol::MAX_MEETING_PAGE);
                    match context
                        .capabilities
                        .meetings()
                        .page_meetings(hub, request.query.as_deref(), cursor.as_deref(), page_limit)
                        .await
                    {
                        Ok(page) => {
                            let visible = page
                                .items
                                .into_iter()
                                .filter_map(|shared| {
                                    if !seen_ids.insert(shared.record_id) {
                                        return None;
                                    }
                                    let masked = SyncOutboxRepository::deletion_masks(
                                        &connection,
                                        shared.record_id,
                                        crate::sync::protocol::RecordKind::Meeting,
                                    )
                                    .map_err(|error| {
                                        LibraryError::internal(
                                            "reading meeting deletion mask",
                                            error,
                                        )
                                    });
                                    match masked {
                                        Ok(false) => Some(Ok(shared)),
                                        Ok(true) => None,
                                        Err(error) => Some(Err(error)),
                                    }
                                })
                                .collect::<LibraryResult<Vec<_>>>()?;
                            unmasked = unmasked.saturating_add(visible.len());
                            merge_meetings(&mut entries, visible);
                            match advance_remote_cursor(
                                page.next_cursor,
                                &mut seen_cursors,
                                "meeting",
                            ) {
                                Ok(Some(next)) => cursor = Some(next),
                                Ok(None) => break,
                                Err(error) => {
                                    self.observe(&context, false, Some(&error.to_string()))
                                        .await?;
                                    return Err(error);
                                }
                            }
                        }
                        Err(error) => {
                            failure = Some(error.to_string());
                            break;
                        }
                    }
                }
                if let Some(error) = failure {
                    entries.retain(|_, value| value.access.source() == "local");
                    for value in entries.values_mut() {
                        value.access = if SyncOutboxRepository::may_have_reached_hub(
                            &connection,
                            value.id,
                            crate::sync::protocol::RecordKind::Meeting,
                        )
                        .map_err(|error| {
                            LibraryError::internal("reading meeting publication state", error)
                        })? {
                            LibraryItemAccess::LocalOfflineReadOnly
                        } else {
                            LibraryItemAccess::LocalOffline
                        };
                    }
                    ReadResult {
                        values: entries,
                        hub_reachable: false,
                        error: Some(error),
                    }
                } else {
                    ReadResult {
                        values: entries,
                        hub_reachable: true,
                        error: None,
                    }
                }
            }
        };
        let mut meetings = result.values.into_values().collect::<Vec<_>>();
        meetings.sort_by(|left, right| {
            right
                .started_at
                .cmp(&left.started_at)
                .then_with(|| right.id.cmp(&left.id))
        });
        meetings = meetings
            .into_iter()
            .skip(request.offset)
            .take(limit)
            .collect();
        self.observe(&context, result.hub_reachable, result.error.as_deref())
            .await?;
        Ok(meetings)
    }

    pub async fn meeting(&self, id: RecordId) -> LibraryResult<LibraryMeeting> {
        let context = self.context()?;
        let connection = crate::db::open_db_at(&context.db_path)
            .map_err(|error| LibraryError::internal("opening meeting library", error))?;
        if SyncOutboxRepository::deletion_masks(
            &connection,
            id,
            crate::sync::protocol::RecordKind::Meeting,
        )
        .map_err(|error| LibraryError::internal("reading meeting deletion mask", error))?
        {
            return Err(LibraryError::NotFound(format!("Meeting {id} not found")));
        }
        let local = crate::db::meetings::MeetingRepository::get_by_sync_id(&connection, id)
            .map_err(|error| LibraryError::internal("reading local meeting", error))?
            .map(|meeting| {
                let upload = SyncOutboxRepository::state_for_kind(
                    &connection,
                    meeting.sync_id,
                    crate::sync::protocol::RecordKind::Meeting,
                )
                .map_err(|error| LibraryError::internal("reading meeting upload state", error))?;
                let payload =
                    SyncOutboxRepository::payload_availability(&connection, meeting.sync_id)
                        .map_err(|error| {
                            LibraryError::internal("reading meeting payload state", error)
                        })?;
                Ok::<_, LibraryError>(local_meeting(meeting, upload, payload))
            })
            .transpose()?;
        match &context.role {
            LibraryRole::Standalone => {
                local.ok_or_else(|| LibraryError::NotFound(format!("Meeting {id} not found")))
            }
            LibraryRole::HomeHub => {
                let shared = SharedLibraryRepository::get_meeting(&connection, id)
                    .map_err(|error| {
                        LibraryError::internal("reading authoritative meeting", error)
                    })?
                    .map(shared_meeting);
                self.observe(&context, true, None).await?;
                shared
                    .map(|shared| overlay_local_payload(local.as_ref(), shared))
                    .or(local)
                    .ok_or_else(|| LibraryError::NotFound(format!("Meeting {id} not found")))
            }
            LibraryRole::ConnectedDevice { hub } => {
                match context.capabilities.meetings().meeting(hub, id).await {
                    Ok(Some(shared)) => {
                        self.observe(&context, true, None).await?;
                        Ok(overlay_local_payload(
                            local.as_ref(),
                            shared_meeting(shared),
                        ))
                    }
                    Ok(None) => {
                        self.observe(&context, true, None).await?;
                        local.ok_or_else(|| {
                            LibraryError::NotFound(format!("Meeting {id} not found"))
                        })
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
                                    crate::sync::protocol::RecordKind::Meeting,
                                )
                            })
                            .transpose()
                            .map_err(|error| {
                                LibraryError::internal("reading meeting publication state", error)
                            })?
                            .unwrap_or(false);
                        local
                            .map(|mut meeting| {
                                meeting.access = if read_only {
                                    LibraryItemAccess::LocalOfflineReadOnly
                                } else {
                                    LibraryItemAccess::LocalOffline
                                };
                                meeting
                            })
                            .ok_or_else(|| {
                                LibraryError::Unavailable(
                                    "Home Hub is unavailable; meeting lookup could not be completed"
                                        .into(),
                                )
                            })
                    }
                }
            }
        }
    }
}

fn merge_dictations(
    entries: &mut BTreeMap<RecordId, HistoryEntry>,
    shared: impl IntoIterator<Item = crate::sync::protocol::SharedDictation>,
) {
    for shared in shared {
        let id = shared.record_id;
        let mut entry = shared_entry(shared, LibraryItemAccess::Shared);
        if let Some(local) = entries.get(&id) {
            entry.payload_availability =
                merge_payload_availability(local.payload_availability, entry.payload_availability);
        }
        entries.insert(id, entry);
    }
}

fn overlay_dictation_payload(
    local: Option<&HistoryEntry>,
    shared: Option<HistoryEntry>,
) -> Option<HistoryEntry> {
    shared.map(|mut shared| {
        if let Some(local) = local {
            shared.payload_availability =
                merge_payload_availability(local.payload_availability, shared.payload_availability);
        }
        shared
    })
}

fn merge_meetings(
    entries: &mut BTreeMap<RecordId, LibraryMeeting>,
    shared: impl IntoIterator<Item = crate::sync::protocol::SharedMeeting>,
) {
    for meeting in shared {
        let id = meeting.record_id;
        let shared = overlay_local_payload(entries.get(&id), shared_meeting(meeting));
        entries.insert(id, shared);
    }
}

pub(super) fn shared_artifact(
    value: crate::sync::protocol::SharedArtifact,
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

pub(super) fn shared_meeting(value: crate::sync::protocol::SharedMeeting) -> LibraryMeeting {
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

pub(super) fn overlay_local_payload(
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
    shared: crate::sync::protocol::SharedDictation,
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

fn contains_case_insensitive(value: &str, query: &str) -> bool {
    value.to_lowercase().contains(&query.to_lowercase())
}

fn advance_remote_cursor(
    next: Option<String>,
    seen: &mut HashSet<String>,
    resource: &str,
) -> LibraryResult<Option<String>> {
    let Some(next) = next else {
        return Ok(None);
    };
    if !seen.insert(next.clone()) {
        return Err(LibraryError::Unavailable(format!(
            "Home Hub returned a repeated {resource} page cursor"
        )));
    }
    Ok(Some(next))
}
