//! CLI handler for transcribing audio/video files.
//!
//! Submits files to the jobs API, polls for progress, and outputs results.

use anyhow::{bail, Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::time::sleep;

use crate::cli::args::{OutputFormat, TranscribeCliArgs};
use crate::cli::compression::{
    cleanup_temp_file, compress_for_transcription, get_file_size, is_already_compressed,
};
use crate::transcription::jobs_client::{mime_type_for_extension, status, Job, JobsClient, TranscriptionResult};
use crate::config::Config;
use crate::text_io::copy_to_clipboard_sync;
const POLL_INTERVAL_MS: u64 = 1000;
const MAX_POLL_ATTEMPTS: u32 = 1800; // 30 minutes at 1s intervals
const DEFAULT_API_URL: &str = "https://audio.audetic.link/api/v1/jobs";

/// Handle the transcribe CLI command.
pub async fn handle_transcribe_command(args: TranscribeCliArgs) -> Result<()> {
    // 1. Validate file exists and is supported format
    validate_file(&args.file)?;

    // 2. Check file size and compress if needed
    let (file_to_upload, temp_file) = prepare_file_for_upload(&args.file, args.no_compress)?;

    // 3. Determine API URL
    let config = Config::load()?;
    let base_url = args
        .api_url
        .or_else(|| {
            config
                .whisper
                .api_endpoint
                .as_ref()
                .map(|e| derive_jobs_url(e))
        })
        .unwrap_or_else(|| DEFAULT_API_URL.to_string());

    let client = JobsClient::new(&base_url);

    // 4. Submit job with progress indicator
    let show_progress = !args.no_progress;
    let pb = if show_progress {
        let pb = create_progress_bar();
        pb.set_message("Uploading...");
        Some(pb)
    } else {
        None
    };

    let language = args
        .language
        .as_deref()
        .or(config.whisper.language.as_deref());

    let job_id = client
        .submit_job(&file_to_upload, language, args.timestamps)
        .await
        .context("Failed to submit transcription job")?;

    // 5. Poll for completion
    let job = poll_until_complete(&client, &job_id, pb.as_ref()).await?;

    // 6. Clean up temp file if one was created
    if let Some(temp) = temp_file {
        cleanup_temp_file(&temp);
    }

    if let Some(pb) = pb {
        pb.finish_with_message("Complete");
    }

    // 7. Handle result
    if job.status == status::FAILED {
        bail!(
            "Transcription failed: {}",
            job.error.unwrap_or_else(|| "Unknown error".to_string())
        );
    }

    let result = job
        .result
        .ok_or_else(|| anyhow::anyhow!("Job completed but no result available"))?;

    // 8. Format and output
    let output_text = format_output(&result, &args.format, args.timestamps);

    if let Some(output_path) = &args.output {
        std::fs::write(output_path, &output_text).context("Failed to write output file")?;
        eprintln!("Transcription saved to: {}", output_path.display());
    } else {
        println!("{}", output_text);
    }

    if args.copy {
        copy_to_clipboard_sync(&output_text)?;
        eprintln!("Copied to clipboard");
    }

    Ok(())
}

/// Validate that the file exists and has a supported format.
fn validate_file(path: &Path) -> Result<()> {
    if !path.exists() {
        bail!("File not found: {}", path.display());
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();

    if mime_type_for_extension(&ext).is_none() {
        bail!(
            "Unsupported format: .{}\nSupported formats: wav, mp3, m4a, flac, ogg, opus, mp4, mkv, webm, avi, mov",
            ext,
        );
    }

    Ok(())
}

/// Prepare file for upload, compressing if needed.
///
/// Returns (file_to_upload, Option<temp_file_path>).
/// If compression was performed, temp_file_path will be Some and should be cleaned up after upload.
fn prepare_file_for_upload(path: &Path, skip_compression: bool) -> Result<(PathBuf, Option<PathBuf>)> {
    // Skip compression if file is already in the target format
    if is_already_compressed(path) {
        return Ok((path.to_path_buf(), None));
    }

    // Skip compression if user passed --no-compress
    if skip_compression {
        return Ok((path.to_path_buf(), None));
    }

    // Compress to mp3
    let size_mb = get_file_size(path)? as f64 / 1_000_000.0;
    eprintln!("Compressing to mp3 for upload ({:.1}MB)...", size_mb);

    let compressed = compress_for_transcription(path)?;
    let compressed_size_mb = get_file_size(&compressed)? as f64 / 1_000_000.0;
    eprintln!("Compressed to {:.1}MB", compressed_size_mb);

    Ok((compressed.clone(), Some(compressed)))
}

/// Derive the jobs URL from a transcriptions endpoint.
fn derive_jobs_url(endpoint: &str) -> String {
    // If endpoint ends with /transcriptions, replace with /jobs
    if endpoint.ends_with("/transcriptions") {
        endpoint.replace("/transcriptions", "/jobs")
    } else if endpoint.ends_with("/transcriptions/") {
        endpoint.replace("/transcriptions/", "/jobs")
    } else {
        // Assume it's a base URL, append /jobs
        format!("{}/jobs", endpoint.trim_end_matches('/'))
    }
}

/// Create a styled progress bar.
fn create_progress_bar() -> ProgressBar {
    let pb = ProgressBar::new(100);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}% {msg}")
            .unwrap()
            .progress_chars("━╸━"),
    );
    pb.enable_steady_tick(Duration::from_millis(100));
    pb
}

