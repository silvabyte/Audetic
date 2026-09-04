//! Integration tests for the meeting pipeline.
//!
//! Uses in-memory mocks for the audio sources, transcription service, and
//! post-meeting hook so the full lifecycle can be exercised without touching
//! real hardware or the network. The one exception is the compression step:
//! `test_meeting_happy_path` runs real `ffmpeg` (via the same code path the
//! daemon uses), so `ffmpeg` with libmp3lame must be on PATH or that test
//! ends in `Error` instead of `Completed`.
//!
//! These tests validate the bug regressions discovered during the v0.1.20
//! meeting feature audit:
//! - happy path: start → stop → background processing → completed
//! - cancel: cleanup + persisted cancelled status
//! - error propagation: stop when idle, start while recording
//! - failed transcription: error text + duration persisted

use anyhow::Result;
use async_trait::async_trait;
use audetic::audio::audio_source::{AudioSource, MeetingMicSource, MeetingSystemSource};
use audetic::meeting::{MeetingMachine, MeetingPhase, MeetingStartOptions, MeetingStatusHandle};
use audetic::post_processing::PostProcessingService;
use audetic::transcription::job_service::{TranscriptionJobResult, TranscriptionJobService};
use audetic::ui::Indicator;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

// ---- mock audio source ----

/// In-memory audio source that yields a canned buffer on stop().
struct MockAudioSource {
    samples: Vec<f32>,
    rate: u32,
    active: bool,
}

impl MockAudioSource {
    fn new(samples: Vec<f32>, rate: u32) -> Self {
        Self {
            samples,
            rate,
            active: false,
        }
    }
}

impl AudioSource for MockAudioSource {
    fn start(&mut self) -> Result<()> {
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
        self.rate
    }
}

#[async_trait(?Send)]
impl MeetingMicSource for MockAudioSource {
    fn has_captured_audio(&self) -> bool {
        !self.samples.is_empty()
    }
}

#[async_trait(?Send)]
impl MeetingSystemSource for MockAudioSource {
    fn has_captured_audio(&self) -> bool {
        !self.samples.is_empty()
    }
}

// ---- mock transcription service ----

struct MockTranscription {
    text: String,
    should_fail: bool,
    call_count: Arc<AtomicUsize>,
}

impl MockTranscription {
    fn ok(text: &str) -> (Self, Arc<AtomicUsize>) {
        let counter = Arc::new(AtomicUsize::new(0));
        (
            Self {
                text: text.to_string(),
                should_fail: false,
                call_count: Arc::clone(&counter),
            },
            counter,
        )
    }

    fn failing() -> (Self, Arc<AtomicUsize>) {
        let counter = Arc::new(AtomicUsize::new(0));
        (
            Self {
                text: String::new(),
                should_fail: true,
                call_count: Arc::clone(&counter),
            },
            counter,
        )
    }
}

#[async_trait]
impl TranscriptionJobService for MockTranscription {
    async fn submit_and_poll(
        &self,
        _file_path: &Path,
        _language: Option<&str>,
    ) -> Result<TranscriptionJobResult> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        if self.should_fail {
            anyhow::bail!("mock transcription failure");
        }
        Ok(TranscriptionJobResult {
            text: self.text.clone(),
            segments: None,
        })
    }
}

// ---- helpers ----

/// Build a meeting machine with mock dependencies. Skips the Hyprland
/// notification side-effects by using the default Indicator with audio
/// feedback disabled. Post-processing is a no-op in tests — no DB rows
/// means dispatch fans out to zero jobs.
fn build_test_machine(
    mic_samples: Vec<f32>,
    system_samples: Vec<f32>,
    transcription: Arc<dyn TranscriptionJobService>,
) -> (MeetingMachine, MeetingStatusHandle, std::path::PathBuf) {
    let mic: Box<dyn MeetingMicSource> = Box::new(MockAudioSource::new(mic_samples, 16000));
    let system: Box<dyn MeetingSystemSource> =
        Box::new(MockAudioSource::new(system_samples, 16000));
    let indicator = Indicator::new().with_audio_feedback(false);
    let status = MeetingStatusHandle::default();

    // Each test gets its own meetings dir and database under /tmp so concurrent
    // tests cannot clobber one another or write into the user's database.
    let meetings_dir = tempfile::tempdir()
        .expect("create test meetings dir")
        .keep();
    let db_path = meetings_dir.join("audetic.db");
    audetic::db::migrate_db_at(&db_path).expect("initialize test database");
    let post_processing = Arc::new(PostProcessingService::new(db_path.clone()));
    let machine = MeetingMachine::new(
        mic,
        system,
        transcription,
        post_processing,
        indicator,
        status.clone(),
        meetings_dir,
    );
    (machine, status, db_path)
}

