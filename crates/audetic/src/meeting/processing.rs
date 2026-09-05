//! Post-recording meeting pipeline.
//!
//! Drives a meeting from a freshly-staged audio file to a completed,
//! transcribed record: compress → transcribe → write transcript → mark
//! completed → dispatch the `meeting.completed` event. Updates the DB
//! row at every transition. Side effects that depend on the *caller*
//! (live indicator, status handle) are delegated to a
//! `MeetingProgressObserver` so this module stays oblivious to whether
//! it's serving a live recording, an import, or a retry.
//!
//! See `meeting_machine::stop()` and `meeting::import_meeting_file` for the
//! two call sites that drive a meeting from creation to completion.

use std::path::PathBuf;
use std::sync::Arc;
use tracing::{error, info, warn};

use crate::db::{self, meetings::MeetingRepository};
use crate::post_processing::{
    Event as PostProcessingEvent, MeetingCompletedPayload, PostProcessingService,
};
use crate::transcription::job_service::TranscriptionJobService;
use audetic_core::compression::{cleanup_temp_file, prepare_for_upload};

use super::progress::MeetingProgressObserver;
use super::status::MeetingPhase;

/// Dependencies the pipeline shares with every meeting-driving flow (live
/// recording, import, retry). Cheap to clone — every field is an `Arc`.
#[derive(Clone)]
pub struct ProcessingServices {
    pub transcription: Arc<dyn TranscriptionJobService>,
    pub post_processing: Arc<PostProcessingService>,
    pub db_path: PathBuf,
}

impl ProcessingServices {
    pub fn new(
        transcription: Arc<dyn TranscriptionJobService>,
        post_processing: Arc<PostProcessingService>,
        db_path: PathBuf,
    ) -> Self {
        Self {
            transcription,
            post_processing,
            db_path,
        }
    }

    fn open_db(&self) -> anyhow::Result<rusqlite::Connection> {
        db::open_db_at(&self.db_path)
    }

    pub fn public_meeting_id(&self, local_id: i64) -> anyhow::Result<audetic_core::sync::RecordId> {
        MeetingRepository::get(&self.open_db()?, local_id)?
            .map(|meeting| meeting.sync_id)
            .ok_or_else(|| anyhow::anyhow!("meeting {local_id} not found"))
    }

    pub(crate) fn local_library(&self) -> crate::sync::shared_library::SharedLibrary {
        crate::sync::SyncService::local_library(self.db_path.clone()).library()
    }
}

/// One pipeline invocation. The audio file at `audio_path` must already be
/// staged in its durable location — the pipeline may replace it with a
/// compressed sibling, but it won't move it across directories.
pub struct ProcessingArgs {
    pub meeting_id: i64,
    pub audio_path: PathBuf,
    pub duration_seconds: u64,
    pub services: ProcessingServices,
    pub observer: Arc<dyn MeetingProgressObserver>,
}

