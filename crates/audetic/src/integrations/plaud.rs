//! Plaud CLI adapter. This is one poll implementation of [`RecordingSource`].

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use uuid::Uuid;

use super::recording_source::{parse_db_timestamp, RecordingSource};
use super::{ExternalImportRequest, IntegrationService};

const MAX_AUDIO_BYTES: u64 = 1024 * 1024 * 1024;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// The single Plaud CLI installation on this machine. `SOURCE_INSTANCE` is the
/// same identity migration 0002 writes and `external_imports` already uses.
pub struct PlaudSource;

impl PlaudSource {
    pub const PROVIDER: &'static str = "plaud";
    pub const SOURCE_INSTANCE: &'static str = "default";
    /// Shown when a poll row has no interval. The migrated Plaud row always has one.
    pub const DEFAULT_INTERVAL_MINUTES: i64 = 15;
}

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
        if !plaud_output_succeeded(&output.status, &output.stdout, &output.stderr) {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("Plaud CLI failed: {}", stderr.trim());
        }
        String::from_utf8(output.stdout).context("Plaud CLI returned non-UTF-8 output")
    }
}

fn plaud_output_succeeded(status: &std::process::ExitStatus, stdout: &[u8], stderr: &[u8]) -> bool {
    if status.success() {
        return true;
    }

    let stderr = String::from_utf8_lossy(stderr);
    status.code() == Some(13)
        && !stdout.is_empty()
        && stderr.contains("Warning: Detected unsettled top-level await at ")
        && stderr.contains("@plaud-ai/cli/dist/index.js:")
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

#[async_trait]
impl RecordingSource for PlaudSource {
    fn provider(&self) -> &str {
        Self::PROVIDER
    }

    fn source_instance(&self) -> &str {
        Self::SOURCE_INSTANCE
    }

    async fn poll(&self, service: &IntegrationService, backfill: bool) -> Result<()> {
        let cli = PlaudCli::discover()
            .context("Plaud CLI is not installed; install @plaud-ai/cli and run `plaud login`")?;
        if !cli.authenticated().await {
            anyhow::bail!("Plaud CLI is not authenticated; run `plaud login`");
        }
        let state = service
            .recording_source(Self::PROVIDER, Self::SOURCE_INSTANCE)
            .await?;
        let import_after = if backfill {
            None
        } else {
            state
                .import_after
                .as_deref()
                .map(parse_db_timestamp)
                .transpose()?
        };
        let import_boundary = import_boundary(backfill, import_after, chrono::Utc::now());

        for page in 1..=1000 {
            let ids = cli.list_ids(page).await?;
            if ids.is_empty() {
                break;
            }
            let page_len = ids.len();
            let mut reached_boundary = false;
            for id in ids {
                let recording = cli.file(&id).await?;
                let recorded_at = recording.start_at.as_ref().unwrap_or(&recording.created_at);
                let recorded_at = parse_timestamp(recorded_at)
                    .with_context(|| format!("Plaud recording {id} has an invalid timestamp"))?;
                if import_boundary.is_some_and(|boundary| recorded_at < boundary) {
                    reached_boundary = true;
                    continue;
                }
                if service
                    .external_imported(Self::PROVIDER, Self::SOURCE_INSTANCE, &id)
                    .await?
                {
                    continue;
                }
                let url = cli.audio_url(&id).await?;
                let (staged_path, filename) =
                    download_audio(service.uploads_dir(), &id, url).await?;
                service
                    .import_external(ExternalImportRequest {
                        provider: Self::PROVIDER.to_string(),
                        source_instance: Self::SOURCE_INSTANCE.to_string(),
                        external_id: id,
                        recorded_at: Some(recorded_at.format("%Y-%m-%d %H:%M:%S").to_string()),
                        source_filename: Some(filename),
                        title: Some(recording.name),
                        staged_path,
                    })
                    .await?;
            }
            if reached_boundary || page_len < 100 {
                break;
            }
            if page == 1000 {
                anyhow::bail!("Plaud synchronization exceeded the 100,000-recording safety limit");
            }
        }
        if let Some(next_cursor) = import_boundary {
            service
                .update_import_cursor(
                    Self::PROVIDER,
                    Self::SOURCE_INSTANCE,
                    &next_cursor.format("%Y-%m-%d %H:%M:%S").to_string(),
                )
                .await?;
        }
        Ok(())
    }
}

async fn download_audio(
    uploads_dir: PathBuf,
    id: &str,
    url: reqwest::Url,
) -> Result<(PathBuf, String)> {
    anyhow::ensure!(url.scheme() == "https", "Plaud audio URL must use HTTPS");
    anyhow::ensure!(
        url.username().is_empty(),
        "Plaud audio URL must not contain user information"
    );
    let extension = Path::new(url.path())
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| audetic_core::jobs_client::mime_type_for_extension(extension).is_some())
        .unwrap_or("m4a")
        .to_ascii_lowercase();
    let filename = format!("plaud-{id}.{extension}");
    tokio::fs::create_dir_all(&uploads_dir).await?;
    let path = uploads_dir.join(format!("plaud-{}", Uuid::new_v4().simple()));
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(DOWNLOAD_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut response = client.get(url).send().await?.error_for_status()?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_AUDIO_BYTES)
    {
        anyhow::bail!("Plaud audio exceeds the 1 GiB limit");
    }
    let download_result = async {
        let mut file = tokio::fs::File::create(&path).await?;
        let mut written = 0_u64;
        while let Some(chunk) = response.chunk().await? {
            written = written.saturating_add(chunk.len() as u64);
            anyhow::ensure!(
                written <= MAX_AUDIO_BYTES,
                "Plaud audio exceeds the 1 GiB limit"
            );
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        anyhow::ensure!(written > 0, "Plaud audio download was empty");
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if let Err(error) = download_result {
        super::remove_staged(&path).await;
        return Err(error);
    }
    Ok((path, filename))
}

fn parse_timestamp(value: &str) -> Result<chrono::DateTime<chrono::Utc>> {
    if let Ok(timestamp) = chrono::DateTime::parse_from_rfc3339(value) {
        return Ok(timestamp.with_timezone(&chrono::Utc));
    }
    let naive = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f")?;
    Ok(naive.and_utc())
}

fn import_boundary(
    backfill: bool,
    import_after: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    if backfill {
        return None;
    }
    let overlap_boundary = now - chrono::Duration::hours(24);
    Some(
        import_after
            .map(|boundary| boundary.max(overlap_boundary))
            .unwrap_or(overlap_boundary),
    )
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

    #[cfg(unix)]
    #[test]
    fn accepts_official_cli_unsettled_await_exit_after_valid_output() {
        use std::os::unix::process::ExitStatusExt;

        let status = std::process::ExitStatus::from_raw(13 << 8);
        assert!(plaud_output_succeeded(
            &status,
            b"plaud 0.3.14\n",
            b"Warning: Detected unsettled top-level await at file:///opt/homebrew/lib/node_modules/@plaud-ai/cli/dist/index.js:22271\n",
        ));
        assert!(!plaud_output_succeeded(
            &status,
            b"",
            b"Warning: Detected unsettled top-level await at file:///opt/homebrew/lib/node_modules/@plaud-ai/cli/dist/index.js:22271\n",
        ));
    }

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

    #[test]
    fn timestamp_parser_accepts_official_naive_utc_format() {
        assert_eq!(
            parse_timestamp("2026-09-30T13:01:11").unwrap(),
            chrono::DateTime::parse_from_rfc3339("2026-09-30T13:01:11Z")
                .unwrap()
                .with_timezone(&chrono::Utc)
        );
        assert_eq!(
            parse_timestamp("2026-09-12T13:35:45.446000").unwrap(),
            chrono::DateTime::parse_from_rfc3339("2026-09-12T13:35:45.446000Z")
                .unwrap()
                .with_timezone(&chrono::Utc)
        );
    }

    #[test]
    fn first_incremental_sync_only_scans_the_last_day() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-01T16:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(
            import_boundary(false, None, now),
            Some(now - chrono::Duration::hours(24))
        );
        assert_eq!(
            import_boundary(false, Some(now - chrono::Duration::hours(48)), now),
            Some(now - chrono::Duration::hours(24))
        );
        assert_eq!(import_boundary(true, None, now), None);
    }
}
