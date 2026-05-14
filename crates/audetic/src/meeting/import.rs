//! Import an existing media file as a new meeting.
//!
//! Takes a media file (typically just-uploaded from the HTTP layer or
//! specified by the CLI) and turns it into a meeting record that runs
//! through the same post-recording pipeline a live recording would.
//!
//! Imports never touch the singleton `MeetingStatusHandle` or the
//! `Indicator` — the meeting row in SQLite is the source of truth for
//! their state, exactly like `retry_meeting_transcription`. This means
//! imports run concurrently with a live recording without conflict.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::info;

use crate::db::{self, meetings::MeetingRepository};
use crate::transcription::jobs_client::mime_type_for_extension;

use super::media_inspector::MediaInspector;
use super::processing::{process_meeting, ProcessingArgs, ProcessingServices};
use super::progress::NoopProgressObserver;
use super::status::MeetingPhase;

/// One file-import request.
pub struct ImportArgs {
    /// Where the file is right now — typically a temp file the HTTP handler
    /// just streamed to disk, or a path passed by the CLI. The file is
    /// **moved** into the meetings directory; on success, `source_path` no
    /// longer exists.
    pub source_path: PathBuf,
    /// Original filename, used to derive an extension and a title fallback.
    /// Take this from the multipart filename or `path.file_name()`.
    pub original_filename: Option<String>,
    /// Optional user-supplied title. Falls back to the filename stem.
    pub title: Option<String>,
    /// Shared pipeline dependencies (transcription + post-processing dispatch).
    pub services: ProcessingServices,
    /// How to read media duration. Production wires up `FfprobeMediaInspector`.
    pub inspector: Arc<dyn MediaInspector>,
    /// Where durable meeting audio lives (`~/.local/share/audetic/meetings`).
    pub meetings_dir: PathBuf,
}

/// Result of staging an imported file: the new meeting id and the final
/// path the audio was moved to.
pub struct ImportResult {
    pub meeting_id: i64,
    pub audio_path: PathBuf,
}

/// Stage an imported media file and kick off the processing pipeline.
///
/// Synchronous up through "the row exists and the pipeline is spawned";
/// returns the new meeting id immediately. The pipeline runs in the
/// background, advancing the row through `compressing` → `transcribing` →
/// `completed` (or `error`).
///
/// Rejects unsupported extensions before doing any work. Cleans up the
/// staged file if DB insertion fails.
pub async fn import_meeting_file(args: ImportArgs) -> Result<ImportResult> {
    let ImportArgs {
        source_path,
        original_filename,
        title,
        services,
        inspector,
        meetings_dir,
    } = args;

    let extension = extension_for_import(&source_path, original_filename.as_deref())
        .ok_or_else(|| anyhow::anyhow!("Imported file is missing an extension"))?;

    if mime_type_for_extension(&extension).is_none() {
        bail!(
            "Unsupported media extension '.{}'. Supported: wav, mp3, m4a, flac, ogg, opus, mp4, mkv, webm, avi, mov",
            extension
        );
    }

    std::fs::create_dir_all(&meetings_dir)
        .with_context(|| format!("Failed to create meetings dir at {:?}", meetings_dir))?;

    let destination = imported_destination(&meetings_dir, &extension);
    move_file(&source_path, &destination)
        .with_context(|| format!("Failed to move imported file into {:?}", destination))?;

    let resolved_title = title.or_else(|| {
        original_filename
            .as_deref()
            .or_else(|| source_path.file_name().and_then(|n| n.to_str()))
            .and_then(|name| Path::new(name).file_stem().and_then(|s| s.to_str()))
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
    });

    let duration_seconds = inspector
        .probe_duration_seconds(&destination)
        .await
        .unwrap_or(0);

    let meeting_id = match insert_meeting_row(&destination, resolved_title.as_deref()) {
        Ok(id) => id,
        Err(e) => {
            // The DB row is the only thing tying this file to the meeting
            // surface — if insert failed, the file is orphaned. Remove it
            // so we don't leak storage on every failed import.
            if let Err(cleanup_err) = std::fs::remove_file(&destination) {
                tracing::warn!(
                    "Failed to clean up orphaned import at {:?}: {}",
                    destination,
                    cleanup_err
                );
            }
            return Err(e);
        }
    };

    info!(
        "Imported meeting {} from file: {:?} ({}s)",
        meeting_id, destination, duration_seconds
    );

    let pipeline_args = ProcessingArgs {
        meeting_id,
        audio_path: destination.clone(),
        title: resolved_title,
        duration_seconds,
        services,
        observer: Arc::new(NoopProgressObserver),
    };
    tokio::spawn(async move { process_meeting(pipeline_args).await });

    Ok(ImportResult {
        meeting_id,
        audio_path: destination,
    })
}

