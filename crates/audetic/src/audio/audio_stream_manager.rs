#![allow(clippy::arc_with_non_send_sync)]

use anyhow::{Context, Result};
use hound::{WavSpec, WavWriter};
use tracing::{debug, info, warn};

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::capture_recovery::{start_capture_with_retries, CaptureRecovery};
use super::input_device::{
    ActiveInput, CaptureBackend, CpalCaptureBackend, InputDataCallback, InputErrorCallback,
};
use super::resample::{push_mono_f32, resample_mono_f32};
use super::stream_event::{CaptureSource, StreamDeath, StreamEventSink, StreamGeneration};

/// Target sample rate the VTT pipeline (Whisper) expects. The device may
/// capture at a higher native rate; the WAV written on stop is at this rate.
const TARGET_SAMPLE_RATE: u32 = 16000; // Whisper optimal

/// State of the audio recording session
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RecordingState {
    Idle,
    Recording,
    Stopping,
}

enum ReplacementTrigger {
    DefaultInputSwitched,
    StreamDied(StreamDeath),
}

/// Manages the lifecycle of audio streams and recordings
pub struct AudioStreamManager {
    /// Device-free adapter that resolves Default Input for each recording.
    backend: Box<dyn CaptureBackend>,
    active_segment: Mutex<Option<ActiveSegment>>,
    /// Completed Segments normalized to the pipeline sample rate.
    canonical_samples: Mutex<Vec<f32>>,
    stream_generation: Mutex<StreamGeneration>,
    stream_event_sink: StreamEventSink,
    state: Arc<Mutex<RecordingState>>,
}

/// One contiguous interval captured at a single native sample rate.
struct ActiveSegment {
    input: ActiveInput,
    native_samples: Arc<Mutex<Vec<f32>>>,
    generation: StreamGeneration,
}

impl AudioStreamManager {
    /// Create a new audio stream manager.
    ///
    /// Does **not** open the audio device — that's deferred to the first
    /// `start_recording` so the daemon boots even when the mic TCC grant
    /// hasn't been resolved yet. Returns `Result` only to keep the call site
    /// stable; construction itself is infallible.
    pub fn new() -> Result<Self> {
        Self::with_event_sink(Arc::new(|_| {}))
    }

    pub(crate) fn with_event_sink(stream_event_sink: StreamEventSink) -> Result<Self> {
        Ok(Self::with_backend(
            Box::new(CpalCaptureBackend::new("Dictation")),
            stream_event_sink,
        ))
    }

    pub(crate) fn with_backend(
        backend: Box<dyn CaptureBackend>,
        stream_event_sink: StreamEventSink,
    ) -> Self {
        Self {
            backend,
            active_segment: Mutex::new(None),
            canonical_samples: Mutex::new(Vec::new()),
            stream_generation: Mutex::new(StreamGeneration(0)),
            stream_event_sink,
            state: Arc::new(Mutex::new(RecordingState::Idle)),
        }
    }

    /// Start recording audio, properly managing stream lifecycle
    pub async fn start_recording(&self) -> Result<()> {
        let mut state = self.state.lock().unwrap();

        match *state {
            RecordingState::Recording => {
                return Err(anyhow::anyhow!("Recording already in progress"));
            }
            RecordingState::Stopping => {
                return Err(anyhow::anyhow!("Previous recording still stopping"));
            }
            RecordingState::Idle => {}
        }

        // Stop any existing stream before starting a new recording.
        self.cleanup_stream();

        // Clear canonical audio from the previous recording.
        {
            let mut samples = self.canonical_samples.lock().unwrap();
            samples.clear();
            samples.shrink_to_fit(); // Free memory from previous recordings
        }

        debug!("Creating new audio stream");
        let segment = self
            .start_segment()
            .context("Failed to start initial recording Segment")?;
        info!("Started audio recording");

        *self.active_segment.lock().unwrap() = Some(segment);
        *state = RecordingState::Recording;

        Ok(())
    }

