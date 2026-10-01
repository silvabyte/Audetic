//! Import an existing media file as a new meeting.
//!
//! Takes a media file (typically just-uploaded from the HTTP layer or
//! specified by the CLI) and turns it into a meeting record that runs
//! through the same post-recording pipeline a live recording would.
//!
//! Imports never touch the singleton `AudioNoteStatusHandle` or the
//! `Indicator` — the meeting row in SQLite is the source of truth for
//! their state, exactly like `retry_audio_note_transcription`. This means
//! imports run concurrently with a live recording without conflict.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::info;

use crate::db::{self, audio_notes::AudioNoteRepository};
use crate::transcription::jobs_client::mime_type_for_extension;

use super::media_inspector::MediaInspector;
use super::processing::{process_audio_note, ProcessingArgs, ProcessingServices};
use super::progress::NoopProgressObserver;
use super::status::AudioNotePhase;

/// One file-import request.
pub struct ImportArgs {
    /// Where the file is right now — typically a temp file the HTTP handler
    /// just streamed to disk, or a path passed by the CLI. The file is
    /// **moved** into the audio_notes directory; on success, `source_path` no
    /// longer exists.
    pub source_path: PathBuf,
    /// Original filename, used to derive an extension and retained as a
    /// presentation fallback until a canonical AudioNote Title exists.
    /// Take this from the multipart filename or `path.file_name()`.
    pub original_filename: Option<String>,
    /// Optional user-supplied Manual Title.
    pub title: Option<String>,
    /// Acquisition provider shown in the Audio Note provenance metadata.
    pub source_provider: Option<String>,
    /// Stable provider-owned recording identifier used for traceability.
    pub source_external_id: Option<String>,
    /// Original UTC recording time, normalized to SQLite timestamp text.
    pub source_recorded_at: Option<String>,
    /// Claimed external-import row committed atomically with the Audio Note.
    pub external_import_id: Option<i64>,
    /// Shared pipeline dependencies (transcription + post-processing dispatch).
    pub services: ProcessingServices,
    /// How to read media duration. Production wires up `FfprobeMediaInspector`.
    pub inspector: Arc<dyn MediaInspector>,
    /// Where durable meeting audio lives (`~/.local/share/audetic/audio-notes`).
    pub audio_notes_dir: PathBuf,
}

/// Result of staging an imported file: the new meeting id and the final
/// path the audio was moved to.
pub struct ImportResult {
    pub note_id: i64,
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
pub async fn import_audio_note_file(args: ImportArgs) -> Result<ImportResult> {
    let ImportArgs {
        source_path,
        original_filename,
        title,
        source_provider,
        source_external_id,
        source_recorded_at,
        external_import_id,
        services,
        inspector,
        audio_notes_dir,
    } = args;

    let extension = extension_for_import(&source_path, original_filename.as_deref())
        .ok_or_else(|| anyhow::anyhow!("Imported file is missing an extension"))?;

    if mime_type_for_extension(&extension).is_none() {
        bail!(
            "Unsupported media extension '.{}'. Supported: wav, mp3, m4a, flac, ogg, opus, mp4, mkv, webm, avi, mov",
            extension
        );
    }

    std::fs::create_dir_all(&audio_notes_dir)
        .with_context(|| format!("Failed to create audio_notes dir at {:?}", audio_notes_dir))?;

    let destination = imported_destination(&audio_notes_dir, &extension);
    move_file(&source_path, &destination)
        .with_context(|| format!("Failed to move imported file into {:?}", destination))?;
    let mut destination_cleanup = ImportedFileCleanup::new(destination.clone());

    let resolved_title = title
        .map(|title| title.trim().to_string())
        .filter(|title| !title.is_empty());
    let source_filename = original_filename
        .as_deref()
        .or_else(|| source_path.file_name().and_then(|name| name.to_str()));

    let duration_seconds = inspector
        .probe_duration_seconds(&destination)
        .await
        .unwrap_or(0);

    let note_id = match insert_audio_note_row(
        &services.db_path,
        ImportedNoteRow {
            audio_path: &destination,
            title: resolved_title.as_deref(),
            source_filename,
            source_provider: source_provider.as_deref(),
            source_external_id: source_external_id.as_deref(),
            source_recorded_at: source_recorded_at.as_deref(),
            external_import_id,
        },
    ) {
        Ok(id) => id,
        Err(e) => return Err(e),
    };
    destination_cleanup.persist();

    info!(
        "Imported Audio Note {} from file: {:?} ({}s)",
        note_id, destination, duration_seconds
    );

    let pipeline_args = ProcessingArgs {
        note_id,
        audio_path: destination.clone(),
        duration_seconds,
        services,
        observer: Arc::new(NoopProgressObserver),
        delivery: Default::default(),
    };
    tokio::spawn(async move { process_audio_note(pipeline_args).await });

    Ok(ImportResult {
        note_id,
        audio_path: destination,
    })
}

/// Compute the durable destination filename for an imported file.
/// Mirrors the `meeting-{timestamp}-{uuid}.{ext}` layout used by live
/// recordings (see `audio_note_machine::generate_audio_path`).
fn imported_destination(audio_notes_dir: &Path, extension: &str) -> PathBuf {
    let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let unique = uuid::Uuid::new_v4().simple();
    audio_notes_dir.join(format!("imported-{timestamp}-{unique}.{extension}"))
}

struct ImportedFileCleanup {
    path: PathBuf,
    persist: bool,
}

impl ImportedFileCleanup {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            persist: false,
        }
    }

    fn persist(&mut self) {
        self.persist = true;
    }
}