/// Poll the job status until completion or failure.
async fn poll_until_complete(
    client: &JobsClient,
    job_id: &str,
    pb: Option<&ProgressBar>,
) -> Result<Job> {
    for _ in 0..MAX_POLL_ATTEMPTS {
        let status = client.get_status(job_id).await?;

        if let Some(pb) = pb {
            pb.set_position(status.progress as u64);
            if let Some(msg) = &status.progress_message {
                pb.set_message(msg.clone());
            } else {
                let msg = match status.status.as_str() {
                    status::PENDING => "Waiting...",
                    status::EXTRACTING_AUDIO => "Extracting audio...",
                    status::TRANSCRIBING => "Transcribing...",
                    _ => "",
                };
                pb.set_message(msg);
            }
        }

        match status.status.as_str() {
            status::COMPLETED | status::FAILED => {
                return client.get_job(job_id).await;
            }
            status::CANCELLED => {
                bail!("Job was cancelled");
            }
            _ => {
                sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
            }
        }
    }

    bail!(
        "Transcription timed out after {} seconds",
        MAX_POLL_ATTEMPTS
    );
}

/// Format the transcription result according to the requested format.
fn format_output(result: &TranscriptionResult, format: &OutputFormat, timestamps: bool) -> String {
    match format {
        OutputFormat::Text => {
            if timestamps {
                format_text_with_timestamps(result)
            } else {
                result.text.clone()
            }
        }
        OutputFormat::Json => {
            serde_json::to_string_pretty(result).unwrap_or_else(|_| result.text.clone())
        }
        OutputFormat::Srt => format_as_srt(result),
    }
}

/// Format result as text with timestamps.
fn format_text_with_timestamps(result: &TranscriptionResult) -> String {
    match &result.segments {
        Some(segments) if !segments.is_empty() => segments
            .iter()
            .map(|s| format!("[{:.2} - {:.2}] {}", s.start, s.end, s.text))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => result.text.clone(),
    }
}