    /// Stop recording and save audio to file
    pub async fn stop_recording(&self, output_path: PathBuf) -> Result<PathBuf> {
        let mut state = self.state.lock().unwrap();

        match *state {
            RecordingState::Idle => {
                return Err(anyhow::anyhow!("No recording in progress"));
            }
            RecordingState::Stopping => {
                return Err(anyhow::anyhow!("Recording already stopping"));
            }
            RecordingState::Recording => {}
        }

        *state = RecordingState::Stopping;
        drop(state); // Release lock before cleanup

        // Degraded Capture has no active Segment, but prior canonical audio is
        // still a valid recording and must remain stoppable.
        self.close_current_segment()
            .context("Failed to close final recording Segment")?;

        let canonical = std::mem::take(&mut *self.canonical_samples.lock().unwrap());
        if canonical.is_empty() {
            *self.state.lock().unwrap() = RecordingState::Idle;
            return Err(anyhow::anyhow!("No audio samples recorded"));
        }

        info!(
            "Stopping recording: {} canonical samples @ {} Hz",
            canonical.len(),
            TARGET_SAMPLE_RATE
        );

        // Write WAV file
        let spec = WavSpec {
            channels: 1,
            sample_rate: TARGET_SAMPLE_RATE,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };

        let mut writer = WavWriter::create(&output_path, spec)?;
        for sample in canonical {
            writer.write_sample(sample)?;
        }
        writer.finalize()?;

        *self.state.lock().unwrap() = RecordingState::Idle;

        info!("Audio saved to: {:?}", output_path);
        Ok(output_path)
    }

    /// Follow the current Default Input while preserving the active dictation.
    /// Idle switches are ignored so a switch queued after stop cannot reopen capture.
    pub(crate) async fn default_input_switched(&self) -> Result<CaptureRecovery> {
        self.replace_capture(ReplacementTrigger::DefaultInputSwitched)
            .await
    }

    pub(crate) async fn stream_died(&self, death: StreamDeath) -> Result<CaptureRecovery> {
        self.replace_capture(ReplacementTrigger::StreamDied(death))
            .await
    }

    async fn replace_capture(&self, trigger: ReplacementTrigger) -> Result<CaptureRecovery> {
        if *self.state.lock().unwrap() != RecordingState::Recording {
            return Ok(CaptureRecovery::Ignored);
        }

        if let ReplacementTrigger::StreamDied(death) = trigger {
            let current_generation = self
                .active_segment
                .lock()
                .unwrap()
                .as_ref()
                .map(|segment| segment.generation);
            if death.source != CaptureSource::Dictation
                || current_generation != Some(death.generation)
            {
                return Ok(CaptureRecovery::Ignored);
            }
        }

        self.close_current_segment()
            .context("Failed to close Segment for capture replacement")?;

        match start_capture_with_retries("replacement Default Input", || self.start_segment()).await
        {
            Ok(segment) => {
                *self.active_segment.lock().unwrap() = Some(segment);
                info!("Recovered active dictation on the current Default Input");
                Ok(CaptureRecovery::Capturing)
            }
            Err(_) => {
                warn!("Dictation remains active in Degraded Capture");
                Ok(CaptureRecovery::Degraded)
            }
        }
    }

    fn start_segment(&self) -> Result<ActiveSegment> {
        let generation = {
            let current = self.stream_generation.lock().unwrap();
            current.next()?
        };
        let native_samples = Arc::new(Mutex::new(Vec::new()));
        let callback_samples = native_samples.clone();
        let on_data: InputDataCallback = Box::new(move |data, channels| {
            push_mono_f32(data, channels, &callback_samples);
        });
        let stream_event_sink = self.stream_event_sink.clone();
        let on_error: InputErrorCallback = Box::new(move || {
            stream_event_sink(StreamDeath {
                source: CaptureSource::Dictation,
                generation,
            });
        });

        // Resolving Default Input stays user-initiated and lazy because the
        // native config lookup can gate on macOS microphone permission.
        let input = self
            .backend
            .start_default_input(on_data, on_error)
            .context("Failed to start Segment from current Default Input")?;
        *self.stream_generation.lock().unwrap() = generation;
        Ok(ActiveSegment {
            input,
            native_samples,
            generation,
        })
    }

