//! System audio capture on macOS via cpal's loopback device.
//!
//! cpal 0.17+ on macOS 14.6+ exposes an audio tap on the default output
//! device as an input stream. We:
//! 1. Open `default_output_device()` and grab its `default_output_config()`
//!    (typically 48 kHz stereo f32 — that's the device's native rate, not
//!    our target).
//! 2. Build an input stream with `build_input_stream` on that device. The
//!    callback receives interleaved samples; we mix channels to mono and
//!    push them into a shared buffer at the *native* rate.
//! 3. On stop, drain the buffer and resample to `target_sample_rate` with
//!    rubato.
//!
//! macOS first prompts for Screen Recording permission when the stream is
//! built. There is no "denied" callback — denied looks like a stream of
//! silence. We warn the user post-hoc if the captured buffer is all-zero.

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SupportedStreamConfig};
use rubato::{FftFixedIn, Resampler};
use std::sync::{Arc, Mutex};
use tracing::{debug, error, info, warn};

use crate::audio::audio_source::AudioSource;

/// rubato FFT chunk size. 1024 frames @ 48 kHz ≈ 21 ms — small enough to keep
/// the final partial-chunk padding cheap, large enough that the FFT overhead
/// per chunk is negligible compared to the recording length.
const RESAMPLE_CHUNK_FRAMES: usize = 1024;

pub struct SystemAudioSource {
    target_sample_rate: u32,
    stream: Option<cpal::Stream>,
    /// Mono samples at the *device's* native rate, accumulated by the cpal
    /// callback. Resampled to `target_sample_rate` on `stop()`.
    native_samples: Arc<Mutex<Vec<f32>>>,
    native_sample_rate: Option<u32>,
    active: bool,
}

impl SystemAudioSource {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            target_sample_rate: sample_rate,
            stream: None,
            native_samples: Arc::new(Mutex::new(Vec::new())),
            native_sample_rate: None,
            active: false,
        }
    }

    /// Build the cpal loopback stream. Separated so `start()` can recover
    /// gracefully when this fails (no output device, macOS < 14.6, or the
    /// audio-tap framework rejects the request).
    fn build_stream(&mut self) -> Result<(cpal::Stream, SupportedStreamConfig)> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| anyhow!("No default output device — cannot capture system audio"))?;

        let device_name = device
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| "<unknown>".to_string());

        let config = device
            .default_output_config()
            .with_context(|| format!("Failed to get default output config for {device_name}"))?;

        info!(
            "System audio loopback on {} ({} ch, {} Hz, {:?})",
            device_name,
            config.channels(),
            config.sample_rate(),
            config.sample_format()
        );

        // CoreAudio output streams are virtually always f32. Bail loudly
        // rather than silently delivering garbage if that ever changes.
        if config.sample_format() != SampleFormat::F32 {
            return Err(anyhow!(
                "Default output device uses {:?} samples — only f32 is supported for loopback",
                config.sample_format()
            ));
        }

        let channels = config.channels() as usize;
        let samples = self.native_samples.clone();
        let stream_config: cpal::StreamConfig = config.clone().into();

        let stream = device
            .build_input_stream(
                &stream_config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    push_mono(data, channels, &samples);
                },
                |err| error!("System audio stream error: {err}"),
                None,
            )
            .context("Failed to build cpal input stream on default output device")?;

        Ok((stream, config))
    }
}

/// Mix interleaved multi-channel input down to mono and append to `dst`.
/// For mono input this is a straight extend; for stereo+ each frame is
/// averaged across channels.
fn push_mono(data: &[f32], channels: usize, dst: &Arc<Mutex<Vec<f32>>>) {
    if channels == 0 {
        return;
    }
    let Ok(mut buf) = dst.lock() else { return };
    if channels == 1 {
        buf.extend_from_slice(data);
        return;
    }
    let inv = 1.0 / channels as f32;
    buf.reserve(data.len() / channels);
    for frame in data.chunks_exact(channels) {
        let sum: f32 = frame.iter().sum();
        buf.push(sum * inv);
    }
}

