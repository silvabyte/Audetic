#![allow(clippy::arc_with_non_send_sync)]

use anyhow::Result;
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
    /// Mono samples at the *native* rate, accumulated by the cpal callback.
    samples: Arc<Mutex<Vec<f32>>>,
    active_input: Mutex<Option<ActiveInput>>,
    state: Arc<Mutex<RecordingState>>,
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

    fn with_backend(backend: Box<dyn CaptureBackend>) -> Self {
        Self {
            backend,
            samples: Arc::new(Mutex::new(Vec::new())),
            active_input: Mutex::new(None),
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

        // Stop any existing stream before starting new one
        self.cleanup_stream();

        // Clear samples buffer for new recording
        {
            let mut samples = self.samples.lock().unwrap();
            samples.clear();
            samples.shrink_to_fit(); // Free memory from previous recordings
        }

        debug!("Creating new audio stream");

        let samples_clone = self.samples.clone();
        let on_data: InputDataCallback = Box::new(move |data, channels| {
            push_mono_f32(data, channels, &samples_clone);
        });

        // Resolving Default Input stays user-initiated and lazy because the
        // native config lookup can gate on macOS microphone permission.
        let active_input = self.backend.start_default_input(on_data)?;
        info!("Started audio recording");

        *self.active_input.lock().unwrap() = Some(active_input);
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

        // End this native-rate segment before reading its samples.
        let Some(active_input) = self.active_input.lock().unwrap().take() else {
            *self.state.lock().unwrap() = RecordingState::Idle;
            return Err(anyhow::anyhow!(
                "Recording stopped but no input stream was active"
            ));
        };
        let native_sample_rate = active_input.native_sample_rate();
        drop(active_input);

        // Extract native-rate samples
        let native = {
            let samples_guard = self.samples.lock().unwrap();
            samples_guard.clone()
        };

        if native.is_empty() {
            *self.state.lock().unwrap() = RecordingState::Idle;
            return Err(anyhow::anyhow!("No audio samples recorded"));
        }

        // Resample from the device's native rate to the VTT target rate. This
        // is a no-op (early return) when they already match — e.g. Linux
        // devices that offer 16 kHz directly.
        let resampled = resample_mono_f32(&native, native_sample_rate, TARGET_SAMPLE_RATE)?;

        info!(
            "Stopping recording: {} native @ {} Hz → {} samples @ {} Hz",
            native.len(),
            native_sample_rate,
            resampled.len(),
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
        for sample in resampled {
            writer.write_sample(sample)?;
        }
        writer.finalize()?;

        // Clear samples and reset state
        {
            let mut samples = self.samples.lock().unwrap();
            samples.clear();
            samples.shrink_to_fit();
        }

        *self.state.lock().unwrap() = RecordingState::Idle;

        info!("Audio saved to: {:?}", output_path);
        Ok(output_path)
    }

    /// Cleanup any active stream
    fn cleanup_stream(&self) {
        let mut active_input = self.active_input.lock().unwrap();
        if let Some(input) = active_input.take() {
            debug!("Cleaning up audio stream");
            // Stream is automatically stopped when dropped
            drop(input);
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
