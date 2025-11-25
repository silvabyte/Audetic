//! History module for transcription history operations.
//!
//! This module provides the core business logic for searching, retrieving,
//! and managing transcription history. It is used by both the CLI and REST API.

use crate::db::{self, Workflow, WorkflowData};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: i64,
    pub text: String,
    pub audio_path: String,
    pub created_at: String,
}

impl From<Workflow> for HistoryEntry {
    fn from(workflow: Workflow) -> Self {
        let (text, audio_path) = match workflow.data {
            WorkflowData::VoiceToText(data) => (data.text, data.audio_path),
        };
        Self {
            id: workflow.id.unwrap_or(0),
            text,
            audio_path,
            created_at: workflow.created_at.unwrap_or_else(|| "Unknown".to_string()),
        }
    }
}

/// Search transcription history with optional filters.
///
/// If no filters are specified, returns recent transcriptions.
pub fn search(params: &SearchParams) -> Result<Vec<HistoryEntry>> {
    let conn = db::init_db()?;

    let workflows = if params.has_filters() {
        db::search_workflows(
            &conn,
            params.query.as_deref(),
            params.from.as_deref(),
            params.to.as_deref(),
            params.limit,
        )?
    } else {
        db::get_recent_workflows(&conn, params.limit)?
    };

    Ok(workflows.into_iter().map(HistoryEntry::from).collect())
}

/// Get recent transcription history.
pub fn get_recent(limit: usize) -> Result<Vec<HistoryEntry>> {
    let conn = db::init_db()?;
    let workflows = db::get_recent_workflows(&conn, limit)?;
    Ok(workflows.into_iter().map(HistoryEntry::from).collect())
}

/// Get a single transcription by ID.
pub fn get_by_id(id: i64) -> Result<Option<HistoryEntry>> {
    let conn = db::init_db()?;
    // Use search with a high limit to find by ID
    // TODO: Add a proper get_by_id to db module
    let workflows = db::search_workflows(&conn, None, None, None, 10000)?;

    Ok(workflows
        .into_iter()
        .find(|w| w.id == Some(id))
        .map(HistoryEntry::from))
}

/// Get the text content of a transcription by ID.
///
/// Returns the raw text, suitable for copying to clipboard or returning via API.
pub fn get_text_by_id(id: i64) -> Result<String> {
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
