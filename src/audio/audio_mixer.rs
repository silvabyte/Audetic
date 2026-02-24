//! Audio mixer for combining multiple audio sources into a single stream.
//!
//! Pure function (no state, no side effects) — easy to test.

/// Mix multiple sample vectors into a single mono output.
///
/// Handles:
/// - Different-length inputs by zero-padding the shorter ones
/// - Normalization to prevent clipping
/// - Resampling when sources have different sample rates
pub struct AudioMixer;

impl AudioMixer {
    /// Mix sample vectors (all at the same sample rate) into a single output.
    /// Zero-pads shorter inputs and normalizes to prevent clipping.
    pub fn mix(sources: &[Vec<f32>]) -> Vec<f32> {
        if sources.is_empty() {
            return Vec::new();
        }

        // Filter out empty sources
        let non_empty: Vec<&Vec<f32>> = sources.iter().filter(|s| !s.is_empty()).collect();

        if non_empty.is_empty() {
            return Vec::new();
        }

        if non_empty.len() == 1 {
            return non_empty[0].clone();
        }

        let max_len = non_empty.iter().map(|s| s.len()).max().unwrap_or(0);
        let num_sources = non_empty.len() as f32;

        let mut mixed = vec![0.0f32; max_len];

        for source in &non_empty {
            for (i, &sample) in source.iter().enumerate() {
                mixed[i] += sample;
            }
        }

        // Average the samples to prevent clipping
        for sample in &mut mixed {
            *sample /= num_sources;
        }

        // Normalize if any samples exceed [-1.0, 1.0]
        let max_abs = mixed
            .iter()
            .map(|s| s.abs())
            .fold(0.0f32, f32::max);

        if max_abs > 1.0 {
            for sample in &mut mixed {
                *sample /= max_abs;
            }
        }

        mixed
    }

    /// Resample audio from one sample rate to another using linear interpolation.
    /// Suitable for speech audio where perfect quality isn't critical.
    pub fn resample(samples: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
        if from_rate == to_rate || samples.is_empty() {
            return samples.to_vec();
        }

        let ratio = from_rate as f64 / to_rate as f64;
        let new_len = (samples.len() as f64 / ratio).ceil() as usize;
        let mut resampled = Vec::with_capacity(new_len);

        for i in 0..new_len {
            let src_pos = i as f64 * ratio;
            let src_idx = src_pos as usize;
            let frac = src_pos - src_idx as f64;

            let sample = if src_idx + 1 < samples.len() {
                // Linear interpolation
                samples[src_idx] as f64 * (1.0 - frac) + samples[src_idx + 1] as f64 * frac
            } else if src_idx < samples.len() {
                samples[src_idx] as f64
            } else {
                0.0
            };

            resampled.push(sample as f32);
        }

        resampled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mix_empty() {
        assert!(AudioMixer::mix(&[]).is_empty());
    }

    #[test]
    fn test_mix_single_source() {
        let source = vec![0.5, -0.3, 0.1];
        let result = AudioMixer::mix(&[source.clone()]);
        assert_eq!(result, source);
    }

    #[test]
    fn test_mix_two_equal_sources() {
        let a = vec![0.5, 0.5, 0.5];
        let b = vec![0.5, 0.5, 0.5];
        let result = AudioMixer::mix(&[a, b]);
        // Average: (0.5 + 0.5) / 2 = 0.5
        assert_eq!(result, vec![0.5, 0.5, 0.5]);
    }

    #[test]
    fn test_mix_different_lengths() {
        let a = vec![1.0, 1.0];
        let b = vec![1.0, 1.0, 1.0, 1.0];
        let result = AudioMixer::mix(&[a, b]);
        assert_eq!(result.len(), 4);
        // First two: (1.0+1.0)/2 = 1.0, last two: (0.0+1.0)/2 = 0.5
        assert_eq!(result[0], 1.0);
        assert_eq!(result[2], 0.5);
    }

    #[test]
    fn test_mix_with_empty_source() {
        let a = vec![0.5, 0.3];
        let b: Vec<f32> = vec![];
        let result = AudioMixer::mix(&[a.clone(), b]);
        assert_eq!(result, a);
    }

    #[test]
    fn test_mix_normalizes_clipping() {
        let a = vec![1.0, 1.0];
        let b = vec![1.0, 1.0];
        let result = AudioMixer::mix(&[a, b]);
        // Average is 1.0, no clipping needed
        for s in &result {
            assert!(*s <= 1.0);
            assert!(*s >= -1.0);
        }
    }

    #[test]
    fn test_resample_same_rate() {
        let samples = vec![1.0, 2.0, 3.0];
        let result = AudioMixer::resample(&samples, 16000, 16000);
        assert_eq!(result, samples);
    }

    #[test]
    fn test_resample_downsample() {
        // 48kHz to 16kHz (3:1 ratio)
        let samples: Vec<f32> = (0..48).map(|i| i as f32).collect();
        let result = AudioMixer::resample(&samples, 48000, 16000);
        assert_eq!(result.len(), 16);
    }

    #[test]
    fn test_resample_empty() {
        let result = AudioMixer::resample(&[], 48000, 16000);
        assert!(result.is_empty());
    }
}
