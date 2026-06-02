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

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, StreamTrait};
use std::sync::{Arc, Mutex};
use tracing::{debug, error, info};

use super::audio_source::AudioSource;
use super::input_device::{open_default_input, OpenInput};
use super::resample::{push_mono_f32, resample_mono_f32};

pub struct MicAudioSource {
    /// Default input device + native config, opened lazily on first `start()`.
    /// Acquiring it gates on the macOS mic TCC permission, so it's deferred out
    /// of construction (which runs at daemon boot) to avoid wedging the daemon
    /// in `tccd` (see [`crate::audio::input_device`]).
    input: Option<OpenInput>,
    /// Mono samples at the *native* rate, accumulated by the cpal callback.
    samples: Arc<Mutex<Vec<f32>>>,
    stream: Option<cpal::Stream>,
    active: bool,
    target_sample_rate: u32,
}

impl MicAudioSource {
    /// Create a new mic source backed by the default input device.
    ///
    /// Does **not** open the device — that's deferred to the first `start()`
    /// so constructing the meeting pipeline at boot never blocks on the mic
    /// TCC grant. Returns `Result` only to keep the call site stable;
    /// construction itself is infallible.
    ///
    /// # Arguments
    /// * `sample_rate` - Target sample rate after resampling (e.g. 16000 for
    ///   Whisper). The device may capture at a higher native rate; the
    ///   returned buffer from `stop()` is at this target rate.
    pub fn new(sample_rate: u32) -> Result<Self> {
        Ok(Self {
            input: None,
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

        // Open the device on first use. On macOS this is the call that gates on
        // the mic TCC permission; deferring it out of `new()` keeps daemon boot
        // from wedging in `tccd`.
        if self.input.is_none() {
            self.input = Some(open_default_input("Meeting mic source")?);
        }
        let input = self.input.as_ref().unwrap();

        let samples_clone = self.samples.clone();
        let channels = input.channels;
        let err_fn = |err| error!("Meeting mic stream error: {err}");

        let stream = input.device.build_input_stream(
            &input.config,
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

        // `start()` opened the device, so the native rate is known. Fall back
        // to the target rate (resample becomes a no-op) on the impossible path
        // where we were active without an opened device.
        let native_sample_rate = self
            .input
            .as_ref()
            .map(|i| i.native_sample_rate)
            .unwrap_or(self.target_sample_rate);

        let resampled = resample_mono_f32(&native, native_sample_rate, self.target_sample_rate)?;
        info!(
            "Meeting mic stopped: {} native @ {} Hz → {} samples @ {} Hz",
            native.len(),
            native_sample_rate,
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