/// Format result as SRT subtitles.
fn format_as_srt(result: &TranscriptionResult) -> String {
    match &result.segments {
        Some(segments) if !segments.is_empty() => segments
            .iter()
            .enumerate()
            .map(|(i, s)| {
                format!(
                    "{}\n{} --> {}\n{}\n",
                    i + 1,
                    format_srt_time(s.start),
                    format_srt_time(s.end),
                    s.text.trim()
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => format!("1\n00:00:00,000 --> 00:00:00,000\n{}\n", result.text),
    }
}

/// Format seconds as SRT timestamp (HH:MM:SS,mmm).
fn format_srt_time(seconds: f64) -> String {
    let hours = (seconds / 3600.0) as u32;
    let minutes = ((seconds % 3600.0) / 60.0) as u32;
    let secs = (seconds % 60.0) as u32;
    let millis = ((seconds % 1.0) * 1000.0) as u32;
    format!("{:02}:{:02}:{:02},{:03}", hours, minutes, secs, millis)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_validate_file_supported_audio() {
        let path = PathBuf::from("/tmp/test_audio.wav");
        std::fs::write(&path, b"test").unwrap();
        assert!(validate_file(&path).is_ok());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn test_validate_file_supported_video() {
        let path = PathBuf::from("/tmp/test_video.mp4");
        std::fs::write(&path, b"test").unwrap();
        assert!(validate_file(&path).is_ok());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn test_validate_file_unsupported() {
        let path = PathBuf::from("/tmp/test_unsupported.xyz");
        std::fs::write(&path, b"test").unwrap();
        assert!(validate_file(&path).is_err());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn test_validate_file_not_found() {
        let path = PathBuf::from("/tmp/nonexistent_file.wav");
        assert!(validate_file(&path).is_err());
    }

    #[test]
    fn test_derive_jobs_url_from_transcriptions() {
        assert_eq!(
            derive_jobs_url("https://audio.audetic.link/api/v1/transcriptions"),
            "https://audio.audetic.link/api/v1/jobs"
        );
    }

    #[test]
    fn test_derive_jobs_url_from_transcriptions_trailing_slash() {
        assert_eq!(
            derive_jobs_url("https://audio.audetic.link/api/v1/transcriptions/"),
            "https://audio.audetic.link/api/v1/jobs"
        );
    }

    #[test]
    fn test_derive_jobs_url_base() {
        assert_eq!(
            derive_jobs_url("https://audio.audetic.link/api/v1"),
            "https://audio.audetic.link/api/v1/jobs"
        );
    }

    #[test]
    fn test_format_srt_time_zero() {
        assert_eq!(format_srt_time(0.0), "00:00:00,000");
    }

    #[test]
    fn test_format_srt_time_minutes() {
        assert_eq!(format_srt_time(61.5), "00:01:01,500");
    }

    #[test]
    fn test_format_srt_time_hours() {
        assert_eq!(format_srt_time(3661.123), "01:01:01,123");
    }

    #[test]
    fn test_format_output_text() {
        let result = TranscriptionResult {
            text: "Hello world".to_string(),
            segments: None,
        };
        assert_eq!(
            format_output(&result, &OutputFormat::Text, false),
            "Hello world"
        );
    }

    #[test]
    fn test_prepare_opus_file_skips_compression() {
        let path = PathBuf::from("/tmp/test_skip_compress.opus");
        std::fs::write(&path, b"fake opus data").unwrap();

        let (upload_path, temp_file) = prepare_file_for_upload(&path, false).unwrap();

        assert_eq!(upload_path, path);
        assert!(temp_file.is_none());

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn test_prepare_no_compress_flag_skips_compression() {
        let path = PathBuf::from("/tmp/test_no_compress_flag.wav");
        std::fs::write(&path, b"fake wav data").unwrap();

        let (upload_path, temp_file) = prepare_file_for_upload(&path, true).unwrap();

        assert_eq!(upload_path, path);
        assert!(temp_file.is_none());

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn test_format_output_json() {
        let result = TranscriptionResult {
            text: "Hello".to_string(),
            segments: None,
        };
        let output = format_output(&result, &OutputFormat::Json, false);
        assert!(output.contains("\"text\""));
        assert!(output.contains("Hello"));
    }
}
