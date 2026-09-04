use anyhow::{Context, Result};
use audetic_core::sync::RecordId;
use base64::Engine;
use serde::{Deserialize, Serialize};

use std::path::PathBuf;

use crate::db::shared_library::{ApplySnapshotError, SharedLibraryRepository};

use super::protocol::{
    DictationPage, MeetingPage, MeetingTitlePatch, RecordKind, SharedMeeting, Snapshot,
    SnapshotBatchResponse, SnapshotDisposition, SnapshotResult, MAX_DICTATION_PAGE,
    MAX_MEETING_PAGE, MAX_SNAPSHOT_BATCH,
};

/// Authoritative validation/application boundary used by both the HTTP hub
/// router and a Home Hub's own outbox worker.
#[derive(Clone, Debug)]
pub struct HubLibrary {
    db_path: PathBuf,
}

impl HubLibrary {
    pub fn new(db_path: PathBuf) -> Self {
        Self { db_path }
    }

    pub fn apply_snapshots<T: Into<Snapshot>>(
        &self,
        snapshots: Vec<T>,
    ) -> Result<SnapshotBatchResponse> {
        if snapshots.is_empty() || snapshots.len() > MAX_SNAPSHOT_BATCH {
            anyhow::bail!("snapshot batch must contain 1..={MAX_SNAPSHOT_BATCH} items");
        }
        let mut connection = crate::db::open_db_at(&self.db_path)?;
        let mut results = Vec::with_capacity(snapshots.len());
        for snapshot in snapshots {
            let snapshot = snapshot.into();
            let record_id = snapshot.record_id();
            let result = match canonicalize_snapshot(snapshot) {
                Ok(snapshot) => match SharedLibraryRepository::apply(&mut connection, &snapshot) {
                    Ok(accepted) => SnapshotResult {
                        record_id: snapshot.record_id(),
                        disposition: SnapshotDisposition::Accepted,
                        authoritative_revision: Some(accepted.revision),
                        error_code: None,
                        message: None,
                    },
                    Err(error) => rejection(snapshot.record_id(), &error),
                },
                Err(message) => SnapshotResult {
                    record_id,
                    disposition: SnapshotDisposition::Rejected,
                    authoritative_revision: None,
                    error_code: Some("invalid_snapshot".to_owned()),
                    message: Some(message),
                },
            };
            results.push(result);
        }
        Ok(SnapshotBatchResponse { results })
    }

