//! One durable transcription pipeline for capture, import, and retry.
//! Enrichment is independent: persisted raw text is delivered before any AI work.

use anyhow::{Context, Result};
use async_trait::async_trait;
use audetic_core::compression::{cleanup_temp_file, prepare_for_upload};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{error, warn};

use super::progress::AudioNoteProgressObserver;
use super::status::AudioNotePhase;
use crate::db::{self, audio_notes::AudioNoteRepository};
use crate::transcription::job_service::TranscriptionJobService;

#[derive(Debug, Clone, Copy, Default)]
pub struct AudioNoteDeliveryOptions {
    pub auto_paste: bool,
    pub copy_to_clipboard: bool,
}

#[async_trait]
pub trait TranscriptDelivery: Send + Sync {
    async fn deliver(&self, text: &str, options: AudioNoteDeliveryOptions) -> Result<()>;
}

struct DesktopDelivery;

#[async_trait]
pub trait AudioNoteEnrichment: Send + Sync {
    async fn enrich(&self, note_id: i64, db_path: PathBuf) -> Result<()>;
}

struct BackgroundEnrichment;
#[async_trait]
impl AudioNoteEnrichment for BackgroundEnrichment {
    async fn enrich(&self, note_id: i64, db_path: PathBuf) -> Result<()> {
        crate::note_intelligence::enrich_audio_note(note_id, db_path).await
    }
}

