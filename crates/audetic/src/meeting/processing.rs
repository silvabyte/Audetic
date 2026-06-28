//! Post-recording meeting pipeline.
//!
//! Drives a meeting from a freshly-staged audio file to a completed,
//! transcribed record: compress → transcribe → write transcript →
//! dispatch the `meeting.completed` event → mark completed. Updates the DB
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
}

/// One pipeline invocation. The audio file at `audio_path` must already be
/// staged in its durable location — the pipeline may replace it with a
/// compressed sibling, but it won't move it across directories.
pub struct ProcessingArgs {
    pub meeting_id: i64,
    pub audio_path: PathBuf,
    pub title: Option<String>,
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
        title,
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
            if let Ok(conn) = db::init_db() {
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
    if let Ok(conn) = db::init_db() {
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

            // Serialize per-segment timestamps for clickable transcript lines.
            // Best-effort: a serialization hiccup shouldn't fail the meeting, so
            // we just store NULL and the UI falls back to plain text.
            let segments_json = result
                .segments
                .as_ref()
                .filter(|s| !s.is_empty())
                .and_then(|s| serde_json::to_string(s).ok());

            if let Ok(conn) = db::init_db() {
                let _ = MeetingRepository::complete(
                    &conn,
                    meeting_id,
                    &transcript_path.to_string_lossy(),
                    &result.text,
                    segments_json.as_deref(),
                    duration_seconds as i64,
                );
            }

            info!(
                "Meeting {} transcription complete: {} chars",
                meeting_id,
                result.text.len()
            );

            // Fire any post-processing jobs subscribed to `meeting.completed`.
            // Dispatch is fire-and-forget: each matching job runs in its own
            // spawned task, and failures are logged but never flip the meeting
            // to `error` (the transcription itself succeeded).
            services
                .post_processing
                .dispatch(PostProcessingEvent::MeetingCompleted(
                    MeetingCompletedPayload {
                        meeting_id,
                        title,
                        audio_path: durable_audio,
                        transcript_path,
                        transcript_text: result.text.clone(),
                        duration_seconds,
                    },
                ));

            observer.on_complete(&result.text).await;
        }
        Err(e) => {
            error!("Meeting {} transcription failed: {}", meeting_id, e);
            let error_msg = e.to_string();

            if let Ok(conn) = db::init_db() {
                let _ =
                    MeetingRepository::fail(&conn, meeting_id, &error_msg, duration_seconds as i64);
            }

            observer.on_error(&error_msg).await;
        }
    }
}
