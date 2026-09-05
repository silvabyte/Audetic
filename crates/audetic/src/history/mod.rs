//! History module for transcription history operations.
//!
//! This module provides the core business logic for searching, retrieving,
//! and managing transcription history. It is used by both the CLI and REST API.

use crate::db::{self, Workflow, WorkflowData};
use anyhow::{anyhow, Result};
use audetic_core::sync::{DeviceId, PayloadAvailability, RecordId, UploadState};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Parameters for searching transcription history.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SearchParams {
    /// Text query to filter transcriptions
    pub query: Option<String>,
    /// Filter by start date (YYYY-MM-DD format)
    pub from: Option<String>,
    /// Filter by end date (YYYY-MM-DD format)
    pub to: Option<String>,
    /// Maximum number of results
    pub limit: usize,
    /// Number of canonical results to skip.
    pub offset: usize,
}

impl SearchParams {
    pub fn new() -> Self {
        Self {
            limit: 20,
            ..Default::default()
        }
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    pub fn with_query(mut self, query: impl Into<String>) -> Self {
        self.query = Some(query.into());
        self
    }

    pub fn with_date_range(mut self, from: Option<String>, to: Option<String>) -> Self {
        self.from = from;
        self.to = to;
        self
    }

    /// Returns true if no filters are specified (only limit)
    pub fn has_filters(&self) -> bool {
        self.query.is_some() || self.from.is_some() || self.to.is_some()
    }
}

/// A single history entry with formatted display data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HistorySource {
    Local,
    Shared,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HistoryEntry {
    pub id: RecordId,
    pub text: String,
    pub created_at: String,
    pub origin_device_id: DeviceId,
    pub source: HistorySource,
    pub upload_state: Option<UploadState>,
    pub payload_availability: PayloadAvailability,
    pub offline: bool,
    pub read_only: bool,
}

impl From<Workflow> for HistoryEntry {
    fn from(workflow: Workflow) -> Self {
        let (text, audio_path) = match workflow.data {
            WorkflowData::VoiceToText(data) => (data.text, data.audio_path),
        };
        Self {
            id: workflow.sync_id.expect("persisted workflows have UUIDs"),
            text,
            created_at: workflow
                .created_at
                .map(|value| {
                    chrono::NaiveDateTime::parse_from_str(&value, "%Y-%m-%d %H:%M:%S")
                        .map(|timestamp| {
                            timestamp
                                .and_utc()
                                .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
                        })
                        .unwrap_or(value)
                })
                .unwrap_or_else(|| "Unknown".to_string()),
            origin_device_id: workflow
                .origin_device_id
                .expect("persisted workflows have origins"),
            source: HistorySource::Local,
            upload_state: None,
            payload_availability: if std::path::Path::new(&audio_path).exists() {
                PayloadAvailability::Available
            } else {
                PayloadAvailability::Unavailable
            },
            offline: false,
            read_only: false,
        }
    }
}

/// Search transcription history with optional filters.
///
/// If no filters are specified, returns recent transcriptions.
pub fn search(params: &SearchParams) -> Result<Vec<HistoryEntry>> {
    let conn = db::open_db()?;
    let workflows = db::list_visible_workflows(
        &conn,
        params.query.as_deref(),
        params.from.as_deref(),
        params.to.as_deref(),
        params.offset,
        params.limit.clamp(1, 100),
    )?;

    Ok(workflows.into_iter().map(HistoryEntry::from).collect())
}

/// Get recent transcription history.
pub fn get_recent(limit: usize) -> Result<Vec<HistoryEntry>> {
    let conn = db::open_db()?;
    let workflows = db::get_recent_workflows(&conn, limit)?;
    Ok(workflows.into_iter().map(HistoryEntry::from).collect())
}

/// Get a single transcription by ID.
pub fn get_by_id(id: RecordId) -> Result<Option<HistoryEntry>> {
    let conn = db::open_db()?;
    Ok(db::get_workflow_by_sync_id(&conn, id)?.map(HistoryEntry::from))
}

/// Get the text content of a transcription by ID.
///
/// Returns the raw text, suitable for copying to clipboard or returning via API.
pub fn get_text_by_id(id: RecordId) -> Result<String> {
    get_by_id(id)?
        .map(|entry| entry.text)
        .ok_or_else(|| anyhow!("Workflow with ID {} not found", id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_params_has_filters() {
        let params = SearchParams::new();
        assert!(!params.has_filters());

        let params = SearchParams::new().with_query("test");
        assert!(params.has_filters());

        let params = SearchParams::new().with_date_range(Some("2024-01-01".into()), None);
        assert!(params.has_filters());
    }

    #[test]
    fn test_search_params_builder() {
        let params = SearchParams::new()
            .with_limit(50)
            .with_query("hello")
            .with_date_range(Some("2024-01-01".into()), Some("2024-12-31".into()));

        assert_eq!(params.limit, 50);
        assert_eq!(params.query, Some("hello".to_string()));
        assert_eq!(params.from, Some("2024-01-01".to_string()));
        assert_eq!(params.to, Some("2024-12-31".to_string()));
    }
}
