//! Hardware/network-independent lifecycle regressions. Compression exercises
//! real ffmpeg, while all transcription, delivery, and enrichment are injected.
use anyhow::Result;
use async_trait::async_trait;
use audetic::audio::audio_source::{AudioSource, CaptureMicSource, CaptureSystemSource};
use audetic::audio_notes::processing::AudioNoteEnrichment;
use audetic::audio_notes::{
    AudioNoteCaptureSource, AudioNoteMachine, AudioNotePhase, AudioNoteStartOptions,
    AudioNoteStatusHandle, ProcessingServices,
};
use audetic::db::audio_notes::AudioNoteRepository;
use audetic::transcription::job_service::{TranscriptionJobResult, TranscriptionJobService};
use audetic::ui::Indicator;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

struct MockSource {
    samples: Vec<f32>,
    active: bool,
    starts: Arc<AtomicUsize>,
}
impl AudioSource for MockSource {
    fn start(&mut self) -> Result<()> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        self.active = true;
        Ok(())
    }
    fn stop(&mut self) -> Result<Vec<f32>> {
        self.active = false;
        Ok(self.samples.clone())
    }
    fn is_active(&self) -> bool {
        self.active
    }
    fn sample_rate(&self) -> u32 {
        16000
    }
}
#[async_trait(?Send)]
impl CaptureMicSource for MockSource {
    fn has_captured_audio(&self) -> bool {
        !self.samples.is_empty()
    }
}
#[async_trait(?Send)]
impl CaptureSystemSource for MockSource {
    fn has_captured_audio(&self) -> bool {
        !self.samples.is_empty()
    }
}

struct MockTranscription {
    fail: bool,
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl TranscriptionJobService for MockTranscription {
    async fn submit_and_poll(&self, _: &Path, _: Option<&str>) -> Result<TranscriptionJobResult> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            anyhow::bail!("mock transcription failure");
        }
        Ok(TranscriptionJobResult {
            text: "hello world from the mock".into(),
            segments: None,
        })
    }
}
struct NoEnrichment;
#[async_trait]
impl AudioNoteEnrichment for NoEnrichment {
    async fn enrich(&self, _: i64, _: PathBuf) -> Result<()> {
        Ok(())
    }
}

struct Fixture {
    machine: AudioNoteMachine,
    status: AudioNoteStatusHandle,
    calls: Arc<AtomicUsize>,
    system_starts: Arc<AtomicUsize>,
    directory: tempfile::TempDir,
    db_path: PathBuf,
}
impl Fixture {
    fn new(fail: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let db_path = directory.path().join("isolated.db");
        let calls = Arc::new(AtomicUsize::new(0));
        let system_starts = Arc::new(AtomicUsize::new(0));
        let samples: Vec<f32> = (0..32000).map(|i| (i as f32 * 0.1).sin() * 0.2).collect();
        let transcription = Arc::new(MockTranscription {
            fail,
            calls: calls.clone(),
        });
        let mut services = ProcessingServices::new(transcription.clone(), db_path.clone());
        services.enrichment = Arc::new(NoEnrichment);
        let status = AudioNoteStatusHandle::default();
        let machine = AudioNoteMachine::new(
            Box::new(MockSource {
                samples: samples.clone(),
                active: false,
                starts: Arc::new(AtomicUsize::new(0)),
            }),
            Box::new(MockSource {
                samples,
                active: false,
                starts: system_starts.clone(),
            }),
            transcription,
            Indicator::new().with_audio_feedback(false),
            status.clone(),
            directory.path().to_path_buf(),
            db_path.clone(),
        )
        .with_processing_services(services)
        .unwrap();
        Self {
            machine,
            status,
            calls,
            system_starts,
            directory,
            db_path,
        }
    }

