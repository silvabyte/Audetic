//! Logs module for application and transcription log retrieval.
//!
//! This module provides the core business logic for fetching logs.
//! It is used by both the CLI and REST API.

use crate::history::{self, HistoryEntry};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
#[cfg(target_os = "linux")]
use std::process::Command;
use utoipa::ToSchema;

/// Combined logs result containing both app logs and transcription history.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
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

/// Get application logs from the platform's log store.
///
/// Linux: systemd journal via `journalctl --user -u audetic.service`.
/// macOS: tail `~/Library/Logs/Audetic/audetic.log` (written by launchd).
/// Other: empty (no log integration yet).
///
/// Returns a vector of log lines. Returns empty vec if the source is
/// unavailable rather than erroring — log retrieval is best-effort and
/// shouldn't break the `audetic logs` command on a clean install.
pub fn get_app_logs(lines: usize) -> Result<Vec<String>> {
    #[cfg(target_os = "linux")]
    return get_app_logs_journalctl(lines);

    #[cfg(target_os = "macos")]
    return get_app_logs_file(lines);

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = lines;
        Ok(Vec::new())
    }
}

#[cfg(target_os = "linux")]
fn get_app_logs_journalctl(lines: usize) -> Result<Vec<String>> {
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
        // Journal might not exist (no systemd, unit never installed). Empty
        // vec keeps `audetic logs` usable instead of erroring out.
        Ok(Vec::new())
    }
}

#[cfg(target_os = "macos")]
fn get_app_logs_file(lines: usize) -> Result<Vec<String>> {
    let Some(home) = dirs::home_dir() else {
        return Ok(Vec::new());
    };
    let path = home.join("Library/Logs/Audetic/audetic.log");
    if !path.exists() {
        return Ok(Vec::new());
    }

    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;

    // Tail the last `lines` non-empty lines.
    let mut all: Vec<String> = contents
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(String::from)
        .collect();
    let start = all.len().saturating_sub(lines);
    Ok(all.split_off(start))
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