/// Run the post-recording pipeline.
///
/// Always leaves the meeting row in a terminal state (`completed` or
/// `error`). Post-processing dispatch is fire-and-forget — a slow or
/// failing user job never flips the meeting to `error`. Never panics;
/// every infrastructure error is logged and recorded in the row.
pub async fn process_meeting(args: ProcessingArgs) {
    let ProcessingArgs {
        meeting_id,
        audio_path,
        duration_seconds,
        services,
        observer,
    } = args;

    info!("Compressing meeting {} audio: {:?}", meeting_id, audio_path);

    let (temp_upload, temp_to_cleanup) = match prepare_for_upload(&audio_path, false) {
        Ok(v) => v,
        Err(e) => {
            let error_msg = e.to_string();
            error!("Meeting {} compression failed: {}", meeting_id, error_msg);
            if let Ok(conn) = services.open_db() {
                let _ =
                    MeetingRepository::fail(&conn, meeting_id, &error_msg, duration_seconds as i64);
            }
            observer.on_error(&error_msg).await;
            return;
        }
    };

    // Move the compressed mp3 next to the original via copy (cross-fs safe —
    // the temp dir is often tmpfs while the meetings dir is under
    // `~/.local/share`). The durable mp3 is what post-processing jobs and
    // history reference; drop the original once the mp3 is in place.
    let durable_audio = if temp_to_cleanup.is_some() {
        let durable = audio_path.with_extension("mp3");
        match std::fs::copy(&temp_upload, &durable) {
            Ok(_) => {
                if durable != audio_path {
                    if let Err(e) = std::fs::remove_file(&audio_path) {
                        warn!("Failed to delete pre-compression source: {}", e);
                    }
                }
                durable
            }
            Err(e) => {
                warn!("Failed to copy compressed mp3 next to source: {}", e);
                audio_path.clone()
            }
        }
    } else {
        temp_upload.clone()
    };

    info!(
        "Compressed meeting {} audio at: {:?}",
        meeting_id, durable_audio
    );

    observer.on_phase(MeetingPhase::Transcribing).await;
    if let Ok(conn) = services.open_db() {
        let _ = MeetingRepository::update_status(&conn, meeting_id, MeetingPhase::Transcribing);
        // Keep the DB row pointing at the file that actually exists. The
        // source is gone after a successful copy; retries / file UI need
        // the .mp3 path or they'll error out trying to read a deleted file.
        if durable_audio != audio_path {
            let _ = MeetingRepository::update_audio_path(
                &conn,
                meeting_id,
                &durable_audio.to_string_lossy(),
            );
        }
    }

    let transcription_result = services
        .transcription
        .submit_and_poll(&temp_upload, None)
        .await;

    if let Some(temp) = &temp_to_cleanup {
        cleanup_temp_file(temp);
    }

    match transcription_result {
        Ok(result) => {
            let transcript_path = durable_audio.with_extension("txt");
            if let Err(e) = std::fs::write(&transcript_path, &result.text) {
                error!("Failed to write transcript file: {}", e);
            }

            let conn = match services.open_db() {
                Ok(conn) => conn,
                Err(error) => {
                    let message = format!("Failed to persist completed meeting: {error}");
                    error!("Meeting {}: {}", meeting_id, message);
                    observer.on_error(&message).await;
                    return;
                }
            };
            if let Err(error) = MeetingRepository::complete(
                &conn,
                meeting_id,
                &transcript_path.to_string_lossy(),
                &result.text,
                result.segments.as_deref(),
                duration_seconds as i64,
            ) {
                let message = format!("Failed to persist completed meeting: {error}");
                error!("Meeting {}: {}", meeting_id, message);
                let _ =
                    MeetingRepository::fail(&conn, meeting_id, &message, duration_seconds as i64);
                observer.on_error(&message).await;
                return;
            }
            let meeting_identity = MeetingRepository::get(&conn, meeting_id)
                .map_err(|error| error.to_string())
                .ok()
                .flatten()
                .map(|meeting| (meeting.sync_id, meeting.title));

            info!(
                "Meeting {} transcription complete: {} chars",
                meeting_id,
                result.text.len()
            );

            // Fire any post-processing jobs subscribed to `meeting.completed`.
            // Dispatch is fire-and-forget: each matching job runs in its own
            // spawned task, and failures are logged but never flip the meeting
            // to `error` (the transcription itself succeeded).
            if let Some((record_id, canonical_title)) = meeting_identity {
                services
                    .post_processing
                    .dispatch(PostProcessingEvent::MeetingCompleted(
                        MeetingCompletedPayload {
                            meeting_id,
                            record_id,
                            title: canonical_title,
                            audio_path: durable_audio,
                            transcript_path,
                            transcript_text: result.text.clone(),
                            duration_seconds,
                        },
                    ));
            }

            observer.on_complete(&result.text).await;
            super::title::spawn_title_generation_at(meeting_id, services.db_path.clone());
        }
        Err(e) => {
            error!("Meeting {} transcription failed: {}", meeting_id, e);
            let error_msg = e.to_string();

            if let Ok(conn) = services.open_db() {
                let _ =
                    MeetingRepository::fail(&conn, meeting_id, &error_msg, duration_seconds as i64);
            }

            observer.on_error(&error_msg).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use async_trait::async_trait;
    use std::path::Path;
    use tokio::sync::Mutex;

    use crate::post_processing::{Action, EventKind, JobRepository, NewJob};
    use crate::transcription::job_service::TranscriptionJobResult;

    struct SuccessfulTranscription;

    #[async_trait]
    impl TranscriptionJobService for SuccessfulTranscription {
        async fn submit_and_poll(
            &self,
            _file_path: &Path,
            _language: Option<&str>,
        ) -> Result<TranscriptionJobResult> {
            Ok(TranscriptionJobResult {
                text: "persist me".into(),
                segments: None,
            })
        }
    }

    #[derive(Default)]
    struct RecordingObserver {
        errors: Mutex<Vec<String>>,
        completions: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl MeetingProgressObserver for RecordingObserver {
        async fn on_phase(&self, _phase: MeetingPhase) {}

        async fn on_error(&self, message: &str) {
            self.errors.lock().await.push(message.into());
        }

        async fn on_complete(&self, transcript_preview: &str) {
            self.completions
                .lock()
                .await
                .push(transcript_preview.into());
        }
    }

    #[tokio::test]
    async fn completion_persistence_failure_suppresses_success_side_effects() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let db_path = directory.path().join("audetic.db");
        crate::db::migrate_db_at(&db_path)?;
        let audio_path = directory.path().join("meeting.mp3");
        std::fs::write(&audio_path, b"fake mp3")?;
        let marker = directory.path().join("post-processing-ran");
        let conn = crate::db::open_db_at(&db_path)?;
        let meeting_id = MeetingRepository::insert(&conn, None, audio_path.to_str().unwrap())?;
        JobRepository::insert(
            &conn,
            &NewJob {
                name: "completion marker".into(),
                event: EventKind::MeetingCompleted,
                action: Action::Command {
                    command: format!("touch '{}'", marker.display()),
                    timeout_seconds: 5,
                },
                enabled: true,
            },
        )?;
        conn.execute_batch(&format!(
            "CREATE TRIGGER reject_meeting_completion \
             BEFORE UPDATE OF status ON meetings \
             WHEN NEW.id = {meeting_id} AND NEW.status = 'completed' \
             BEGIN SELECT RAISE(ABORT, 'forced completion failure'); END;"
        ))?;
        drop(conn);

        let observer = Arc::new(RecordingObserver::default());
        process_meeting(ProcessingArgs {
            meeting_id,
            audio_path,
            duration_seconds: 12,
            services: ProcessingServices::new(
                Arc::new(SuccessfulTranscription),
                Arc::new(PostProcessingService::new(db_path.clone())),
                db_path.clone(),
            ),
            observer: Arc::clone(&observer) as Arc<dyn MeetingProgressObserver>,
        })
        .await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        assert!(observer.completions.lock().await.is_empty());
        assert_eq!(observer.errors.lock().await.len(), 1);
        assert!(!marker.exists(), "post-processing must not be dispatched");
        let conn = crate::db::open_db_at(&db_path)?;
        let meeting = MeetingRepository::get(&conn, meeting_id)?.unwrap();
        assert_eq!(meeting.status, MeetingPhase::Error.as_str());
        assert!(meeting.title.is_none(), "title generation must not start");
        Ok(())
    }
}