    fn close_current_segment(&self) -> Result<bool> {
        let Some(segment) = self.active_segment.lock().unwrap().take() else {
            return Ok(false);
        };

        let native_sample_rate = segment.input.native_sample_rate();
        drop(segment.input);
        let native = std::mem::take(&mut *segment.native_samples.lock().unwrap());
        let canonical = resample_mono_f32(&native, native_sample_rate, TARGET_SAMPLE_RATE)
            .context("Failed to normalize audio Segment")?;

        debug!(
            "Closed Segment: {} native @ {} Hz -> {} canonical @ {} Hz",
            native.len(),
            native_sample_rate,
            canonical.len(),
            TARGET_SAMPLE_RATE
        );
        self.canonical_samples.lock().unwrap().extend(canonical);
        Ok(true)
    }

    /// Cleanup any active stream
    fn cleanup_stream(&self) {
        let mut active_segment = self.active_segment.lock().unwrap();
        if let Some(segment) = active_segment.take() {
            debug!("Cleaning up audio stream");
            // Stream is automatically stopped when dropped
            drop(segment);
        }
    }
}

impl Drop for AudioStreamManager {
    fn drop(&mut self) {
        debug!("Dropping AudioStreamManager, cleaning up resources");
        self.cleanup_stream();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::super::input_device::{
        ActiveInput, CaptureBackend, InputDataCallback, InputErrorCallback,
    };
    use super::super::stream_event::{CaptureSource, StreamDeath, StreamEventSink};
    use super::*;

    #[derive(Clone)]
    struct FakeDefaultInput {
        sample_rate: u32,
        samples: Vec<f32>,
    }

    struct FakeCaptureBackend {
        current_default: Arc<Mutex<FakeDefaultInput>>,
        starts: Arc<AtomicUsize>,
        errors: Arc<Mutex<Vec<InputErrorCallback>>>,
    }

    impl CaptureBackend for FakeCaptureBackend {
        fn start_default_input(
            &self,
            mut on_data: InputDataCallback,
            on_error: InputErrorCallback,
        ) -> Result<ActiveInput> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            let current = self.current_default.lock().unwrap().clone();
            on_data(&current.samples, 1);
            self.errors.lock().unwrap().push(on_error);
            Ok(ActiveInput::new(current.sample_rate, ()))
        }
    }

    enum CapturePlan {
        Input(FakeDefaultInput),
        Fail(&'static str),
    }

    struct PlannedCaptureBackend {
        plans: Mutex<VecDeque<CapturePlan>>,
        starts: Arc<AtomicUsize>,
        errors: Arc<Mutex<Vec<InputErrorCallback>>>,
    }

    impl CaptureBackend for PlannedCaptureBackend {
        fn start_default_input(
            &self,
            mut on_data: InputDataCallback,
            on_error: InputErrorCallback,
        ) -> Result<ActiveInput> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            match self.plans.lock().unwrap().pop_front().unwrap() {
                CapturePlan::Input(input) => {
                    on_data(&input.samples, 1);
                    self.errors.lock().unwrap().push(on_error);
                    Ok(ActiveInput::new(input.sample_rate, ()))
                }
                CapturePlan::Fail(message) => Err(anyhow::anyhow!(message)),
            }
        }
    }

    /// Construction must not touch the audio device — it's deferred to the
    /// first `start_recording` so the daemon boots even when no device is
    /// present or the mic TCC grant is unresolved. This runs unconditionally
    /// (including in CI, which has no audio devices): if `new()` regressed to
    /// opening the device eagerly, this would fail without hardware.
    #[tokio::test]
    async fn new_does_not_open_audio_device() {
        let manager = AudioStreamManager::new();
        assert!(
            manager.is_ok(),
            "AudioStreamManager::new() must be infallible and device-free"
        );
    }