    pub fn page_dictations(
        &self,
        query: Option<&str>,
        from: Option<&str>,
        to: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<DictationPage> {
        let cursor = cursor.map(decode_cursor).transpose()?;
        let limit = limit.clamp(1, MAX_DICTATION_PAGE);
        let connection = crate::db::open_db_at(&self.db_path)?;
        let mut items = SharedLibraryRepository::page_dictations(
            &connection,
            query,
            from,
            to,
            cursor
                .as_ref()
                .map(|cursor| (cursor.created_at.as_str(), cursor.record_id)),
            limit + 1,
        )?;
        let has_more = items.len() > limit;
        items.truncate(limit);
        let next_cursor = if has_more {
            items
                .last()
                .map(|item| {
                    encode_cursor(&DictationCursor {
                        created_at: item.created_at.clone(),
                        record_id: item.record_id,
                    })
                })
                .transpose()?
        } else {
            None
        };
        Ok(DictationPage { next_cursor, items })
    }

    pub fn page_meetings(
        &self,
        query: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<MeetingPage> {
        let cursor = cursor.map(decode_meeting_cursor).transpose()?;
        let limit = limit.clamp(1, MAX_MEETING_PAGE);
        let connection = crate::db::open_db_at(&self.db_path)?;
        let mut items = SharedLibraryRepository::page_meetings(
            &connection,
            query,
            cursor
                .as_ref()
                .map(|value| (value.created_at.as_str(), value.record_id)),
            limit + 1,
        )?;
        let has_more = items.len() > limit;
        items.truncate(limit);
        let next_cursor = has_more
            .then(|| items.last())
            .flatten()
            .map(|item| {
                encode_cursor(&DictationCursor {
                    created_at: item.created_at.clone(),
                    record_id: item.record_id,
                })
            })
            .transpose()?;
        Ok(MeetingPage { items, next_cursor })
    }

    pub fn meeting(&self, record_id: RecordId) -> Result<Option<SharedMeeting>> {
        let connection = crate::db::open_db_at(&self.db_path)?;
        SharedLibraryRepository::get_meeting(&connection, record_id)
    }

    pub fn update_meeting_title(
        &self,
        record_id: RecordId,
        patch: &MeetingTitlePatch,
    ) -> Result<Option<SharedMeeting>> {
        let mut connection = crate::db::open_db_at(&self.db_path)?;
        SharedLibraryRepository::compare_and_set_meeting_title(&mut connection, record_id, patch)
    }

    pub fn delete(
        &self,
        record_id: RecordId,
        kind: RecordKind,
    ) -> std::result::Result<crate::db::shared_library::ApplyResult, ApplySnapshotError> {
        let mut connection =
            crate::db::open_db_at(&self.db_path).map_err(ApplySnapshotError::Database)?;
        SharedLibraryRepository::apply_delete(
            &mut connection,
            record_id,
            kind,
            &chrono::Utc::now().to_rfc3339(),
        )
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct DictationCursor {
    created_at: String,
    record_id: RecordId,
}

fn encode_cursor(cursor: &DictationCursor) -> Result<String> {
    let json = serde_json::to_vec(cursor).context("serializing dictation page cursor")?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json))
}

fn decode_cursor(value: &str) -> Result<DictationCursor> {
    let json = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .context("invalid dictation page cursor encoding")?;
    let cursor: DictationCursor =
        serde_json::from_slice(&json).context("invalid dictation page cursor payload")?;
    let canonical = canonical_timestamp(&cursor.created_at)
        .map_err(|message| anyhow::anyhow!("invalid dictation page cursor: {message}"))?;
    if canonical != cursor.created_at {
        anyhow::bail!("invalid dictation page cursor timestamp");
    }
    Ok(cursor)
}

fn decode_meeting_cursor(value: &str) -> Result<DictationCursor> {
    decode_cursor(value)
}

fn canonicalize_snapshot(mut snapshot: Snapshot) -> std::result::Result<Snapshot, String> {
    let (kind, declared, schema, version, created_at, updated_at) = match &mut snapshot {
        Snapshot::Dictation(value) => (
            RecordKind::Dictation,
            value.kind,
            value.schema_version,
            value.local_version,
            &mut value.created_at,
            &mut value.updated_at,
        ),
        Snapshot::Meeting(value) => (
            RecordKind::Meeting,
            value.kind,
            value.schema_version,
            value.local_version,
            &mut value.created_at,
            &mut value.updated_at,
        ),
        Snapshot::Artifact(value) => (
            RecordKind::Artifact,
            value.kind,
            value.schema_version,
            value.local_version,
            &mut value.created_at,
            &mut value.updated_at,
        ),
    };
    if declared != kind {
        return Err("snapshot kind does not match its payload".to_owned());
    }
    if schema != 1 {
        return Err(format!("unsupported {kind:?} schema version {schema}"));
    }
    if version == 0 {
        return Err("local version must be positive".to_owned());
    }
    *created_at =
        canonical_timestamp(created_at).map_err(|_| "created_at must be RFC 3339".to_owned())?;
    *updated_at =
        canonical_timestamp(updated_at).map_err(|_| "updated_at must be RFC 3339".to_owned())?;
    match &mut snapshot {
        Snapshot::Dictation(value) if value.payload.text.trim().is_empty() => {
            return Err("dictation text must not be empty".to_owned())
        }
        Snapshot::Meeting(value) => {
            if value.payload.status != "completed"
                || value.payload.transcript_text.trim().is_empty()
            {
                return Err("meeting snapshot must be a completed transcript".to_owned());
            }
            value.payload.completed_at = canonical_timestamp(&value.payload.completed_at)
                .map_err(|_| "completed_at must be RFC 3339".to_owned())?;
        }
        Snapshot::Artifact(value) => {
            if value.payload.content_markdown.trim().is_empty() {
                return Err("completed artifact content must not be empty".to_owned());
            }
            value.payload.completed_at = canonical_timestamp(&value.payload.completed_at)
                .map_err(|_| "completed_at must be RFC 3339".to_owned())?;
        }
        Snapshot::Dictation(_) => {}
    }
    Ok(snapshot)
}

fn canonical_timestamp(value: &str) -> std::result::Result<String, String> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|timestamp| {
            timestamp
                .with_timezone(&chrono::Utc)
                .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
        })
        .map_err(|error| error.to_string())
}

