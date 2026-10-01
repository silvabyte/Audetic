//! Plaud CLI adapter. Synchronization orchestration is added separately.

use anyhow::{bail, Context, Result};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaudRecording {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub start_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PlaudCli {
    executable: std::path::PathBuf,
}

impl PlaudCli {
    pub fn discover() -> Option<Self> {
        which::which("plaud")
            .ok()
            .map(|executable| Self { executable })
    }

    pub async fn version(&self) -> Result<String> {
        let stdout = self.run(["version"]).await?;
        Ok(stdout.trim().to_string())
    }

    pub async fn authenticated(&self) -> bool {
        self.run(["me"]).await.is_ok()
    }

    pub async fn list_ids(&self, page: usize) -> Result<Vec<String>> {
        let page = page.to_string();
        let stdout = self
            .run(["files", "--page", &page, "--page-size", "100"])
            .await?;
        parse_file_ids(&stdout)
    }

    pub async fn file(&self, id: &str) -> Result<PlaudRecording> {
        parse_file_details(&self.run(["file", id]).await?)
    }

    pub async fn audio_url(&self, id: &str) -> Result<reqwest::Url> {
        parse_audio_url(&self.run(["audio", id]).await?)
    }

    async fn run<const N: usize>(&self, arguments: [&str; N]) -> Result<String> {
        let mut command = Command::new(&self.executable);
        command
            .args(arguments)
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .kill_on_drop(true);
        let output = tokio::time::timeout(Duration::from_secs(60), command.output())
            .await
            .context("Plaud CLI timed out after 60 seconds")?
            .context("Failed to run Plaud CLI")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("Plaud CLI failed: {}", stderr.trim());
        }
        String::from_utf8(output.stdout).context("Plaud CLI returned non-UTF-8 output")
    }
}

fn parse_file_ids(output: &str) -> Result<Vec<String>> {
    let mut in_table = false;
    let mut ids = Vec::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("ID") && trimmed.contains("NAME") {
            in_table = true;
            continue;
        }
        if !in_table || trimmed.is_empty() || trimmed.starts_with('─') {
            continue;
        }
        if trimmed.starts_with("Page ") {
            break;
        }
        let Some(id) = trimmed.split_whitespace().next() else {
            continue;
        };
        if id.len() < 8 {
            bail!("Plaud CLI files output changed unexpectedly");
        }
        ids.push(id.to_string());
    }
    if !in_table {
        bail!("Plaud CLI files output did not contain a file table");
    }
    Ok(ids)
}

fn parse_file_details(output: &str) -> Result<PlaudRecording> {
    let value = |label: &str| {
        output.lines().find_map(|line| {
            let trimmed = line.trim();
            trimmed
                .strip_prefix(label)
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
    };
    Ok(PlaudRecording {
        id: value("id:")
            .context("Plaud file output did not contain an ID")?
            .to_string(),
        name: value("name:")
            .context("Plaud file output did not contain a name")?
            .to_string(),
        created_at: value("created_at:")
            .context("Plaud file output did not contain a creation time")?
            .to_string(),
        start_at: value("start_at:")
            .filter(|value| *value != "-")
            .map(str::to_string),
    })
}

fn parse_audio_url(output: &str) -> Result<reqwest::Url> {
    let urls = output
        .split_whitespace()
        .filter_map(|value| reqwest::Url::parse(value).ok())
        .filter(|url| url.scheme() == "https")
        .collect::<Vec<_>>();
    match urls.as_slice() {
        [url] => Ok(url.clone()),
        [] => bail!("Plaud recording has no downloadable audio URL"),
        _ => bail!("Plaud CLI audio output contained multiple URLs"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_documented_human_readable_cli_output() {
        let files = "\nFiles on this page: 2\n\n  ID                                  NAME                                  DATE          DURATION\n  ──────────────────────────────────────────────────────────────────────────────────────────────────\n  recording-123                       Weekly sync                           Sep 30        2m03s\n  recording-456                       Notes                                 Sep 29        42s\n\nPage 1\n";
        assert_eq!(
            parse_file_ids(files).unwrap(),
            vec!["recording-123", "recording-456"]
        );

        let details = "\nFile Details:\n\n  id:           recording-123\n  name:         Weekly sync\n  created_at:   2026-09-30T12:00:00Z\n  start_at:     2026-09-30T11:59:00Z\n  duration:     2m03s\n";
        assert_eq!(
            parse_file_details(details).unwrap(),
            PlaudRecording {
                id: "recording-123".to_string(),
                name: "Weekly sync".to_string(),
                created_at: "2026-09-30T12:00:00Z".to_string(),
                start_at: Some("2026-09-30T11:59:00Z".to_string()),
            }
        );

        let audio = "\nAudio Download URL:\n\nhttps://example.com/audio.m4a?signature=abc\n\nNote: This URL expires in 24 hours.\n";
        assert_eq!(
            parse_audio_url(audio).unwrap().host_str(),
            Some("example.com")
        );
    }
}