    #[tokio::test]
    async fn fresh_dictation_resolves_the_current_default_input() {
        let current_default = Arc::new(Mutex::new(FakeDefaultInput {
            sample_rate: 48_000,
            samples: vec![0.25; 480],
        }));
        let starts = Arc::new(AtomicUsize::new(0));
        let errors = Arc::new(Mutex::new(Vec::new()));
        let manager = AudioStreamManager::with_backend(
            Box::new(FakeCaptureBackend {
                current_default: current_default.clone(),
                starts: starts.clone(),
                errors,
            }),
            Arc::new(|_| {}),
        );
        assert_eq!(starts.load(Ordering::SeqCst), 0);

        let output_dir = tempfile::tempdir().unwrap();
        let first_path = output_dir.path().join("default-a.wav");
        manager.start_recording().await.unwrap();
        manager.stop_recording(first_path.clone()).await.unwrap();

        *current_default.lock().unwrap() = FakeDefaultInput {
            sample_rate: 44_100,
            samples: vec![-0.5; 441],
        };

        let second_path = output_dir.path().join("default-b.wav");
        manager.start_recording().await.unwrap();
        manager.stop_recording(second_path.clone()).await.unwrap();

        let first = read_samples(&first_path);
        let second = read_samples(&second_path);
        assert_eq!(first.len(), 160);
        assert_eq!(second.len(), 160);
        assert!(first.iter().all(|sample| *sample > 0.0));
        assert!(second.iter().all(|sample| *sample < 0.0));
        assert_eq!(starts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn live_segment_reports_its_source_and_stream_generation() {
        let current_default = Arc::new(Mutex::new(FakeDefaultInput {
            sample_rate: 48_000,
            samples: vec![0.25; 480],
        }));
        let errors = Arc::new(Mutex::new(Vec::new()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink: StreamEventSink = {
            let events = events.clone();
            Arc::new(move |event| events.lock().unwrap().push(event))
        };
        let manager = AudioStreamManager::with_backend(
            Box::new(FakeCaptureBackend {
                current_default,
                starts: Arc::new(AtomicUsize::new(0)),
                errors: errors.clone(),
            }),
            sink,
        );

        manager.start_recording().await.unwrap();
        errors.lock().unwrap()[0]();

        assert_eq!(
            *events.lock().unwrap(),
            vec![StreamDeath {
                source: CaptureSource::Dictation,
                generation: 1.into(),
            }]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn stream_death_exhausts_retries_but_preserves_prior_audio() {
        let starts = Arc::new(AtomicUsize::new(0));
        let errors = Arc::new(Mutex::new(Vec::new()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let manager = AudioStreamManager::with_backend(
            Box::new(PlannedCaptureBackend {
                plans: Mutex::new(VecDeque::from([
                    CapturePlan::Input(FakeDefaultInput {
                        sample_rate: 48_000,
                        samples: vec![0.25; 480],
                    }),
                    CapturePlan::Fail("replacement 1 failed"),
                    CapturePlan::Fail("replacement 2 failed"),
                    CapturePlan::Fail("replacement 3 failed"),
                ])),
                starts: starts.clone(),
                errors: errors.clone(),
            }),
            {
                let events = events.clone();
                Arc::new(move |event| events.lock().unwrap().push(event))
            },
        );
        let output_dir = tempfile::tempdir().unwrap();
        let output_path = output_dir.path().join("degraded.wav");

        manager.start_recording().await.unwrap();
        errors.lock().unwrap()[0]();
        let death = events.lock().unwrap().pop().unwrap();

        assert_eq!(
            manager.stream_died(death).await.unwrap(),
            CaptureRecovery::Degraded
        );
        assert_eq!(starts.load(Ordering::SeqCst), 4);
        assert_eq!(
            manager.stream_died(death).await.unwrap(),
            CaptureRecovery::Ignored
        );
        assert_eq!(starts.load(Ordering::SeqCst), 4);

        manager.stop_recording(output_path.clone()).await.unwrap();
        let samples = read_samples(&output_path);
        assert_eq!(samples.len(), 160);
        assert!(samples.iter().all(|sample| *sample > 0.0));
    }

    #[tokio::test(start_paused = true)]
    async fn later_default_input_switch_recovers_degraded_capture() {
        let errors = Arc::new(Mutex::new(Vec::new()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let manager = AudioStreamManager::with_backend(
            Box::new(PlannedCaptureBackend {
                plans: Mutex::new(VecDeque::from([
                    CapturePlan::Input(FakeDefaultInput {
                        sample_rate: 48_000,
                        samples: vec![0.25; 480],
                    }),
                    CapturePlan::Fail("replacement 1 failed"),
                    CapturePlan::Fail("replacement 2 failed"),
                    CapturePlan::Fail("replacement 3 failed"),
                    CapturePlan::Input(FakeDefaultInput {
                        sample_rate: 44_100,
                        samples: vec![-0.5; 441],
                    }),
                ])),
                starts: Arc::new(AtomicUsize::new(0)),
                errors: errors.clone(),
            }),
            {
                let events = events.clone();
                Arc::new(move |event| events.lock().unwrap().push(event))
            },
        );
        let output_dir = tempfile::tempdir().unwrap();
        let output_path = output_dir.path().join("recovered.wav");

        manager.start_recording().await.unwrap();
        errors.lock().unwrap()[0]();
        let death = events.lock().unwrap().pop().unwrap();
        assert_eq!(
            manager.stream_died(death).await.unwrap(),
            CaptureRecovery::Degraded
        );

        assert_eq!(
            manager.default_input_switched().await.unwrap(),
            CaptureRecovery::Capturing
        );
        manager.stop_recording(output_path.clone()).await.unwrap();

        let samples = read_samples(&output_path);
        assert_eq!(samples.len(), 320);
        assert!(samples[..160].iter().all(|sample| *sample > 0.0));
        assert!(samples[160..].iter().all(|sample| *sample < 0.0));
    }

    #[tokio::test]
    async fn stale_stream_death_does_not_rebuild_or_change_capture_health() {
        let starts = Arc::new(AtomicUsize::new(0));
        let errors = Arc::new(Mutex::new(Vec::new()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let manager = AudioStreamManager::with_backend(
            Box::new(PlannedCaptureBackend {
                plans: Mutex::new(VecDeque::from([
                    CapturePlan::Input(FakeDefaultInput {
                        sample_rate: 48_000,
                        samples: vec![0.25; 480],
                    }),
                    CapturePlan::Input(FakeDefaultInput {
                        sample_rate: 44_100,
                        samples: vec![-0.5; 441],
                    }),
                ])),
                starts: starts.clone(),
                errors: errors.clone(),
            }),
            {
                let events = events.clone();
                Arc::new(move |event| events.lock().unwrap().push(event))
            },
        );

        manager.start_recording().await.unwrap();
        assert_eq!(
            manager.default_input_switched().await.unwrap(),
            CaptureRecovery::Capturing
        );
        errors.lock().unwrap()[0]();
        let stale_death = events.lock().unwrap().pop().unwrap();

        assert_eq!(
            manager.stream_died(stale_death).await.unwrap(),
            CaptureRecovery::Ignored
        );
        assert_eq!(starts.load(Ordering::SeqCst), 2);
    }

    fn read_samples(path: &std::path::Path) -> Vec<f32> {
        hound::WavReader::open(path)
            .unwrap()
            .samples::<f32>()
            .collect::<std::result::Result<_, _>>()
            .unwrap()
    }
}