/// Resample a mono buffer from `from_rate` Hz to `to_rate` Hz using rubato's
/// FFT-based fixed-input resampler. Pads the final partial chunk with silence.
fn resample_to_target(input: &[f32], from_rate: u32, to_rate: u32) -> Result<Vec<f32>> {
    if from_rate == to_rate || input.is_empty() {
        return Ok(input.to_vec());
    }

    let mut resampler = FftFixedIn::<f32>::new(
        from_rate as usize,
        to_rate as usize,
        RESAMPLE_CHUNK_FRAMES,
        1, // sub-chunks per chunk
        1, // channels
    )
    .context("Failed to construct rubato resampler")?;

    let mut output: Vec<f32> =
        Vec::with_capacity(input.len() * to_rate as usize / from_rate as usize);
    let mut chunk_buf = vec![0.0_f32; RESAMPLE_CHUNK_FRAMES];
    let mut idx = 0;

    while idx < input.len() {
        let remaining = input.len() - idx;
        if remaining >= RESAMPLE_CHUNK_FRAMES {
            chunk_buf.copy_from_slice(&input[idx..idx + RESAMPLE_CHUNK_FRAMES]);
            idx += RESAMPLE_CHUNK_FRAMES;
        } else {
            chunk_buf[..remaining].copy_from_slice(&input[idx..]);
            for slot in &mut chunk_buf[remaining..] {
                *slot = 0.0;
            }
            idx = input.len();
        }

        let waves_in = vec![chunk_buf.clone()];
        let waves_out = resampler
            .process(&waves_in, None)
            .context("rubato resample failed")?;
        output.extend_from_slice(&waves_out[0]);
    }

    Ok(output)
}

impl AudioSource for SystemAudioSource {
    fn start(&mut self) -> Result<()> {
        if self.active {
            return Err(anyhow!("System audio source already recording"));
        }

        // Clear any leftovers from a previous run.
        {
            let mut buf = self.native_samples.lock().unwrap();
            buf.clear();
            buf.shrink_to_fit();
        }

        match self.build_stream() {
            Ok((stream, config)) => {
                stream
                    .play()
                    .context("Failed to start cpal loopback stream")?;
                self.native_sample_rate = Some(config.sample_rate());
                self.stream = Some(stream);
                self.active = true;
                info!("System audio capture started via cpal loopback");
                Ok(())
            }
            Err(err) => {
                // Mirror the Linux behavior when pw-cat is missing: degrade
                // to mic-only rather than blowing up the whole meeting.
                warn!(
                    "System audio unavailable — meeting will record mic only. \
                     Cause: {err:#}"
                );
                self.active = true;
                self.native_sample_rate = None;
                Ok(())
            }
        }
    }

    fn stop(&mut self) -> Result<Vec<f32>> {
        if !self.active {
            return Err(anyhow!("System audio source not recording"));
        }

        if let Some(stream) = self.stream.take() {
            debug!("Stopping cpal loopback stream");
            drop(stream);
        }

        let native = {
            let mut guard = self.native_samples.lock().unwrap();
            let s = std::mem::take(&mut *guard);
            guard.shrink_to_fit();
            s
        };

        self.active = false;

        let Some(native_rate) = self.native_sample_rate.take() else {
            // Stream never started — degraded to mic-only. Nothing to return.
            return Ok(Vec::new());
        };

        // Detect a totally silent buffer — almost always means the user denied
        // Screen Recording (or hasn't granted it yet). Resampling silence is
        // a waste; just warn and return empty so downstream mixing skips it.
        if !native.is_empty() && native.iter().all(|s| *s == 0.0) {
            warn!(
                "Captured system audio was entirely silent ({} samples). \
                 If you expected audio, grant Screen Recording / System \
                 Audio Recording permission to audetic in System Settings \
                 → Privacy & Security.",
                native.len()
            );
            return Ok(Vec::new());
        }

        let resampled = resample_to_target(&native, native_rate, self.target_sample_rate)?;
        info!(
            "System audio stopped: {} native @ {} Hz → {} samples @ {} Hz",
            native.len(),
            native_rate,
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

impl Drop for SystemAudioSource {
    fn drop(&mut self) {
        if self.active {
            debug!("Dropping active SystemAudioSource, cleaning up");
            let _ = self.stop();
        }
    }
}