impl Drop for ImportedFileCleanup {
    fn drop(&mut self) {
        if !self.persist {
            if let Err(error) = std::fs::remove_file(&self.path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(path = ?self.path, %error, "Failed to clean up orphaned import");
                }
            }
        }
    }
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
struct ImportedNoteRow<'a> {
    audio_path: &'a Path,
    title: Option<&'a str>,
    source_filename: Option<&'a str>,
    source_provider: Option<&'a str>,
    source_external_id: Option<&'a str>,
    source_recorded_at: Option<&'a str>,
    external_import_id: Option<i64>,
}

fn insert_audio_note_row(db_path: &Path, row: ImportedNoteRow<'_>) -> Result<i64> {
    let mut conn = db::init_db_at(db_path).context("Failed to open audetic database")?;
    let conn = conn.transaction()?;
    let id = AudioNoteRepository::insert_import(
        &conn,
        row.title,
        &row.audio_path.to_string_lossy(),
        row.source_filename,
    )?;
    AudioNoteRepository::update_status(&conn, id, AudioNotePhase::Compressing)?;
    AudioNoteRepository::set_capture_source(&conn, id, "import")?;
    AudioNoteRepository::set_source_metadata(
        &conn,
        id,
        row.source_provider,
        row.source_external_id,
        row.source_recorded_at,
    )?;
    if let Some(import_id) = row.external_import_id {
        crate::db::integrations::IntegrationRepository::accept_external_import(
            &conn, import_id, id,
        )?;
    }
    conn.commit()?;
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
        let dest = imported_destination(Path::new("/var/audetic/audio-notes"), "mp3");
        let name = dest.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("imported-"), "got {name}");
        assert!(name.ends_with(".mp3"), "got {name}");
        assert_eq!(dest.parent(), Some(Path::new("/var/audetic/audio-notes")));
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

    #[test]
    fn external_import_acceptance_commits_with_the_audio_note() {
        let directory = tempfile::tempdir().unwrap();
        let db_path = directory.path().join("audetic.db");
        let conn = crate::db::init_db_at(&db_path).unwrap();
        let claim = crate::db::integrations::IntegrationRepository::claim_external_import(
            &conn,
            "generic_webhook",
            "key-id",
            "recording-id",
            None,
            Some("recording.m4a"),
        )
        .unwrap();
        let crate::db::integrations::ImportClaim::Claimed(import_id) = claim else {
            panic!("first import should be claimed")
        };

        let note_id = insert_audio_note_row(
            &db_path,
            ImportedNoteRow {
                audio_path: Path::new("/tmp/recording.m4a"),
                title: None,
                source_filename: Some("recording.m4a"),
                source_provider: Some("generic_webhook"),
                source_external_id: Some("recording-id"),
                source_recorded_at: None,
                external_import_id: Some(import_id),
            },
        )
        .unwrap();
        let accepted_note =
            crate::db::integrations::IntegrationRepository::accepted_external_import_note_id(
                &conn,
                "generic_webhook",
                "key-id",
                "recording-id",
            )
            .unwrap();
        assert_eq!(accepted_note, Some(note_id));

        assert!(insert_audio_note_row(
            &db_path,
            ImportedNoteRow {
                audio_path: Path::new("/tmp/orphan.m4a"),
                title: None,
                source_filename: Some("orphan.m4a"),
                source_provider: Some("generic_webhook"),
                source_external_id: Some("orphan-id"),
                source_recorded_at: None,
                external_import_id: Some(i64::MAX),
            },
        )
        .is_err());
        let orphan_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM audio_notes WHERE source_external_id = 'orphan-id'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(orphan_count, 0);
    }
}
