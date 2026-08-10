#![allow(clippy::arc_with_non_send_sync)]

use anyhow::{Context, Result};
use hound::{WavSpec, WavWriter};
use tracing::{debug, info};

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::input_device::{ActiveInput, CaptureBackend, CpalCaptureBackend, InputDataCallback};
use super::resample::{push_mono_f32, resample_mono_f32};

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

/// Manages the lifecycle of audio streams and recordings
pub struct AudioStreamManager {
    /// Device-free adapter that resolves Default Input for each recording.
    backend: Box<dyn CaptureBackend>,
    active_segment: Mutex<Option<ActiveSegment>>,
    /// Completed Segments normalized to the pipeline sample rate.
    canonical_samples: Mutex<Vec<f32>>,
    state: Arc<Mutex<RecordingState>>,
}

/// One contiguous interval captured at a single native sample rate.
struct ActiveSegment {
    input: ActiveInput,
    native_samples: Arc<Mutex<Vec<f32>>>,
}

impl AudioStreamManager {
    /// Create a new audio stream manager.
    ///
    /// Does **not** open the audio device — that's deferred to the first
    /// `start_recording` so the daemon boots even when the mic TCC grant
    /// hasn't been resolved yet. Returns `Result` only to keep the call site
    /// stable; construction itself is infallible.
    pub fn new() -> Result<Self> {
        Ok(Self::with_backend(Box::new(CpalCaptureBackend::new(
            "Dictation",
        ))))
    }

    pub(crate) fn with_backend(backend: Box<dyn CaptureBackend>) -> Self {
        Self {
            backend,
            active_segment: Mutex::new(None),
            canonical_samples: Mutex::new(Vec::new()),
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

        // End and normalize the final native-rate Segment.
        if !self
            .close_current_segment()
            .context("Failed to close final recording Segment")?
        {
            *self.state.lock().unwrap() = RecordingState::Idle;
            return Err(anyhow::anyhow!(
                "Recording stopped but no input stream was active"
            ));
        }

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
    pub fn default_input_switched(&self) -> Result<()> {
        if *self.state.lock().unwrap() != RecordingState::Recording {
            return Ok(());
        }

        self.close_current_segment()
            .context("Failed to close Segment for Default Input switch")?;
        let segment = self
            .start_segment()
            .context("Failed to start replacement Segment after Default Input switch")?;
        *self.active_segment.lock().unwrap() = Some(segment);
        info!("Switched active dictation to the current Default Input");
        Ok(())
    }

    fn start_segment(&self) -> Result<ActiveSegment> {
        let native_samples = Arc::new(Mutex::new(Vec::new()));
        let callback_samples = native_samples.clone();
        let on_data: InputDataCallback = Box::new(move |data, channels| {
            push_mono_f32(data, channels, &callback_samples);
        });

        // Resolving Default Input stays user-initiated and lazy because the
        // native config lookup can gate on macOS microphone permission.
        let input = self
            .backend
            .start_default_input(on_data)
            .context("Failed to start Segment from current Default Input")?;
        Ok(ActiveSegment {
            input,
            native_samples,
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::super::input_device::{ActiveInput, CaptureBackend, InputDataCallback};
    use super::*;

    #[derive(Clone)]
    struct FakeDefaultInput {
        sample_rate: u32,
        samples: Vec<f32>,
    }

    struct FakeCaptureBackend {
        current_default: Arc<Mutex<FakeDefaultInput>>,
        starts: Arc<AtomicUsize>,
    }

    impl CaptureBackend for FakeCaptureBackend {
        fn start_default_input(&self, mut on_data: InputDataCallback) -> Result<ActiveInput> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            let current = self.current_default.lock().unwrap().clone();
            on_data(&current.samples, 1);
            Ok(ActiveInput::new(current.sample_rate, ()))
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
        let manager = AudioStreamManager::with_backend(Box::new(FakeCaptureBackend {
            current_default: current_default.clone(),
            starts: starts.clone(),
        }));
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

    fn read_samples(path: &std::path::Path) -> Vec<f32> {
        hound::WavReader::open(path)
            .unwrap()
            .samples::<f32>()
            .collect::<std::result::Result<_, _>>()
            .unwrap()
    }
}