fn rejection(record_id: RecordId, error: &ApplySnapshotError) -> SnapshotResult {
    let code = match error {
        ApplySnapshotError::Tombstoned => "tombstoned",
        ApplySnapshotError::KindChanged => "kind_changed",
        ApplySnapshotError::OriginChanged => "origin_changed",
        ApplySnapshotError::VersionConflict => "version_conflict",
        ApplySnapshotError::ParentUnavailable => "parent_unavailable",
        ApplySnapshotError::Sqlite(_) | ApplySnapshotError::Json(_) => "storage_error",
        ApplySnapshotError::Database(_) => "storage_error",
    };
    SnapshotResult {
        record_id,
        disposition: SnapshotDisposition::Rejected,
        authoritative_revision: None,
        error_code: Some(code.to_owned()),
        message: Some(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::protocol::DictationSnapshot;
    use audetic_core::sync::DeviceId;

    fn snapshot(record: u128, hour: u32) -> DictationSnapshot {
        DictationSnapshot {
            kind: RecordKind::Dictation,
            schema_version: 1,
            record_id: RecordId::from_uuid(uuid::Uuid::from_u128(record)),
            origin_device_id: DeviceId::from_uuid(uuid::Uuid::from_u128(1000 + record)),
            local_version: 1,
            created_at: format!("2026-09-04T{hour:02}:00:00+00:00"),
            updated_at: format!("2026-09-04T{hour:02}:00:00+00:00"),
            payload: super::super::protocol::DictationPayload {
                text: format!("record-{record}"),
            },
        }
    }

    #[test]
    fn keyset_cursor_does_not_skip_existing_rows_when_a_newer_row_arrives() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("hub.db");
        crate::db::migrate_db_at(&path).unwrap();
        let library = HubLibrary::new(path);
        library
            .apply_snapshots(vec![snapshot(1, 10), snapshot(2, 11), snapshot(3, 12)])
            .unwrap();

        let first = library.page_dictations(None, None, None, None, 2).unwrap();
        assert_eq!(
            first
                .items
                .iter()
                .map(|item| item.record_id)
                .collect::<Vec<_>>(),
            vec![snapshot(3, 12).record_id, snapshot(2, 11).record_id]
        );

        library.apply_snapshots(vec![snapshot(4, 13)]).unwrap();
        let second = library
            .page_dictations(None, None, None, first.next_cursor.as_deref(), 2)
            .unwrap();
        assert_eq!(
            second
                .items
                .iter()
                .map(|item| item.record_id)
                .collect::<Vec<_>>(),
            vec![snapshot(1, 10).record_id]
        );
        assert!(second.next_cursor.is_none());
    }

    #[test]
    fn snapshot_timestamps_are_canonicalized_before_storage_and_cursoring() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("hub.db");
        crate::db::migrate_db_at(&path).unwrap();
        let library = HubLibrary::new(path);
        let mut item = snapshot(1, 10);
        item.created_at = "2026-09-04T12:00:00+02:00".into();
        item.updated_at = item.created_at.clone();
        library.apply_snapshots(vec![item]).unwrap();

        let page = library.page_dictations(None, None, None, None, 1).unwrap();
        assert_eq!(page.items[0].created_at, "2026-09-04T10:00:00.000000000Z");
    }

    #[test]
    fn canonical_timestamps_sort_fractional_seconds_chronologically() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("hub.db");
        crate::db::migrate_db_at(&path).unwrap();
        let library = HubLibrary::new(path);
        let whole_second = snapshot(1, 10);
        let mut later_fraction = snapshot(2, 10);
        later_fraction.created_at = "2026-09-04T10:00:00.5Z".into();
        later_fraction.updated_at = later_fraction.created_at.clone();
        library
            .apply_snapshots(vec![whole_second, later_fraction])
            .unwrap();

        let page = library.page_dictations(None, None, None, None, 2).unwrap();
        assert_eq!(
            page.items
                .iter()
                .map(|item| item.record_id)
                .collect::<Vec<_>>(),
            vec![snapshot(2, 10).record_id, snapshot(1, 10).record_id]
        );
    }
}
