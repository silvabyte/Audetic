//! Microphone audio capture via cpal.
//!
//! Independent from `AudioStreamManager` — this is used exclusively by the
//! meeting recording pipeline. The existing voice-to-text pipeline uses
//! `AudioStreamManager` and is not modified.

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex};
use tracing::{debug, error, info};

use super::audio_source::AudioSource;

pub struct MicAudioSource {
    device: cpal::Device,
    config: cpal::StreamConfig,
    samples: Arc<Mutex<Vec<f32>>>,
    stream: Option<cpal::Stream>,
    active: bool,
    target_sample_rate: u32,
}

impl MicAudioSource {
    /// Create a new mic source using the default input device.
    ///
    /// # Arguments
    /// * `sample_rate` - Target sample rate (e.g., 16000 for Whisper optimal)
    pub fn new(sample_rate: u32) -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .context("No input device available for meeting mic capture")?;

        info!(
            "Meeting mic source using device: {}",
            device.name().unwrap_or_else(|_| "unknown".to_string())
        );

        let config = cpal::StreamConfig {
            channels: 1,
            sample_rate: cpal::SampleRate(sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        Ok(Self {
            device,
            config,
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
            return Err(anyhow::anyhow!("Mic source already recording"));
        }

        // Clear previous samples
        {
            let mut samples = self.samples.lock().unwrap();
            samples.clear();
            samples.shrink_to_fit();
        }

        let samples_clone = self.samples.clone();
        let err_fn = |err| error!("Meeting mic stream error: {}", err);

        let stream = self.device.build_input_stream(
            &self.config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                if let Ok(mut samples) = samples_clone.lock() {
                    samples.extend_from_slice(data);
                }
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
            return Err(anyhow::anyhow!("Mic source not recording"));
        }

        // Drop stream to stop recording
        if let Some(stream) = self.stream.take() {
            debug!("Stopping meeting mic stream");
            drop(stream);
        }

        self.active = false;

        let samples = {
            let mut guard = self.samples.lock().unwrap();
            let s = guard.clone();
            guard.clear();
            guard.shrink_to_fit();
            s
        };

        info!("Meeting mic stopped, {} samples captured", samples.len());
        Ok(samples)
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