#[async_trait]
impl TranscriptDelivery for DesktopDelivery {
    async fn deliver(&self, text: &str, options: AudioNoteDeliveryOptions) -> Result<()> {
        if !options.auto_paste && !options.copy_to_clipboard {
            return Ok(());
        }
        let config = crate::config::Config::load()?;
        let text_io = crate::text_io::TextIoService::new(
            Some(&config.wayland.input_method),
            config.behavior.preserve_clipboard,
        )?;
        if options.copy_to_clipboard
            || (options.auto_paste
                && matches!(
                    text_io.injection_method(),
                    crate::text_io::InjectionMethod::Clipboard
                ))
        {
            text_io.copy_to_clipboard(text).await?;
        }
        if options.auto_paste {
            text_io.inject_text(text).await?;
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct ProcessingServices {
    pub transcription: Arc<dyn TranscriptionJobService>,
    pub db_path: PathBuf,
    pub delivery: Arc<dyn TranscriptDelivery>,
    pub enrichment: Arc<dyn AudioNoteEnrichment>,
}

impl ProcessingServices {
    pub fn new(transcription: Arc<dyn TranscriptionJobService>, db_path: PathBuf) -> Self {
        Self {
            transcription,
            db_path,
            delivery: Arc::new(DesktopDelivery),
            enrichment: Arc::new(BackgroundEnrichment),
        }
    }
}

pub struct ProcessingArgs {
    pub note_id: i64,
    pub audio_path: PathBuf,
    pub duration_seconds: u64,
    pub services: ProcessingServices,
    pub observer: Arc<dyn AudioNoteProgressObserver>,
    pub delivery: AudioNoteDeliveryOptions,
}

/// Persist errors instead of announcing false completion. A database failure is
/// still surfaced to the live observer even when the error row cannot be saved.
pub async fn process_audio_note(args: ProcessingArgs) {
    match transcribe_and_persist(&args).await {
        Ok(text) => {
            if let Err(error) = args.services.delivery.deliver(&text, args.delivery).await {
                warn!(
                    note_id = args.note_id,
                    "Transcript saved but delivery failed: {error:#}"
                );
            }
            args.observer.on_complete(&text).await;
            let note_id = args.note_id;
            let db_path = args.services.db_path.clone();
            let enrichment = args.services.enrichment.clone();
            tokio::spawn(async move {
                if let Err(error) = enrichment.enrich(note_id, db_path).await {
                    warn!(note_id, "Audio note enrichment failed: {error:#}");
                }
            });
        }
        Err(error) => {
            let message = format!("{error:#}");
            error!(
                note_id = args.note_id,
                "Audio note processing failed: {message}"
            );
            let recorded = db::init_db_at(&args.services.db_path).and_then(|conn| {
                AudioNoteRepository::fail(
                    &conn,
                    args.note_id,
                    &message,
                    args.duration_seconds as i64,
                )
            });
            if let Err(persistence_error) = recorded {
                error!(
                    note_id = args.note_id,
                    "Could not persist failure: {persistence_error:#}"
                );
            }
            args.observer.on_error(&message).await;
        }
    }
}

async fn transcribe_and_persist(args: &ProcessingArgs) -> Result<String> {
    let db_path = &args.services.db_path;
    {
        let conn = db::init_db_at(db_path)?;
        AudioNoteRepository::update_status(&conn, args.note_id, AudioNotePhase::Compressing)?;
    }
    args.observer.on_phase(AudioNotePhase::Compressing).await;
    let source = args.audio_path.clone();
    let (upload, temporary) =
        tokio::task::spawn_blocking(move || prepare_for_upload(&source, false)).await??;
    // RAII cleanup also covers failures before transcription starts.
    struct TemporaryUpload(Option<PathBuf>);
    impl Drop for TemporaryUpload {
        fn drop(&mut self) {
            if let Some(path) = &self.0 {
                cleanup_temp_file(path);
            }
        }
    }
    let _cleanup = TemporaryUpload(temporary.clone());
    let durable_audio = if temporary.is_some() {
        let durable = args.audio_path.with_extension("mp3");
        std::fs::copy(&upload, &durable).context("Failed to retain compressed audio")?;
        durable
    } else {
        upload.clone()
    };
    {
        let conn = db::init_db_at(db_path)?;
        AudioNoteRepository::update_audio_path(
            &conn,
            args.note_id,
            &durable_audio.to_string_lossy(),
        )?;
        AudioNoteRepository::update_status(&conn, args.note_id, AudioNotePhase::Transcribing)?;
    }
    // Keep the original capture too: durable audio is never deleted by this pipeline.
    args.observer.on_phase(AudioNotePhase::Transcribing).await;
    let result = args
        .services
        .transcription
        .submit_and_poll(&durable_audio, None)
        .await?;
    persist_transcript(
        db_path,
        args.note_id,
        &durable_audio,
        &result,
        args.duration_seconds,
    )?;
    Ok(result.text)
}

fn persist_transcript(
    db_path: &Path,
    note_id: i64,
    audio: &Path,
    result: &crate::transcription::job_service::TranscriptionJobResult,
    duration: u64,
) -> Result<()> {
    let transcript_path = audio.with_extension("txt");
    std::fs::write(&transcript_path, &result.text).context("Failed to retain transcript file")?;
    let conn = db::init_db_at(db_path)?;
    AudioNoteRepository::complete(
        &conn,
        note_id,
        &transcript_path.to_string_lossy(),
        &result.text,
        result.segments.as_deref(),
        duration as i64,
    )
    .context("Failed to persist completed audio note")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcription::job_service::TranscriptionJobResult;
    use std::sync::Mutex;

    struct StubTranscription {
        delete_before_completion: Option<(PathBuf, i64)>,
    }
    #[async_trait]
    impl TranscriptionJobService for StubTranscription {
        async fn submit_and_poll(
            &self,
            _: &Path,
            _: Option<&str>,
        ) -> Result<TranscriptionJobResult> {
            if let Some((path, id)) = &self.delete_before_completion {
                let conn = db::init_db_at(path)?;
                conn.execute("DELETE FROM audio_notes WHERE id=?1", [id])?;
            }
            Ok(TranscriptionJobResult {
                text: "Raw transcript before AI".into(),
                segments: None,
            })
        }
    }

    struct Probe {
        db_path: PathBuf,
        note_id: i64,
        events: Mutex<Vec<String>>,
    }
    impl Probe {
        fn persisted(&self) {
            let conn = db::init_db_at(&self.db_path).unwrap();
            let note = AudioNoteRepository::get(&conn, self.note_id)
                .unwrap()
                .unwrap();
            assert_eq!(note.status, "completed");
            assert_eq!(
                note.transcript_text.as_deref(),
                Some("Raw transcript before AI")
            );
        }
    }
    #[async_trait]
    impl TranscriptDelivery for Probe {
        async fn deliver(&self, text: &str, options: AudioNoteDeliveryOptions) -> Result<()> {
            self.persisted();
            assert_eq!(text, "Raw transcript before AI");
            if options.auto_paste || options.copy_to_clipboard {
                self.events.lock().unwrap().push("delivery".into());
            }
            Ok(())
        }
    }
    #[async_trait]
    impl AudioNoteProgressObserver for Probe {
        async fn on_phase(&self, _: AudioNotePhase) {}
        async fn on_error(&self, _: &str) {
            self.events.lock().unwrap().push("error".into());
        }
        async fn on_complete(&self, _: &str) {
            self.persisted();
            self.events.lock().unwrap().push("completed".into());
        }
    }
    #[async_trait]
    impl AudioNoteEnrichment for Probe {
        async fn enrich(&self, _: i64, _: PathBuf) -> Result<()> {
            self.persisted();
            self.events.lock().unwrap().push("enrichment".into());
            Ok(())
        }
    }

    async fn run_fixture(delete: bool, delivery: AudioNoteDeliveryOptions) -> Vec<String> {
        let directory = tempfile::tempdir().unwrap();
        let db_path = directory.path().join("isolated.db");
        let audio = directory.path().join("recording.mp3");
        std::fs::write(&audio, b"test audio").unwrap();
        let conn = db::init_db_at(&db_path).unwrap();
        let id = AudioNoteRepository::insert(&conn, None, &audio.to_string_lossy()).unwrap();
        drop(conn);
        let probe = Arc::new(Probe {
            db_path: db_path.clone(),
            note_id: id,
            events: Mutex::new(Vec::new()),
        });
        let mut services = ProcessingServices::new(
            Arc::new(StubTranscription {
                delete_before_completion: delete.then_some((db_path, id)),
            }),
            probe.db_path.clone(),
        );
        services.delivery = probe.clone();
        services.enrichment = probe.clone();
        process_audio_note(ProcessingArgs {
            note_id: id,
            audio_path: audio,
            duration_seconds: 4,
            services,
            observer: probe.clone(),
            delivery,
        })
        .await;
        tokio::task::yield_now().await;
        let events = probe.events.lock().unwrap().clone();
        events
    }

    #[tokio::test]
    async fn persistence_precedes_requested_raw_delivery_completion_and_enrichment() {
        assert_eq!(
            run_fixture(
                false,
                AudioNoteDeliveryOptions {
                    auto_paste: true,
                    copy_to_clipboard: false
                }
            )
            .await,
            ["delivery", "completed", "enrichment"]
        );
    }
    #[tokio::test]
    async fn default_capture_does_not_deliver_text() {
        assert_eq!(
            run_fixture(false, Default::default()).await,
            ["completed", "enrichment"]
        );
    }
    #[tokio::test]
    async fn missing_row_cannot_announce_success_deliver_or_enrich() {
        assert_eq!(
            run_fixture(
                true,
                AudioNoteDeliveryOptions {
                    auto_paste: true,
                    copy_to_clipboard: true
                }
            )
            .await,
            ["error"]
        );
    }
}
