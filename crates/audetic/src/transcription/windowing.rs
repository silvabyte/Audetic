//! Window long audio under an encoder's max sequence length.
//!
//! Some on-device encoders precompute positional encodings for a bounded
//! sequence length and crash when fed a longer one. The Parakeet ONNX export is
//! one: past ~5000 encoder frames (≈ 6.7 min) it fails inside self-attention
//! with a broadcast error ("Attempting to broadcast an axis by a dimension
//! other than 1"). This module splits the decoded samples into windows below a
//! caller-supplied limit, snapping each cut to a silence so no word is split,
//! runs a caller-supplied per-window transcribe, then merges the results with
//! timestamps corrected back to absolute time.
//!
//! It is a deep module: the whole interface is [`transcribe_windowed`] — the
//! caller supplies the samples, one window-length knob, and a closure that
//! transcribes a single window. Boundary math, silence-snapping, timestamp
//! offsetting, and text/segment merging stay hidden inside.
//!
//! Short input (≤ one window) calls the closure exactly once and returns its
//! output untouched, so dictation clips and short meetings are unaffected.

use anyhow::{Context, Result};

use super::providers::TranscriptionOutput;
use audetic_core::jobs_client::Segment;

/// All local audio is decoded to 16 kHz mono (`load_audio_16k_mono`); the
/// windowing math relies on that invariant.
const SAMPLE_RATE: usize = 16_000;

/// How far back from a hard window boundary to hunt for a silence to cut on. A
/// pause inside this band becomes the cut point; otherwise we hard-cut at the
/// boundary. 15 s is enough to catch a sentence gap without shrinking windows
/// meaningfully (the window itself is minutes long).
const SNAP_SEARCH_SECS: f32 = 15.0;

/// Width of the RMS scan window used to locate the quietest point in the search
/// band (~100 ms at 16 kHz).
const SNAP_RMS_WIN: usize = 1_600;

/// Transcribe arbitrarily long 16 kHz mono audio by windowing it under
/// `max_window_secs`, then merging the per-window results into one
/// [`TranscriptionOutput`] with absolute timestamps.
///
/// `max_window_secs` is the *caller's* limit (e.g. Parakeet's encoder extent),
/// keeping this module engine-agnostic. The closure transcribes one contiguous
/// window of samples and returns its text + segments with timestamps relative
/// to the start of that window; this function offsets them to absolute time.
///
/// Errors from the closure abort the whole call (fail-fast), wrapped with the
/// failing window's index and time range so the cause is legible rather than a
/// cryptic engine error.
pub fn transcribe_windowed(
    samples: &[f32],
    max_window_secs: f32,
    mut transcribe_window: impl FnMut(&[f32]) -> Result<TranscriptionOutput>,
) -> Result<TranscriptionOutput> {
    let max_window = (max_window_secs * SAMPLE_RATE as f32) as usize;

    // Short input: one pass, exact passthrough — no merge overhead and no
    // behaviour change for dictation / short meetings.
    if max_window == 0 || samples.len() <= max_window {
        return transcribe_window(samples);
    }

    // Never search more than half a window back, so a cut can't collapse the
    // window toward zero length even when the whole band is uniformly quiet.
    let snap_band = ((SNAP_SEARCH_SECS * SAMPLE_RATE as f32) as usize).min(max_window / 2);
    let mut merged = TranscriptionOutput {
        text: String::new(),
        segments: Vec::new(),
    };
    let mut start = 0usize;
    let mut window_index = 0usize;

    while start < samples.len() {
        let hard_end = (start + max_window).min(samples.len());
        // The final window ends at EOF; interior windows snap to a nearby pause.
        let end = if hard_end == samples.len() {
            hard_end
        } else {
            snap_to_silence(samples, hard_end, snap_band).max(start + 1)
        };

        let window = &samples[start..end];
        let offset_secs = start as f64 / SAMPLE_RATE as f64;
        let end_secs = end as f64 / SAMPLE_RATE as f64;

        let out = transcribe_window(window).with_context(|| {
            format!(
                "transcription failed for window {window_index} ({offset_secs:.1}s–{end_secs:.1}s)"
            )
        })?;

        let text = out.text.trim();
        if !text.is_empty() {
            if !merged.text.is_empty() {
                merged.text.push(' ');
            }
            merged.text.push_str(text);
        }
        merged
            .segments
            .extend(out.segments.into_iter().map(|s| Segment {
                start: s.start + offset_secs,
                end: s.end + offset_secs,
                text: s.text,
            }));

        start = end;
        window_index += 1;
    }

    Ok(merged)
}

