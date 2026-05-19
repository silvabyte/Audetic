//! Shared helpers for downsampling and channel mixdown of cpal-captured audio.
//!
//! cpal 0.17 no longer offers transparent format conversion at stream-build
//! time; the device must accept the requested config or the call fails.
//! Mics and loopback sources on modern macOS overwhelmingly use 48 kHz
//! stereo f32, while the VTT pipeline wants 16 kHz mono. Both sources here
//! follow the same shape:
//!
//! 1. Build a cpal stream at the *device's* default config.
//! 2. Mix channels to mono inside the audio callback (cheap, fixed cost
//!    per frame).
//! 3. Accumulate samples at the native rate.
//! 4. On stop, resample the whole buffer to the target rate.
//!
//! Doing the resample on stop instead of in the callback keeps the audio
//! thread allocation-free and lets us batch the FFT work.

use anyhow::{Context, Result};
use rubato::{FftFixedIn, Resampler};
use std::sync::{Arc, Mutex};

/// rubato FFT chunk size. 1024 frames @ 48 kHz ≈ 21 ms — small enough to
/// keep the final partial-chunk padding cheap, large enough that the FFT
/// overhead per chunk is negligible compared to the recording length.
const RESAMPLE_CHUNK_FRAMES: usize = 1024;

/// Mix interleaved multi-channel f32 input down to mono and append to `dst`.
/// For mono input this is a straight extend; for stereo+ each frame is
/// averaged across channels. Designed to be cheap enough for the audio
/// callback — single mutex acquire, no allocation past `reserve()`.
pub fn push_mono_f32(data: &[f32], channels: usize, dst: &Arc<Mutex<Vec<f32>>>) {
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
/// FFT-based fixed-input resampler. The final partial chunk is padded with
/// silence so the input length doesn't need to be a multiple of the chunk
/// size.
pub fn resample_mono_f32(input: &[f32], from_rate: u32, to_rate: u32) -> Result<Vec<f32>> {
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