/// Generate a small sine-ish buffer so downstream ffmpeg has real audio.
fn fake_audio(seconds: f32) -> Vec<f32> {
    let n = (16000.0 * seconds) as usize;
    (0..n)
        .map(|i| {
            let t = i as f32 / 16000.0;
            (t * 440.0 * 2.0 * std::f32::consts::PI).sin() * 0.2
        })
        .collect()
}

/// Poll status until it reaches a terminal phase or times out.
async fn wait_for_terminal(status: &MeetingStatusHandle, timeout: Duration) -> MeetingPhase {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let state = status.get().await;
        if matches!(
            state.phase,
            MeetingPhase::Completed
                | MeetingPhase::Error
                | MeetingPhase::Cancelled
                | MeetingPhase::Idle
        ) {
            return state.phase;
        }
        if std::time::Instant::now() > deadline {
            return state.phase;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ---- tests ----

#[tokio::test]
async fn test_meeting_stop_when_idle_errors() {
    let (transcription, _count) = MockTranscription::ok("ignored");
    let (mut machine, _status, _db_path) =
        build_test_machine(Vec::new(), Vec::new(), Arc::new(transcription));

    let result = machine.stop().await;
    assert!(
        result.is_err(),
        "stop() when idle must return Err, got {:?}",
        result.map(|r| r.meeting_id)
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("No meeting recording in progress"),
        "unexpected error: {}",
        err
    );
}

#[tokio::test]
async fn test_meeting_start_while_recording_errors() {
    let (transcription, _count) = MockTranscription::ok("ignored");
    let (mut machine, status, db_path) =
        build_test_machine(fake_audio(0.5), fake_audio(0.5), Arc::new(transcription));

    let first = machine
        .start(None)
        .await
        .expect("first start should succeed");

    let conn = audetic::db::init_db_at(&db_path).expect("open isolated test database");
    let persisted = audetic::db::meetings::MeetingRepository::get(&conn, first.meeting_id)
        .expect("query isolated test database")
        .expect("meeting should be persisted in isolated test database");
    assert_eq!(persisted.audio_path, first.audio_path.to_string_lossy());

    let second = machine.start(None).await;
    assert!(second.is_err(), "second start must return Err");
    let msg = format!("{}", second.unwrap_err());
    assert!(
        msg.contains("already in progress"),
        "unexpected error: {}",
        msg
    );

    // The original meeting should still be the active one.
    let state = status.get().await;
    assert_eq!(state.meeting_id, Some(first.meeting_id));
    assert_eq!(state.phase, MeetingPhase::Recording);

    // Clean up so cancel() is exercised and the test doesn't leak recording
    // state into subsequent tests (each test has its own in-memory DB row
    // since they share the user DB, but we cancel to restore Idle).
    let _ = machine.cancel().await;
}

#[tokio::test]
async fn test_meeting_cancel_during_recording() {
    let (transcription, count) = MockTranscription::ok("should not be called");
    let transcription = Arc::new(transcription);
    let (mut machine, status, _db_path) = build_test_machine(
        fake_audio(0.5),
        fake_audio(0.5),
        Arc::clone(&transcription) as Arc<dyn TranscriptionJobService>,
    );

    let start = machine.start(None).await.expect("start should succeed");
    assert_eq!(start.capture_state.as_str(), "mic + system audio");

    let cancel = machine.cancel().await.expect("cancel should succeed");
    assert_eq!(cancel.meeting_id, start.meeting_id);

    // Status handle should end up Idle (reset() is called after cancelled()).
    let state = status.get().await;
    assert_eq!(state.phase, MeetingPhase::Idle);
    assert!(state.meeting_id.is_none());

    // Transcription must NOT have been triggered.
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn test_meeting_cancel_when_idle_errors() {
    let (transcription, _count) = MockTranscription::ok("ignored");
    let (mut machine, _status, _db_path) =
        build_test_machine(Vec::new(), Vec::new(), Arc::new(transcription));

    let result = machine.cancel().await;
    assert!(result.is_err());
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("No meeting recording or awaiting review"),
        "unexpected error: {}",
        msg
    );
}

#[tokio::test]
async fn test_meeting_happy_path() {
    let (transcription, call_count) = MockTranscription::ok("hello world from the mock");
    let (mut machine, status, _db_path) =
        build_test_machine(fake_audio(0.5), fake_audio(0.5), Arc::new(transcription));

    let start = machine
        .start(Some(MeetingStartOptions {
            title: Some("Happy path".to_string()),
        }))
        .await
        .expect("start");
    assert_eq!(start.capture_state.as_str(), "mic + system audio");

    let stop = machine.stop().await.expect("stop");
    assert_eq!(stop.meeting_id, start.meeting_id);

    // Stop now pauses for review — nothing is transcribed until confirmed.
    assert_eq!(status.get().await.phase, MeetingPhase::Review);
    assert_eq!(call_count.load(Ordering::SeqCst), 0);

    // Confirm without trimming sends it down the pipeline.
    machine.confirm(None, None).await.expect("confirm");

    // Background task finishes quickly with mocks.
    let phase = wait_for_terminal(&status, Duration::from_secs(5)).await;
    assert_eq!(
        phase,
        MeetingPhase::Completed,
        "expected Completed, got {:?}",
        phase
    );

    assert_eq!(call_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_meeting_transcription_failure() {
    let (transcription, _count) = MockTranscription::failing();
    let (mut machine, status, _db_path) =
        build_test_machine(fake_audio(0.5), fake_audio(0.5), Arc::new(transcription));

    let _start = machine.start(None).await.expect("start");
    let _stop = machine.stop().await.expect("stop");
    machine.confirm(None, None).await.expect("confirm");

    let phase = wait_for_terminal(&status, Duration::from_secs(5)).await;
    assert_eq!(
        phase,
        MeetingPhase::Error,
        "expected Error, got {:?}",
        phase
    );

    let state = status.get().await;
    assert!(
        state.last_error.is_some(),
        "failing transcription should set last_error"
    );
}

#[tokio::test]
async fn test_confirm_when_not_in_review_errors() {
    let (transcription, _count) = MockTranscription::ok("ignored");
    let (mut machine, _status, _db_path) =
        build_test_machine(Vec::new(), Vec::new(), Arc::new(transcription));

    // No meeting recorded yet — confirm has nothing to act on.
    let result = machine.confirm(None, None).await;
    assert!(result.is_err());
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("No meeting awaiting review"),
        "unexpected error: {}",
        msg
    );
}

#[tokio::test]
async fn test_cancel_from_review_discards_recording() {
    let (transcription, count) = MockTranscription::ok("should not be called");
    let (mut machine, status, _db_path) =
        build_test_machine(fake_audio(0.5), fake_audio(0.5), Arc::new(transcription));

    let _start = machine.start(None).await.expect("start");
    let stop = machine.stop().await.expect("stop");

    // We're parked in Review with the WAV on disk.
    let review_state = status.get().await;
    assert_eq!(review_state.phase, MeetingPhase::Review);
    let audio_path = review_state.audio_path.clone().expect("audio path");
    assert!(audio_path.exists(), "WAV should exist while in review");

    let cancel = machine.cancel().await.expect("cancel from review");
    assert_eq!(cancel.meeting_id, stop.meeting_id);

    // Recording discarded: file removed, status reset, nothing transcribed.
    assert!(!audio_path.exists(), "WAV should be removed on cancel");
    assert_eq!(status.get().await.phase, MeetingPhase::Idle);
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn test_confirm_with_trim_shortens_duration() {
    let (transcription, _count) = MockTranscription::ok("trimmed");
    // 2 seconds of audio so a [0.5, 1.0) trim is well within range.
    let (mut machine, status, _db_path) =
        build_test_machine(fake_audio(2.0), fake_audio(2.0), Arc::new(transcription));

    let _start = machine.start(None).await.expect("start");
    machine.stop().await.expect("stop");
    assert_eq!(status.get().await.phase, MeetingPhase::Review);

    let confirmed = machine
        .confirm(Some(0.25), Some(1.25))
        .await
        .expect("confirm with trim");

    // [0.25s, 1.25s) == exactly 1.0s of audio.
    assert_eq!(confirmed.duration_seconds, 1);
}
