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

use anyhow::{bail, Context, Result};
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
/// FFT-based fixed-input resampler. Internal delay and final-chunk padding are
/// trimmed so the returned clip has `input.len() * to_rate / from_rate` frames,
/// rounding a fractional target frame down.
pub fn resample_mono_f32(input: &[f32], from_rate: u32, to_rate: u32) -> Result<Vec<f32>> {
    if from_rate == 0 || to_rate == 0 {
        bail!("Sample rates must be greater than zero");
    }
    if from_rate == to_rate || input.is_empty() {
        return Ok(input.to_vec());
    }

    let expected_len = usize::try_from(input.len() as u128 * to_rate as u128 / from_rate as u128)
        .context("Resampled output length does not fit in memory")?;

    let mut resampler = FftFixedIn::<f32>::new(
        from_rate as usize,
        to_rate as usize,
        RESAMPLE_CHUNK_FRAMES,
        1, // sub-chunks per chunk
        1, // channels
    )
    .context("Failed to construct rubato resampler")?;

    let delay = resampler.output_delay();
    let required_len = delay
        .checked_add(expected_len)
        .context("Resampled output length overflowed")?;
    let mut output = Vec::with_capacity(required_len);
    let mut output_buffer = vec![vec![0.0_f32; resampler.output_frames_max()]];
    let mut remaining = input;

    while remaining.len() >= resampler.input_frames_next() {
        let input_buffer = [remaining];
        let (consumed, produced) = resampler
            .process_into_buffer(&input_buffer, &mut output_buffer, None)
            .context("rubato resample failed")?;
        output.extend_from_slice(&output_buffer[0][..produced]);
        remaining = &remaining[consumed..];
    }

    if !remaining.is_empty() {
        let input_buffer = [remaining];
        let (_, produced) = resampler
            .process_partial_into_buffer(Some(&input_buffer), &mut output_buffer, None)
            .context("rubato partial resample failed")?;
        output.extend_from_slice(&output_buffer[0][..produced]);
    }

    while output.len() < required_len {
        let (_, produced) = resampler
            .process_partial_into_buffer::<&[f32], Vec<f32>>(None, &mut output_buffer, None)
            .context("rubato resampler flush failed")?;
        if produced == 0 {
            bail!("rubato resampler stopped before producing the expected clip length");
        }
        output.extend_from_slice(&output_buffer[0][..produced]);
    }

    Ok(output[delay..required_len].to_vec())
}

#[cfg(test)]
mod tests {
    use super::resample_mono_f32;

    #[test]
    fn normalizes_segments_to_the_exact_target_duration() {
        for (input_len, from_rate, expected_len) in [
            (48_000, 48_000, 16_000),
            (44_100, 44_100, 16_000),
            (96_000, 96_000, 16_000),
            (441, 44_100, 160),
            (1_000, 44_100, 362),
        ] {
            let input = vec![0.25; input_len];
            let output = resample_mono_f32(&input, from_rate, 16_000).unwrap();

            assert_eq!(
                output.len(),
                expected_len,
                "normalizing {input_len} samples at {from_rate} Hz"
            );
        }
    }

    #[test]
    fn separately_normalized_segments_do_not_accumulate_padding() {
        let first = resample_mono_f32(&vec![0.25; 1_000], 44_100, 16_000).unwrap();
        let second = resample_mono_f32(&vec![-0.5; 1_000], 44_100, 16_000).unwrap();

        assert_eq!(first.len() + second.len(), 724);
    }
}
