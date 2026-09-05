//! Private, versioned SQLite codecs for Library Cache and change-feed rows.
//!
//! These representations deliberately do not serialize protocol DTOs directly.
//! Adding or changing an HTTP field therefore cannot reinterpret durable rows.

use anyhow::{bail, Context, Result};
use audetic_core::sync::PayloadAvailability;
use serde::{Deserialize, Serialize};

use crate::sync::protocol::{
    ChangeCursor, ChangeOperation, ChangeRecord, CompletedArtifactPayload,
    CompletedArtifactSnapshot, DictationPayload, DictationSnapshot, MeetingPayload,
    MeetingSnapshot, RecordKind, RecordingPayloadDescriptor, Snapshot,
};

pub(super) const STORED_CODEC_V1: u16 = 1;

#[derive(Deserialize, Serialize)]
struct StoredChangeV1 {
    operation: StoredOperationV1,
    kind: StoredKindV1,
    record_id: String,
    origin_device_id: Option<String>,
    authoritative_revision: u64,
    snapshot: Option<StoredSnapshotV1>,
    changed_at: String,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum StoredOperationV1 {
    Upsert,
    Delete,
    PayloadAvailability,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum StoredKindV1 {
    Dictation,
    Meeting,
    Artifact,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "record_type", rename_all = "snake_case")]
enum StoredSnapshotV1 {
    Dictation {
        record_id: String,
        origin_device_id: String,
        local_version: u64,
        created_at: String,
        updated_at: String,
        text: String,
        recording_payload: StoredRecordingPayloadV1,
    },
    Meeting {
        record_id: String,
        origin_device_id: String,
        local_version: u64,
        created_at: String,
        updated_at: String,
        title: Option<String>,
        title_source: Option<String>,
        title_version: u64,
        source_filename: Option<String>,
        transcript_text: String,
        transcript_segments_json: Option<String>,
        duration_seconds: u64,
        status: String,
        completed_at: String,
        recording_payload: StoredRecordingPayloadV1,
    },
    Artifact {
        record_id: String,
        parent_record_id: String,
        origin_device_id: String,
        local_version: u64,
        created_at: String,
        updated_at: String,
        artifact_kind: String,
        title: String,
        template_id: Option<String>,
        agent_profile_name: Option<String>,
        content_markdown: String,
        completed_at: String,
    },
}

#[derive(Deserialize, Serialize)]
struct StoredRecordingPayloadV1 {
    checksum: Option<String>,
    byte_size: Option<u64>,
    media_type: Option<String>,
    availability: StoredAvailabilityV1,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum StoredAvailabilityV1 {
    Available,
    Pending,
    Unavailable,
    NeedsAttention,
}

pub(super) fn encode_change(change: &ChangeRecord) -> Result<String> {
    let stored = StoredChangeV1 {
        operation: change.operation.into(),
        kind: change.kind.into(),
        record_id: change.record_id.to_string(),
        origin_device_id: change.origin_device_id.map(|value| value.to_string()),
        authoritative_revision: change.authoritative_revision,
        snapshot: change
            .snapshot
            .as_ref()
            .map(try_store_snapshot)
            .transpose()?,
        changed_at: change.changed_at.clone(),
    };
    serde_json::to_string(&stored).context("encoding stored Library change v1")
}

pub(super) fn decode_change(
    codec_version: u16,
    cursor: ChangeCursor,
    json: &str,
) -> Result<ChangeRecord> {
    require_v1(codec_version)?;
    let stored: StoredChangeV1 =
        serde_json::from_str(json).context("decoding stored Library change v1")?;
    Ok(ChangeRecord {
        cursor,
        operation: stored.operation.into(),
        kind: stored.kind.into(),
        record_id: parse_id(&stored.record_id, "stored change record ID")?,
        origin_device_id: stored
            .origin_device_id
            .as_deref()
            .map(|value| parse_id(value, "stored change origin device ID"))
            .transpose()?,
        authoritative_revision: stored.authoritative_revision,
        snapshot: stored.snapshot.map(restore_snapshot).transpose()?,
        changed_at: stored.changed_at,
    })
}

pub(super) fn encode_snapshot(snapshot: &Snapshot) -> Result<String> {
    serde_json::to_string(&try_store_snapshot(snapshot)?)
        .context("encoding stored Library Cache item v1")
}

pub(super) fn decode_snapshot(codec_version: u16, json: &str) -> Result<Snapshot> {
    require_v1(codec_version)?;
    let stored: StoredSnapshotV1 =
        serde_json::from_str(json).context("decoding stored Library Cache item v1")?;
    restore_snapshot(stored)
}

fn try_store_snapshot(snapshot: &Snapshot) -> Result<StoredSnapshotV1> {
    match snapshot {
        Snapshot::Dictation(value) => Ok(StoredSnapshotV1::Dictation {
            record_id: value.record_id.to_string(),
            origin_device_id: value.origin_device_id.to_string(),
            local_version: value.local_version,
            created_at: value.created_at.clone(),
            updated_at: value.updated_at.clone(),
            text: value.payload.text.clone(),
            recording_payload: (&value.payload.recording_payload).into(),
        }),
        Snapshot::Meeting(value) => Ok(StoredSnapshotV1::Meeting {
            record_id: value.record_id.to_string(),
            origin_device_id: value.origin_device_id.to_string(),
            local_version: value.local_version,
            created_at: value.created_at.clone(),
            updated_at: value.updated_at.clone(),
            title: value.payload.title.clone(),
            title_source: value.payload.title_source.clone(),
            title_version: value.payload.title_version,
            source_filename: value.payload.source_filename.clone(),
            transcript_text: value.payload.transcript_text.clone(),
            transcript_segments_json: value
                .payload
                .transcript_segments
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .context("encoding stored meeting transcript segments v1")?,
            duration_seconds: value.payload.duration_seconds,
            status: value.payload.status.clone(),
            completed_at: value.payload.completed_at.clone(),
            recording_payload: (&value.payload.recording_payload).into(),
        }),
        Snapshot::Artifact(value) => Ok(StoredSnapshotV1::Artifact {
            record_id: value.record_id.to_string(),
            parent_record_id: value.parent_record_id.to_string(),
            origin_device_id: value.origin_device_id.to_string(),
            local_version: value.local_version,
            created_at: value.created_at.clone(),
            updated_at: value.updated_at.clone(),
            artifact_kind: value.payload.artifact_kind.clone(),
            title: value.payload.title.clone(),
            template_id: value.payload.template_id.clone(),
            agent_profile_name: value.payload.agent_profile_name.clone(),
            content_markdown: value.payload.content_markdown.clone(),
            completed_at: value.payload.completed_at.clone(),
        }),
        Snapshot::Delete(_) => bail!("deletion snapshots are not stored as cache items"),
    }
}

fn restore_snapshot(stored: StoredSnapshotV1) -> Result<Snapshot> {
    match stored {
        StoredSnapshotV1::Dictation {
            record_id,
            origin_device_id,
            local_version,
            created_at,
            updated_at,
            text,
            recording_payload,
        } => Ok(Snapshot::Dictation(DictationSnapshot {
            kind: RecordKind::Dictation,
            schema_version: 1,
            record_id: parse_id(&record_id, "stored dictation record ID")?,
            origin_device_id: parse_id(&origin_device_id, "stored dictation origin device ID")?,
            local_version,
            created_at,
            updated_at,
            payload: DictationPayload {
                text,
                recording_payload: recording_payload.into(),
            },
        })),
        StoredSnapshotV1::Meeting {
            record_id,
            origin_device_id,
            local_version,
            created_at,
            updated_at,
            title,
            title_source,
            title_version,
            source_filename,
            transcript_text,
            transcript_segments_json,
            duration_seconds,
            status,
            completed_at,
            recording_payload,
        } => Ok(Snapshot::Meeting(MeetingSnapshot {
            kind: RecordKind::Meeting,
            schema_version: 1,
            record_id: parse_id(&record_id, "stored meeting record ID")?,
            origin_device_id: parse_id(&origin_device_id, "stored meeting origin device ID")?,
            local_version,
            created_at,
            updated_at,
            payload: MeetingPayload {
                title,
                title_source,
                title_version,
                source_filename,
                transcript_text,
                transcript_segments: transcript_segments_json
                    .as_deref()
                    .map(serde_json::from_str)
                    .transpose()
                    .context("decoding stored meeting transcript segments v1")?,
                duration_seconds,
                status,
                completed_at,
                recording_payload: recording_payload.into(),
            },
        })),
        StoredSnapshotV1::Artifact {
            record_id,
            parent_record_id,
            origin_device_id,
            local_version,
            created_at,
            updated_at,
            artifact_kind,
            title,
            template_id,
            agent_profile_name,
            content_markdown,
            completed_at,
        } => Ok(Snapshot::Artifact(CompletedArtifactSnapshot {
            kind: RecordKind::Artifact,
            schema_version: 1,
            record_id: parse_id(&record_id, "stored artifact record ID")?,
            parent_record_id: parse_id(&parent_record_id, "stored artifact parent record ID")?,
            origin_device_id: parse_id(&origin_device_id, "stored artifact origin device ID")?,
            local_version,
            created_at,
            updated_at,
            payload: CompletedArtifactPayload {
                artifact_kind,
                title,
                template_id,
                agent_profile_name,
                content_markdown,
                completed_at,
            },
        })),
    }
}

fn require_v1(codec_version: u16) -> Result<()> {
    if codec_version == STORED_CODEC_V1 {
        Ok(())
    } else {
        bail!("unsupported stored Library codec version {codec_version}")
    }
}

fn parse_id<T>(value: &str, field: &str) -> Result<T>
where
    T: std::str::FromStr<Err = String>,
{
    value
        .parse()
        .map_err(anyhow::Error::msg)
        .with_context(|| format!("invalid {field}"))
}

impl From<ChangeOperation> for StoredOperationV1 {
    fn from(value: ChangeOperation) -> Self {
        match value {
            ChangeOperation::Upsert => Self::Upsert,
            ChangeOperation::Delete => Self::Delete,
            ChangeOperation::PayloadAvailability => Self::PayloadAvailability,
        }
    }
}

impl From<StoredOperationV1> for ChangeOperation {
    fn from(value: StoredOperationV1) -> Self {
        match value {
            StoredOperationV1::Upsert => Self::Upsert,
            StoredOperationV1::Delete => Self::Delete,
            StoredOperationV1::PayloadAvailability => Self::PayloadAvailability,
        }
    }
}

impl From<RecordKind> for StoredKindV1 {
    fn from(value: RecordKind) -> Self {
        match value {
            RecordKind::Dictation => Self::Dictation,
            RecordKind::Meeting => Self::Meeting,
            RecordKind::Artifact => Self::Artifact,
        }
    }
}

impl From<StoredKindV1> for RecordKind {
    fn from(value: StoredKindV1) -> Self {
        match value {
            StoredKindV1::Dictation => Self::Dictation,
            StoredKindV1::Meeting => Self::Meeting,
            StoredKindV1::Artifact => Self::Artifact,
        }
    }
}

impl From<&RecordingPayloadDescriptor> for StoredRecordingPayloadV1 {
    fn from(value: &RecordingPayloadDescriptor) -> Self {
        Self {
            checksum: value.checksum.clone(),
            byte_size: value.byte_size,
            media_type: value.media_type.clone(),
            availability: match value.availability {
                PayloadAvailability::Available => StoredAvailabilityV1::Available,
                PayloadAvailability::Pending => StoredAvailabilityV1::Pending,
                PayloadAvailability::Unavailable => StoredAvailabilityV1::Unavailable,
                PayloadAvailability::NeedsAttention => StoredAvailabilityV1::NeedsAttention,
            },
        }
    }
}

impl From<StoredRecordingPayloadV1> for RecordingPayloadDescriptor {
    fn from(value: StoredRecordingPayloadV1) -> Self {
        Self {
            checksum: value.checksum,
            byte_size: value.byte_size,
            media_type: value.media_type,
            availability: match value.availability {
                StoredAvailabilityV1::Available => PayloadAvailability::Available,
                StoredAvailabilityV1::Pending => PayloadAvailability::Pending,
                StoredAvailabilityV1::Unavailable => PayloadAvailability::Unavailable,
                StoredAvailabilityV1::NeedsAttention => PayloadAvailability::NeedsAttention,
            },
        }
    }
}
