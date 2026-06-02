//! Shared default-input-device acquisition for the dictation and meeting
//! capture paths.
//!
//! Pulled out of `AudioStreamManager` and `MicAudioSource` so the blocking
//! CoreAudio calls live in one place. On macOS `default_input_config()` opens
//! an audio unit, which gates on the Microphone TCC service — if the grant
//! isn't resolved yet the call blocks in `tccd`. That's why callers MUST invoke
//! this lazily (on first `start`), never at daemon boot: an eager call wedges
//! the whole process before the API server comes up.

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait};
use cpal::SampleFormat;
use tracing::info;

/// An opened default input device plus the native config we capture at.
///
/// cpal 0.17 no longer converts formats at stream-build time, so we capture at
/// the device's native config (e.g. 48 kHz stereo on a typical Mac) and
/// resample to the pipeline target rate on stop (see [`crate::audio::resample`]).
pub struct OpenInput {
    pub device: cpal::Device,
    pub config: cpal::StreamConfig,
    /// Native sample rate from the device config; the rate we resample *from*.
    pub native_sample_rate: u32,
    /// Channel count of the native stream; the callback mixes this many
    /// channels into mono.
    pub channels: usize,
}

/// Open the default input device and read its native config.
///
/// On macOS this touches an audio unit and blocks in `tccd` until the
/// Microphone TCC grant is resolved — call it lazily (first `start`), never at
/// boot. `label` is used only for the device log line so callers can tell the
/// dictation and meeting-mic sources apart.
pub fn open_default_input(label: &str) -> Result<OpenInput> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .context("No input device available")?;

    let supported = device
        .default_input_config()
        .context("Failed to read default input config for audio device")?;

    info!(
        "{label} using device: {} ({} ch, {} Hz, {:?})",
        device
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| "unknown".to_string()),
        supported.channels(),
        supported.sample_rate(),
        supported.sample_format()
    );

    // The capture callback is typed `&[f32]`, so the device must deliver f32.
    // Bail with a clear error rather than silently mis-decoding. Most cpal
    // hosts deliver f32 by default.
    if supported.sample_format() != SampleFormat::F32 {
        return Err(anyhow!(
            "Default input device uses {:?} samples — only f32 is supported",
            supported.sample_format()
        ));
    }

    let channels = supported.channels() as usize;
    let native_sample_rate = supported.sample_rate();
    let config: cpal::StreamConfig = supported.into();

    Ok(OpenInput {
        device,
        config,
        native_sample_rate,
        channels,
    })
}