/// Find the quietest sample-aligned point within `[hard_end - band, hard_end)`
/// by scanning short RMS windows, returning the start of the quietest one as
/// the cut. Falls back to `hard_end` when the band is empty. Cutting on a pause
/// keeps a window from splitting mid-word.
///
/// Ties break toward `hard_end` (a later window wins on equal energy), so a
/// stretch of uniform silence cuts at its trailing edge — just before speech
/// resumes — which also keeps the window as long as possible.
fn snap_to_silence(samples: &[f32], hard_end: usize, band: usize) -> usize {
    let search_start = hard_end.saturating_sub(band);
    if search_start >= hard_end {
        return hard_end;
    }

    let mut best_cut = hard_end;
    let mut best_energy = f32::MAX;
    let mut i = search_start;
    while i < hard_end {
        let win_end = (i + SNAP_RMS_WIN).min(hard_end);
        let slice = &samples[i..win_end];
        let energy = slice.iter().map(|s| s * s).sum::<f32>() / slice.len() as f32;
        if energy <= best_energy {
            best_energy = energy;
            best_cut = i;
        }
        i += SNAP_RMS_WIN;
    }
    best_cut
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(text: &str, segs: &[(f64, f64, &str)]) -> TranscriptionOutput {
        TranscriptionOutput {
            text: text.to_string(),
            segments: segs
                .iter()
                .map(|(s, e, t)| Segment {
                    start: *s,
                    end: *e,
                    text: t.to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn short_audio_calls_closure_once_and_passes_through() {
        let samples = vec![0.0f32; SAMPLE_RATE]; // 1 s, well under the window
        let mut calls = 0;
        let result = transcribe_windowed(&samples, 480.0, |w| {
            calls += 1;
            assert_eq!(w.len(), samples.len(), "whole buffer passed in one window");
            Ok(out("hello world", &[(0.0, 1.0, "hello world")]))
        })
        .unwrap();

        assert_eq!(calls, 1);
        assert_eq!(result.text, "hello world");
        assert_eq!(result.segments.len(), 1);
        assert_eq!(result.segments[0].start, 0.0);
    }

    #[test]
    fn long_audio_splits_and_offsets_segments() {
        // 25 s of audio with a 1 s window forces multiple windows. Silence
        // throughout means snapping just hard-cuts at the boundary.
        let samples = vec![0.0f32; 25 * SAMPLE_RATE];
        let mut starts = Vec::new();
        let result = transcribe_windowed(&samples, 1.0, |w| {
            // Each window reports one segment at its own t=0.0..0.5.
            starts.push(w.len());
            Ok(out("chunk", &[(0.0, 0.5, "chunk")]))
        })
        .unwrap();

        assert!(result.segments.len() > 1, "should produce several windows");
        // Absolute timestamps must be monotonically increasing across windows.
        let mut prev = -1.0;
        for seg in &result.segments {
            assert!(
                seg.start > prev,
                "segment starts climb: {} > {}",
                seg.start,
                prev
            );
            prev = seg.start;
        }
        // Text from every window is joined with single spaces.
        assert!(result.text.starts_with("chunk chunk"));
    }

    #[test]
    fn snaps_cut_to_the_quietest_point() {
        // 2 s of loud audio with one grid-aligned silent trough; the cut should
        // land exactly on it (a unique energy minimum).
        let total = 2 * SAMPLE_RATE;
        let mut samples = vec![0.5f32; total];
        let trough = 19 * SNAP_RMS_WIN; // grid-aligned, inside the band
        for s in &mut samples[trough..trough + SNAP_RMS_WIN] {
            *s = 0.0;
        }
        let cut = snap_to_silence(&samples, total, total);
        assert_eq!(cut, trough, "cut should land on the silent trough");
    }

    #[test]
    fn closure_error_is_wrapped_with_window_context() {
        let samples = vec![0.0f32; 5 * SAMPLE_RATE];
        let result = transcribe_windowed(&samples, 1.0, |_w| anyhow::bail!("engine boom"));
        let err = match result {
            Ok(_) => panic!("expected the closure error to propagate"),
            Err(e) => e,
        };
        let msg = format!("{err:#}");
        assert!(msg.contains("window 0"), "names the failing window: {msg}");
        assert!(msg.contains("engine boom"), "preserves the cause: {msg}");
    }
}
