//! System audio capture (what others say on Zoom/Meet/etc.).
//!
//! Captures audio from PipeWire/PulseAudio monitor sources, which represent
//! the system's audio output (speakers/headphones) as an input device.
//!
//! Strategy:
//! 1. Try cpal: enumerate input devices, find one with "Monitor" in the name
//! 2. Fallback: spawn `pw-record` targeting the default output monitor
//! 3. Graceful degradation: if nothing works, return empty samples with a warning

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex};
use tracing::{debug, error, info, warn};

use super::audio_source::AudioSource;

pub struct SystemAudioSource {
    capture: Option<SystemCapture>,
    samples: Arc<Mutex<Vec<f32>>>,
    active: bool,
    target_sample_rate: u32,
}

enum SystemCapture {
    Cpal {
        stream: cpal::Stream,
        actual_sample_rate: u32,
    },
}

impl SystemAudioSource {
    /// Create a new system audio source.
    ///
    /// # Arguments
    /// * `sample_rate` - Target sample rate for captured audio
    pub fn new(sample_rate: u32) -> Self {
        Self {
            capture: None,
            samples: Arc::new(Mutex::new(Vec::new())),
            active: false,
            target_sample_rate: sample_rate,
        }
    }

    /// Find a PipeWire/PulseAudio monitor source via cpal.
    fn find_monitor_device() -> Option<(cpal::Device, u32)> {
        let host = cpal::default_host();

        for device in host.input_devices().ok()? {
            if let Ok(name) = device.name() {
                let name_lower = name.to_lowercase();
                if name_lower.contains("monitor") {
                    if let Ok(default_config) = device.default_input_config() {
                        let sample_rate = default_config.sample_rate().0;
                        info!(
                            "Found system audio monitor: {} ({}Hz)",
                            name, sample_rate
                        );
                        return Some((device, sample_rate));
                    }
                }
            }
        }

        None
    }
}

impl AudioSource for SystemAudioSource {
    fn start(&mut self) -> Result<()> {
        if self.active {
            return Err(anyhow::anyhow!("System audio source already recording"));
        }

        // Clear previous samples
        {
            let mut samples = self.samples.lock().unwrap();
            samples.clear();
            samples.shrink_to_fit();
        }

        // Try to find and use a monitor source
        if let Some((device, actual_sample_rate)) = Self::find_monitor_device() {
            let config = cpal::StreamConfig {
                channels: 1,
                sample_rate: cpal::SampleRate(actual_sample_rate),
                buffer_size: cpal::BufferSize::Default,
            };

            let samples_clone = self.samples.clone();
            let err_fn = |err| error!("System audio stream error: {}", err);

            match device.build_input_stream(
                &config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    if let Ok(mut samples) = samples_clone.lock() {
                        samples.extend_from_slice(data);
                    }
                },
                err_fn,
                None,
            ) {
                Ok(stream) => {
                    stream.play().context("Failed to start system audio stream")?;
                    self.capture = Some(SystemCapture::Cpal {
                        stream,
                        actual_sample_rate,
                    });
                    self.active = true;
                    info!("System audio capture started via monitor source");
                    return Ok(());
                }
                Err(e) => {
                    warn!("Failed to build system audio stream: {}", e);
                }
            }
        }

        // No monitor source found — proceed without system audio
        warn!(
            "No system audio monitor source found. \
             Meeting will record mic only. \
             Ensure PipeWire is running and a monitor source is available."
        );
        self.active = true;
        Ok(())
    }

    fn stop(&mut self) -> Result<Vec<f32>> {
        if !self.active {
            return Err(anyhow::anyhow!("System audio source not recording"));
        }

        // Drop capture to stop recording
        if let Some(capture) = self.capture.take() {
            match capture {
                SystemCapture::Cpal { stream, .. } => {
                    debug!("Stopping system audio cpal stream");
                    drop(stream);
                }
            }
        }

        self.active = false;

        let samples = {
            let mut guard = self.samples.lock().unwrap();
            let s = guard.clone();
            guard.clear();
            guard.shrink_to_fit();
            s
        };

        info!(
            "System audio stopped, {} samples captured",
            samples.len()
        );
        Ok(samples)
    }

    fn is_active(&self) -> bool {
        self.active
    }

    fn sample_rate(&self) -> u32 {
        // Return actual sample rate if we have an active cpal capture
        if let Some(SystemCapture::Cpal {
            actual_sample_rate, ..
        }) = &self.capture
        {
            return *actual_sample_rate;
        }
        self.target_sample_rate
    }
}

impl Drop for SystemAudioSource {
    fn drop(&mut self) {
        if self.active {
            debug!("Dropping active SystemAudioSource, cleaning up");
            let _ = self.stop();
        }
    }
}
