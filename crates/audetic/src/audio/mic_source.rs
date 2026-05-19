//! Microphone audio capture via cpal.
//!
//! Independent from `AudioStreamManager` — this is used exclusively by the
//! meeting recording pipeline. The existing voice-to-text pipeline uses
//! `AudioStreamManager` and is not modified.
//!
//! cpal 0.17 stopped doing transparent format conversion at stream-build
//! time, so we capture at the device's *default* config (e.g. 48 kHz
//! stereo on a typical Mac), mix to mono in the callback, then resample to
//! the meeting pipeline's target rate on `stop()` via [`crate::audio::resample`].

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
use std::sync::{Arc, Mutex};
use tracing::{debug, error, info};

use super::audio_source::AudioSource;
use super::resample::{push_mono_f32, resample_mono_f32};

pub struct MicAudioSource {
    device: cpal::Device,
    /// Device's native config — used for `build_input_stream`.
    config: cpal::StreamConfig,
    /// Native sample rate from the device config; the value we resample
    /// from at `stop()` time.
    native_sample_rate: u32,
    /// Channel count of the native stream; the callback mixes this many
    /// channels into mono.
    channels: usize,
    /// Mono samples at the *native* rate, accumulated by the cpal callback.
    samples: Arc<Mutex<Vec<f32>>>,
    stream: Option<cpal::Stream>,
    active: bool,
    target_sample_rate: u32,
}

impl MicAudioSource {
    /// Create a new mic source using the default input device.
    ///
    /// # Arguments
    /// * `sample_rate` - Target sample rate after resampling (e.g. 16000 for
    ///   Whisper). The device may capture at a higher native rate; the
    ///   returned buffer from `stop()` is at this target rate.
    pub fn new(sample_rate: u32) -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .context("No input device available for meeting mic capture")?;

        let supported = device
            .default_input_config()
            .context("Failed to read default input config for mic device")?;

        info!(
            "Meeting mic source using device: {} ({} ch, {} Hz, {:?})",
            device
                .description()
                .map(|d| d.name().to_string())
                .unwrap_or_else(|_| "unknown".to_string()),
            supported.channels(),
            supported.sample_rate(),
            supported.sample_format()
        );

        // Restrict to f32 for the same reason as the loopback source: it's
        // the CoreAudio lingua franca and avoids a fan-out across sample
        // formats. Most cpal hosts deliver f32 by default.
        if supported.sample_format() != SampleFormat::F32 {
            return Err(anyhow!(
                "Default input device uses {:?} samples — only f32 is supported",
                supported.sample_format()
            ));
        }

        let channels = supported.channels() as usize;
        let native_sample_rate = supported.sample_rate();
        let config: cpal::StreamConfig = supported.into();

        Ok(Self {
            device,
            config,
            native_sample_rate,
            channels,
            samples: Arc::new(Mutex::new(Vec::new())),
            stream: None,
            active: false,
            target_sample_rate: sample_rate,
        })
    }
}

impl AudioSource for MicAudioSource {
    fn start(&mut self) -> Result<()> {
        if self.active {
            return Err(anyhow!("Mic source already recording"));
        }

        // Clear previous samples
        {
            let mut samples = self.samples.lock().unwrap();
            samples.clear();
            samples.shrink_to_fit();
        }

        let samples_clone = self.samples.clone();
        let channels = self.channels;
        let err_fn = |err| error!("Meeting mic stream error: {err}");

        let stream = self.device.build_input_stream(
            &self.config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                push_mono_f32(data, channels, &samples_clone);
            },
            err_fn,
            None,
        )?;

        stream.play()?;
        self.stream = Some(stream);
        self.active = true;

        info!("Meeting mic recording started");
        Ok(())
    }

    fn stop(&mut self) -> Result<Vec<f32>> {
        if !self.active {
            return Err(anyhow!("Mic source not recording"));
        }

        if let Some(stream) = self.stream.take() {
            debug!("Stopping meeting mic stream");
            drop(stream);
        }

        self.active = false;

        let native = {
            let mut guard = self.samples.lock().unwrap();
            let s = std::mem::take(&mut *guard);
            guard.shrink_to_fit();
            s
        };

        let resampled =
            resample_mono_f32(&native, self.native_sample_rate, self.target_sample_rate)?;
        info!(
            "Meeting mic stopped: {} native @ {} Hz → {} samples @ {} Hz",
            native.len(),
            self.native_sample_rate,
            resampled.len(),
            self.target_sample_rate
        );
        Ok(resampled)
    }

    fn is_active(&self) -> bool {
        self.active
    }

    fn sample_rate(&self) -> u32 {
        self.target_sample_rate
    }
}

impl Drop for MicAudioSource {
    fn drop(&mut self) {
        if self.active {
            debug!("Dropping active MicAudioSource, cleaning up");
            let _ = self.stop();
        }
    }
}
