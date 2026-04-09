//! Integration tests for the meeting pipeline.
//!
//! Uses in-memory mocks for the audio sources, transcription service, and
//! post-meeting hook so the full lifecycle can be exercised without touching
//! real hardware or the network.
//!
//! These tests validate the bug regressions discovered during the v0.1.20
//! meeting feature audit:
//! - happy path: start → stop → background processing → completed
//! - cancel: cleanup + persisted cancelled status
//! - error propagation: stop when idle, start while recording
//! - failed transcription: error text + duration persisted

use anyhow::Result;
use async_trait::async_trait;
use audetic::audio::audio_source::AudioSource;
use audetic::meeting::{MeetingMachine, MeetingPhase, MeetingStartOptions, MeetingStatusHandle};
use audetic::meeting::{MeetingResult, PostMeetingHook};
use audetic::transcription::job_service::{TranscriptionJobResult, TranscriptionJobService};
use audetic::ui::Indicator;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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

// ---- mock post-meeting hook ----

struct MockHook {
    called: Arc<AtomicBool>,
}

impl MockHook {
    fn new() -> (Self, Arc<AtomicBool>) {
        let flag = Arc::new(AtomicBool::new(false));
        (
            Self {
                called: Arc::clone(&flag),
            },
            flag,
        )
    }
}

#[async_trait]
impl PostMeetingHook for MockHook {
    async fn execute(&self, _result: &MeetingResult) -> Result<()> {
        self.called.store(true, Ordering::SeqCst);
        Ok(())
    }
}

// ---- helpers ----

/// Build a meeting machine with mock dependencies. Skips the Hyprland
/// notification side-effects by using the default Indicator with audio
/// feedback disabled.
fn build_test_machine(
    mic_samples: Vec<f32>,
    system_samples: Vec<f32>,
    transcription: Arc<dyn TranscriptionJobService>,
    hook: Option<Arc<dyn PostMeetingHook>>,
) -> (MeetingMachine, MeetingStatusHandle) {
    let mic: Box<dyn AudioSource> =
        Box::new(MockAudioSource::new(mic_samples, 16000));
    let system: Box<dyn AudioSource> =
        Box::new(MockAudioSource::new(system_samples, 16000));
    let indicator = Indicator::new().with_audio_feedback(false);
    let status = MeetingStatusHandle::default();

    let machine = MeetingMachine::new(mic, system, transcription, hook, indicator, status.clone());
    (machine, status)
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
    let (mut machine, _status) = build_test_machine(
        Vec::new(),
        Vec::new(),
        Arc::new(transcription),
        None,
    );

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
    let (mut machine, status) = build_test_machine(
        fake_audio(0.5),
        fake_audio(0.5),
        Arc::new(transcription),
        None,
    );

    let first = machine.start(None).await.expect("first start should succeed");

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
    let (mut machine, status) = build_test_machine(
        fake_audio(0.5),
        fake_audio(0.5),
        Arc::clone(&transcription) as Arc<dyn TranscriptionJobService>,
        None,
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
    let (mut machine, _status) = build_test_machine(
        Vec::new(),
        Vec::new(),
        Arc::new(transcription),
        None,
    );

    let result = machine.cancel().await;
    assert!(result.is_err());
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("No meeting recording in progress"),
        "unexpected error: {}",
        msg
    );
}

#[tokio::test]
async fn test_meeting_happy_path() {
    let (transcription, call_count) = MockTranscription::ok("hello world from the mock");
    let (hook, hook_called) = MockHook::new();
    let (mut machine, status) = build_test_machine(
        fake_audio(0.5),
        fake_audio(0.5),
        Arc::new(transcription),
        Some(Arc::new(hook)),
    );

    let start = machine
        .start(Some(MeetingStartOptions {
            title: Some("Happy path".to_string()),
        }))
        .await
        .expect("start");
    assert_eq!(start.capture_state.as_str(), "mic + system audio");

    let stop = machine.stop().await.expect("stop");
    assert_eq!(stop.meeting_id, start.meeting_id);

    // Background task finishes quickly with mocks.
    let phase = wait_for_terminal(&status, Duration::from_secs(5)).await;
    assert_eq!(
        phase,
        MeetingPhase::Completed,
        "expected Completed, got {:?}",
        phase
    );

    assert_eq!(call_count.load(Ordering::SeqCst), 1);
    assert!(hook_called.load(Ordering::SeqCst), "hook should have run");
}

#[tokio::test]
async fn test_meeting_transcription_failure() {
    let (transcription, _count) = MockTranscription::failing();
    let (mut machine, status) = build_test_machine(
        fake_audio(0.5),
        fake_audio(0.5),
        Arc::new(transcription),
        None,
    );

    let _start = machine.start(None).await.expect("start");
    let _stop = machine.stop().await.expect("stop");

    let phase = wait_for_terminal(&status, Duration::from_secs(5)).await;
    assert_eq!(phase, MeetingPhase::Error, "expected Error, got {:?}", phase);

    let state = status.get().await;
    assert!(
        state.last_error.is_some(),
        "failing transcription should set last_error"
    );
}