/// Compute the durable destination filename for an imported file.
/// Mirrors the `meeting-{timestamp}-{uuid}.{ext}` layout used by live
/// recordings (see `meeting_machine::generate_audio_path`).
fn imported_destination(meetings_dir: &Path, extension: &str) -> PathBuf {
    let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let unique = uuid::Uuid::new_v4().simple();
    meetings_dir.join(format!("imported-{timestamp}-{unique}.{extension}"))
}

/// Pick the extension to use. Prefers the original filename (which is what
/// the user actually uploaded) over the temp path, since multipart staging
/// rewrites filenames.
fn extension_for_import(source_path: &Path, original_filename: Option<&str>) -> Option<String> {
    let ext_from_original = original_filename
        .and_then(|name| Path::new(name).extension())
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());

    let ext_from_source = source_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());

    ext_from_original.or(ext_from_source)
}

/// Move a file, falling back to copy+delete when rename fails (which it
/// does whenever `source` is on a different filesystem from `destination`
/// — the common case for multipart uploads staged on `/tmp` tmpfs).
fn move_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    match std::fs::rename(source, destination) {
        Ok(()) => Ok(()),
        Err(_) => {
            std::fs::copy(source, destination)?;
            std::fs::remove_file(source)?;
            Ok(())
        }
    }
}

/// Insert the meeting row with `status = compressing` so the list UI shows
/// it as in-flight rather than recording.
fn insert_meeting_row(audio_path: &Path, title: Option<&str>) -> Result<i64> {
    let conn = db::init_db().context("Failed to open audetic database")?;
    let id = MeetingRepository::insert(&conn, title, &audio_path.to_string_lossy())?;
    MeetingRepository::update_status(&conn, id, MeetingPhase::Compressing)?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::path::Path;

    struct LocalStubInspector(Option<u64>);

    #[async_trait]
    impl MediaInspector for LocalStubInspector {
        async fn probe_duration_seconds(&self, _path: &Path) -> Option<u64> {
            self.0
        }
    }

    #[test]
    fn extension_prefers_original_filename() {
        let ext = extension_for_import(Path::new("/tmp/upload-abc"), Some("Team standup.mp4"));
        assert_eq!(ext.as_deref(), Some("mp4"));
    }

    #[test]
    fn extension_falls_back_to_source_path() {
        let ext = extension_for_import(Path::new("/tmp/whatever.flac"), None);
        assert_eq!(ext.as_deref(), Some("flac"));
    }

    #[test]
    fn extension_is_lowercased() {
        let ext = extension_for_import(Path::new("/tmp/x"), Some("foo.MP3"));
        assert_eq!(ext.as_deref(), Some("mp3"));
    }

    #[test]
    fn extension_none_when_missing() {
        let ext = extension_for_import(Path::new("/tmp/no-extension"), None);
        assert_eq!(ext, None);
    }

    #[test]
    fn imported_destination_has_expected_shape() {
        let dest = imported_destination(Path::new("/var/audetic/meetings"), "mp3");
        let name = dest.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("imported-"), "got {name}");
        assert!(name.ends_with(".mp3"), "got {name}");
        assert_eq!(dest.parent(), Some(Path::new("/var/audetic/meetings")));
    }

    /// The stub inspector exists to confirm the trait object can flow
    /// through `Arc<dyn MediaInspector>` — this is the same pattern the
    /// import endpoint uses to inject `FfprobeMediaInspector` in prod.
    #[tokio::test]
    async fn stub_inspector_round_trip() {
        let inspector = Arc::new(LocalStubInspector(Some(42))) as Arc<dyn MediaInspector>;
        let dur = inspector
            .probe_duration_seconds(Path::new("/tmp/x.mp3"))
            .await;
        assert_eq!(dur, Some(42));
    }
}
