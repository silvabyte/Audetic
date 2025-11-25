//! Logs module for application and transcription log retrieval.
//!
//! This module provides the core business logic for fetching logs.
//! It is used by both the CLI and REST API.

use crate::history::{self, HistoryEntry};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::process::Command;

/// Combined logs result containing both app logs and transcription history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogsResult {
    /// Application logs from systemd journal
    pub app_logs: Vec<String>,
    /// Recent transcription entries
    pub transcriptions: Vec<HistoryEntry>,
}

/// Options for log retrieval.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LogsOptions {
    /// Number of log entries to retrieve
    pub lines: usize,
}

impl LogsOptions {
    pub fn new(lines: usize) -> Self {
        Self { lines }
    }
}

/// Get combined application logs and transcription history.
pub fn get_logs(options: &LogsOptions) -> Result<LogsResult> {
    let app_logs = get_app_logs(options.lines)?;
    let transcriptions = history::get_recent(options.lines)?;

    Ok(LogsResult {
        app_logs,
        transcriptions,
    })
}

/// Get application logs from systemd journal.
///
/// Returns a vector of log lines. Returns empty vec if journal is unavailable.
pub fn get_app_logs(lines: usize) -> Result<Vec<String>> {
    let output = Command::new("journalctl")
        .arg("--user")
        .arg("-u")
        .arg("audetic.service")
        .arg("-n")
        .arg(lines.to_string())
        .arg("--output=short-iso")
        .arg("--no-pager")
        .output()
        .context("Failed to execute journalctl. Is the service running?")?;

    if output.status.success() {
        let logs = String::from_utf8_lossy(&output.stdout);
        Ok(logs
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(String::from)
            .collect())
    } else {
        // Return empty vec instead of error - journal might not be available
        Ok(Vec::new())
    }
}

/// Get transcription history logs.
///
/// This is a convenience wrapper around history::get_recent.
pub fn get_transcription_logs(lines: usize) -> Result<Vec<HistoryEntry>> {
    history::get_recent(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_logs_options_new() {
        let opts = LogsOptions::new(50);
        assert_eq!(opts.lines, 50);
    }
}
