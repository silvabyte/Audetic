#![allow(clippy::arc_with_non_send_sync)]

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
use hound::{WavSpec, WavWriter};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::{debug, error, info};

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
    device: cpal::Device,
    /// Device's native config — used for `build_input_stream`. cpal 0.17 no
    /// longer converts formats at build time, so we must request a config the
    /// device actually accepts (see `audio/resample.rs`).
    config: cpal::StreamConfig,
    /// Native sample rate from the device config; the rate we resample *from*
    /// on stop.
    native_sample_rate: u32,
    /// Channel count of the native stream; the callback mixes this many
    /// channels into mono.
    channels: usize,
    /// Mono samples at the *native* rate, accumulated by the cpal callback.
    samples: Arc<Mutex<Vec<f32>>>,
    active_stream: Arc<Mutex<Option<cpal::Stream>>>,
    state: Arc<Mutex<RecordingState>>,
}

impl AudioStreamManager {
    /// Create a new audio stream manager
    pub fn new() -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .context("No input device available")?;

        let supported = device
            .default_input_config()
            .context("Failed to read default input config for audio device")?;

        info!(
            "Using audio device: {} ({} ch, {} Hz, {:?})",
            device
                .description()
                .map(|d| d.name().to_string())
                .unwrap_or_else(|_| "unknown".to_string()),
            supported.channels(),
            supported.sample_rate(),
            supported.sample_format()
        );

        // The capture callback is typed `&[f32]`, so the device must deliver
        // f32. Bail with a clear error rather than silently mis-decoding —
        // same restriction as `mic_source.rs`. Most cpal hosts deliver f32.
        if supported.sample_format() != SampleFormat::F32 {
            return Err(anyhow!(
                "Default input device uses {:?} samples — only f32 is supported",
                supported.sample_format()
            ));
        }

        let channels = supported.channels() as usize;
        let native_sample_rate = supported.sample_rate();
        // Capture at the device's native config. cpal 0.17's CoreAudio backend
        // does not resample — forcing 16 kHz here returns
        // StreamConfigNotSupported on devices that only offer 48 kHz (e.g. the
        // built-in MacBook mic). We resample to TARGET_SAMPLE_RATE on stop.
        let config: cpal::StreamConfig = supported.into();

        Ok(Self {
            device,
            config,
            native_sample_rate,
            channels,
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

        let samples_clone = self.samples.clone();
        let channels = self.channels;
        let err_fn = |err| error!("Audio stream error: {}", err);

        let stream = self.device.build_input_stream(
            &self.config,
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

        // Resample from the device's native rate to the VTT target rate. This
        // is a no-op (early return) when they already match — e.g. Linux
        // devices that offer 16 kHz directly.
        let resampled = resample_mono_f32(&native, self.native_sample_rate, TARGET_SAMPLE_RATE)?;

        info!(
            "Stopping recording: {} native @ {} Hz → {} samples @ {} Hz",
            native.len(),
            self.native_sample_rate,
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

    fn is_ci() -> bool {
        std::env::var("CI").is_ok()
            || std::env::var("GITHUB_ACTIONS").is_ok()
            || std::env::var("GITLAB_CI").is_ok()
            || std::env::var("TRAVIS").is_ok()
    }

    #[tokio::test]
    async fn test_audio_stream_manager_creation() {
        if is_ci() {
            // Skip audio tests in CI - no audio devices available
            return;
        }

        // This test may fail in CI without audio devices
        let _manager = AudioStreamManager::new();
    }
}
