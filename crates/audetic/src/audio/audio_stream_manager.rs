#![allow(clippy::arc_with_non_send_sync)]

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, StreamTrait};
use hound::{WavSpec, WavWriter};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::{debug, error, info};

use super::input_device::{open_default_input, OpenInput};
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
    /// Default input device + native config, opened lazily on first
    /// `start_recording`. Acquiring it touches a CoreAudio audio unit which
    /// gates on the macOS mic TCC permission — doing it eagerly at boot wedges
    /// the whole daemon in `tccd` until the grant resolves (see
    /// [`crate::audio::input_device`]). `Mutex` because `start_recording`
    /// takes `&self`.
    input: Mutex<Option<OpenInput>>,
    /// Mono samples at the *native* rate, accumulated by the cpal callback.
    samples: Arc<Mutex<Vec<f32>>>,
    active_stream: Arc<Mutex<Option<cpal::Stream>>>,
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
        Ok(Self {
            input: Mutex::new(None),
            samples: Arc::new(Mutex::new(Vec::new())),
            active_stream: Arc::new(Mutex::new(None)),
            state: Arc::new(Mutex::new(RecordingState::Idle)),
        })
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

        // Open the device on first use. On macOS this is the call that gates on
        // the mic TCC permission; doing it here (a user-initiated record) rather
        // than at boot keeps the daemon responsive and lets the OS surface its
        // permission prompt at the right moment.
        let mut input = self.input.lock().unwrap();
        if input.is_none() {
            *input = Some(open_default_input("Dictation")?);
        }
        let input = input.as_ref().unwrap();

        let samples_clone = self.samples.clone();
        let channels = input.channels;
        let err_fn = |err| error!("Audio stream error: {}", err);

        let stream = input.device.build_input_stream(
            &input.config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                push_mono_f32(data, channels, &samples_clone);
            },
            err_fn,
            None,
        )?;

        stream.play()?;
        info!("Started audio recording");

        // Store stream for proper cleanup
        *self.active_stream.lock().unwrap() = Some(stream);
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

        // Stop and cleanup stream
        self.cleanup_stream();

        // Extract native-rate samples
        let native = {
            let samples_guard = self.samples.lock().unwrap();
            samples_guard.clone()
        };

        if native.is_empty() {
            *self.state.lock().unwrap() = RecordingState::Idle;
            return Err(anyhow::anyhow!("No audio samples recorded"));
        }

        // The device was opened by `start_recording`, so the native rate is
        // known. (If we somehow recorded without it, `native` would be empty
        // and we'd have bailed above.)
        let native_sample_rate = self
            .input
            .lock()
            .unwrap()
            .as_ref()
            .map(|i| i.native_sample_rate)
            .context("Recording stopped but input device was never opened")?;

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
        let mut active_stream = self.active_stream.lock().unwrap();
        if let Some(stream) = active_stream.take() {
            debug!("Cleaning up audio stream");
            // Stream is automatically stopped when dropped
            drop(stream);
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
    use super::*;

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
}