    async fn terminal(&self) -> AudioNotePhase {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let phase = self.status.get().await.phase;
            if matches!(phase, AudioNotePhase::Completed | AudioNotePhase::Error) {
                return phase;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "Timed out waiting for terminal capture phase"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn note(&self, id: i64) -> audetic::db::audio_notes::AudioNoteRecord {
        let conn = audetic::db::init_db_at(&self.db_path).unwrap();
        AudioNoteRepository::get(&conn, id).unwrap().unwrap()
    }
}

fn review() -> Option<AudioNoteStartOptions> {
    Some(AudioNoteStartOptions {
        capture_source: AudioNoteCaptureSource::MicrophoneAndSystem,
        review_before_processing: true,
        ..Default::default()
    })
}

#[tokio::test]
async fn idle_stop_cancel_and_confirm_are_rejected() {
    let mut f = Fixture::new(false);
    assert!(f.machine.stop().await.is_err());
    assert!(f.machine.cancel().await.is_err());
    assert!(f.machine.confirm(None, None).await.is_err());
}

#[tokio::test]
async fn active_start_is_rejected_without_losing_persisted_capture() {
    let mut f = Fixture::new(false);
    let first = f.machine.start(None).await.unwrap();
    assert!(f.machine.start(None).await.is_err());
    assert_eq!(f.status.get().await.note_id, Some(first.note_id));
    assert_eq!(f.status.get().await.phase, AudioNotePhase::Recording);
    assert_eq!(
        f.note(first.note_id).audio_path,
        first.audio_path.to_string_lossy()
    );
    f.machine.cancel().await.unwrap();
}

#[tokio::test]
async fn default_capture_is_mic_only_and_transcribes_immediately_on_stop() {
    let mut f = Fixture::new(false);
    let start = f.machine.start(None).await.unwrap();
    assert_eq!(start.capture_state.tag(), "mic_only");
    assert_eq!(f.system_starts.load(Ordering::SeqCst), 0);
    assert!(!f.status.get().await.capture_degraded);
    f.machine.stop().await.unwrap();
    assert_eq!(f.terminal().await, AudioNotePhase::Completed);
    let note = f.note(start.note_id);
    assert_eq!(note.capture_source, "microphone");
    assert_eq!(
        note.transcript_text.as_deref(),
        Some("hello world from the mock")
    );
    assert!(Path::new(&note.audio_path).starts_with(f.directory.path()));
    assert!(Path::new(&note.audio_path).exists());
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn dual_capture_review_waits_for_confirmation() {
    let mut f = Fixture::new(false);
    let start = f.machine.start(review()).await.unwrap();
    assert_eq!(start.capture_state.tag(), "both");
    assert_eq!(f.system_starts.load(Ordering::SeqCst), 1);
    f.machine.stop().await.unwrap();
    assert_eq!(f.status.get().await.phase, AudioNotePhase::Review);
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    assert!(f.machine.start(None).await.is_err());
    f.machine.confirm(None, None).await.unwrap();
    assert_eq!(f.terminal().await, AudioNotePhase::Completed);
    assert_eq!(
        f.note(start.note_id).capture_source,
        "microphone_and_system"
    );
}

#[tokio::test]
async fn cancel_during_recording_never_transcribes() {
    let mut f = Fixture::new(false);
    let start = f.machine.start(None).await.unwrap();
    assert_eq!(f.machine.cancel().await.unwrap().note_id, start.note_id);
    assert_eq!(f.status.get().await.phase, AudioNotePhase::Idle);
    assert!(f.status.get().await.note_id.is_none());
    assert_eq!(f.note(start.note_id).status, "cancelled");
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cancel_from_review_discards_audio_and_persists_cancelled() {
    let mut f = Fixture::new(false);
    let start = f.machine.start(review()).await.unwrap();
    f.machine.stop().await.unwrap();
    assert!(start.audio_path.exists());
    f.machine.cancel().await.unwrap();
    assert!(!start.audio_path.exists());
    assert_eq!(f.status.get().await.phase, AudioNotePhase::Idle);
    assert_eq!(f.note(start.note_id).status, "cancelled");
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn transcription_failure_preserves_audio_and_visible_error() {
    let mut f = Fixture::new(true);
    let start = f.machine.start(None).await.unwrap();
    f.machine.stop().await.unwrap();
    assert_eq!(f.terminal().await, AudioNotePhase::Error);
    assert!(f
        .status
        .get()
        .await
        .last_error
        .unwrap()
        .contains("mock transcription failure"));
    let note = f.note(start.note_id);
    assert_eq!(note.status, "error");
    assert!(note.error.unwrap().contains("mock transcription failure"));
    assert!(Path::new(&note.audio_path).exists());
}

#[tokio::test]
async fn review_trim_is_sample_accurate_and_persists_trimmed_duration() {
    let mut f = Fixture::new(false);
    let start = f.machine.start(review()).await.unwrap();
    f.machine.stop().await.unwrap();
    assert!(f.machine.confirm(Some(1.0), Some(0.5)).await.is_err());
    assert_eq!(f.status.get().await.phase, AudioNotePhase::Review);
    let result = f.machine.confirm(Some(0.25), Some(1.25)).await.unwrap();
    assert_eq!(result.duration_seconds, 1);
    assert_eq!(f.terminal().await, AudioNotePhase::Completed);
    assert_eq!(f.note(start.note_id).duration_seconds, Some(1));
}

#[tokio::test]
async fn a_new_capture_survives_completion_of_an_older_background_transcription() {
    struct PausedTranscription {
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }
    #[async_trait]
    impl TranscriptionJobService for PausedTranscription {
        async fn submit_and_poll(
            &self,
            _: &Path,
            _: Option<&str>,
        ) -> Result<TranscriptionJobResult> {
            self.entered.notify_one();
            self.release.notified().await;
            Ok(TranscriptionJobResult {
                text: "older note completed".into(),
                segments: None,
            })
        }
    }
    let mut f = Fixture::new(false);
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut services = ProcessingServices::new(
        Arc::new(PausedTranscription {
            entered: entered.clone(),
            release: release.clone(),
        }),
        f.db_path.clone(),
    );
    services.enrichment = Arc::new(NoEnrichment);
    f.machine = f.machine.with_processing_services(services).unwrap();
    let first = f.machine.start(None).await.unwrap();
    f.machine.stop().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), entered.notified())
        .await
        .unwrap();
    assert_eq!(f.status.get().await.phase, AudioNotePhase::Transcribing);
    let second = f.machine.start(None).await.unwrap();
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(10), async {
        while f.note(first.note_id).status != "completed" {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    tokio::task::yield_now().await;
    let active = f.status.get().await;
    assert_eq!(active.note_id, Some(second.note_id));
    assert_eq!(active.phase, AudioNotePhase::Recording);
    assert!(active.last_error.is_none());
    f.machine.cancel().await.unwrap();
}
