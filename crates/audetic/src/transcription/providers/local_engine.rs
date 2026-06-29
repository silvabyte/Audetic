//! In-process local transcription via `transcribe-rs`.
//!
//! Unlike the `whisper-cpp`/`openai-cli` providers — which shell out to a
//! binary the user installs themselves — this provider links the engine
//! directly and runs entirely on-device. It backs two engine families behind
//! one provider, selected by which model the user downloaded:
//!
//! - **Parakeet** (ONNX, CPU-fast) — the default, great on machines without a
//!   strong GPU.
//! - **Whisper** (whisper.cpp/GGML) — higher accuracy, slower on CPU.
//!
//! Models are managed by [`crate::transcription::models`] and described by
//! [`audetic_core::local_models`]. Loaded engines are cached process-wide so
//! the dictation and meeting pipelines share a single in-memory model rather
//! than each loading their own copy.

use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use tracing::{info, warn};

use transcribe_rs::{
    onnx::{
        parakeet::{ParakeetModel, ParakeetParams, TimestampGranularity},
        Quantization,
    },
    whisper_cpp::{WhisperEngine, WhisperInferenceParams},
    TranscriptionResult,
};

use super::{TranscriptionOutput, TranscriptionProvider};
use crate::normalizer::TranscriptionNormalizer;
use crate::transcription::windowing;
use audetic_core::jobs_client::Segment;
use audetic_core::local_models::{self, Engine, ModelInfo};

/// The exported Parakeet encoder precomputes its relative positional encoding
/// for at most ~5000 frames; a longer sequence crashes in self-attention with a
/// broadcast error ("axis ... 877 by 5877" — the operands always differ by
/// exactly 5000, i.e. `T` vs `T - pos_emb_max_len`). At 12.5 encoder fps
/// (100 fps mel ÷ 8x subsampling) 5000 frames ≈ 400 s, so window well under it:
/// 6 min ≈ 4500 frames leaves ~40 s of headroom for the encoder's prepended
/// silence and mel edges. See [`windowing`].
const PARAKEET_MAX_WINDOW_SECS: f32 = 360.0;

/// A loaded engine, ready to transcribe. Held behind a `Mutex` because the
/// underlying `transcribe-rs` engines take `&mut self`.
enum LoadedEngine {
    Parakeet(ParakeetModel),
    Whisper(WhisperEngine),
}

type EngineHandle = Arc<Mutex<LoadedEngine>>;

/// Process-wide cache of loaded engines, keyed by model id. Sharing one load
/// across the dictation + meeting pipelines avoids holding two ~600 MB copies.
fn engine_cache() -> &'static Mutex<HashMap<String, EngineHandle>> {
    static CACHE: OnceLock<Mutex<HashMap<String, EngineHandle>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub struct LocalEngineProvider {
    model: &'static ModelInfo,
    data_dir: PathBuf,
}

impl LocalEngineProvider {
    /// Resolve the configured model from the catalog. Does NOT load the engine
    /// — loading is deferred to the first `transcribe` call so daemon startup
    /// and provider validation stay cheap.
    pub fn new(model_id: &str) -> Result<Self> {
        let model = local_models::find(model_id).ok_or_else(|| {
            anyhow!(
                "Unknown local model '{model_id}'. Run `audetic models list` to see available models."
            )
        })?;
        let data_dir = audetic_core::global::data_dir()?;
        Ok(Self { model, data_dir })
    }

    /// Run inference off the async runtime, returning text + segments. Both
    /// `transcribe` and `transcribe_detailed` go through here.
    fn run<'a>(
        &'a self,
        audio_path: &'a Path,
        language: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<TranscriptionOutput>> + Send + 'a>> {
        let model = self.model;
        let data_dir = self.data_dir.clone();
        let audio_path = audio_path.to_path_buf();
        let language = language.to_string();

        Box::pin(async move {
            // Loading and inference are both blocking + CPU-heavy; keep them off
            // the async runtime.
            tokio::task::spawn_blocking(move || {
                transcribe_blocking(model, &data_dir, &audio_path, &language)
            })
            .await
            .context("local transcription task panicked")?
        })
    }
}

impl TranscriptionProvider for LocalEngineProvider {
    fn name(&self) -> &'static str {
        "local"
    }

    fn is_available(&self) -> bool {
        local_models::is_installed(&self.data_dir, self.model)
    }

    fn transcribe<'a>(
        &'a self,
        audio_path: &'a Path,
        language: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
        let fut = self.run(audio_path, language);
        Box::pin(async move { Ok(fut.await?.text) })
    }

    fn transcribe_detailed<'a>(
        &'a self,
        audio_path: &'a Path,
        language: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<TranscriptionOutput>> + Send + 'a>> {
        self.run(audio_path, language)
    }

    fn normalizer(&self) -> Result<Box<dyn TranscriptionNormalizer>> {
        // transcribe-rs already returns clean text (no inline timestamps), so a
        // trim is all that's needed.
        Ok(Box::new(PassthroughNormalizer))
    }
}

/// Synchronous transcription: load (or reuse) the engine, decode audio to
/// 16 kHz mono f32, run inference, return text + per-segment timestamps.
fn transcribe_blocking(
    model: &'static ModelInfo,
    data_dir: &Path,
    audio_path: &Path,
    language: &str,
) -> Result<TranscriptionOutput> {
    let samples = load_audio_16k_mono(audio_path)
        .with_context(|| format!("Failed to decode audio: {audio_path:?}"))?;

    let handle = get_or_load_engine(model, data_dir)?;
    let mut engine = handle
        .lock()
        .map_err(|_| anyhow!("local engine mutex poisoned"))?;

    let result = match &mut *engine {
        LoadedEngine::Parakeet(parakeet) => {
            // Segment granularity yields sentence-ish chunks — the right unit
            // for clickable transcript lines (token-level is too fine).
            let params = ParakeetParams {
                timestamp_granularity: Some(TimestampGranularity::Segment),
                ..Default::default()
            };
            // The encoder can't ingest the whole recording past ~6.7 min, so
            // feed it one bounded window at a time and let `windowing` merge the
            // results back into a single timeline.
            windowing::transcribe_windowed(&samples, PARAKEET_MAX_WINDOW_SECS, |window| {
                let result = parakeet
                    .transcribe_with(window, &params)
                    .map_err(|e| anyhow!("Parakeet transcription failed: {e}"))?;
                Ok(result_to_output(result))
            })?
        }
        LoadedEngine::Whisper(whisper) => {
            // "auto"/empty → let Whisper detect; otherwise honor the setting.
            let lang = match language.trim() {
                "" | "auto" => None,
                other => Some(other.to_string()),
            };
            let params = WhisperInferenceParams {
                language: lang,
                ..Default::default()
            };
            // whisper.cpp windows long audio internally, so no chunking here.
            let result = whisper
                .transcribe_with(&samples, &params)
                .map_err(|e| anyhow!("Whisper transcription failed: {e}"))?;
            result_to_output(result)
        }
    };

    info!(
        "Local transcription complete ({}): {} chars",
        model.id,
        result.text.len()
    );
    Ok(TranscriptionOutput {
        text: result.text.trim().to_string(),
        segments: result.segments,
    })
}

/// Map a transcribe-rs result (f32 seconds, optional segments) into the shared
/// [`TranscriptionOutput`], trimming and dropping empty segments. Timestamps
/// are window-relative here; [`windowing`] shifts them to absolute time.
fn result_to_output(result: TranscriptionResult) -> TranscriptionOutput {
    let segments = result
        .segments
        .unwrap_or_default()
        .into_iter()
        .map(|s| Segment {
            start: s.start as f64,
            end: s.end as f64,
            text: s.text.trim().to_string(),
        })
        .filter(|s| !s.text.is_empty())
        .collect();

    TranscriptionOutput {
        text: result.text,
        segments,
    }
}

/// Fetch a cached engine or load it. The cache lock is held across the (rare)
/// load so two concurrent first-calls don't both load the same model.
fn get_or_load_engine(model: &'static ModelInfo, data_dir: &Path) -> Result<EngineHandle> {
    let mut cache = engine_cache()
        .lock()
        .map_err(|_| anyhow!("local engine cache poisoned"))?;

    if let Some(handle) = cache.get(model.id) {
        return Ok(handle.clone());
    }

    if !local_models::is_installed(data_dir, model) {
        bail!(
            "Model '{}' is not downloaded. Run `audetic models download {}` first.",
            model.id,
            model.id
        );
    }

    let path = local_models::model_load_path(data_dir, model);
    info!("Loading local model '{}' from {:?}", model.id, path);

    let engine = match model.engine {
        Engine::Parakeet => LoadedEngine::Parakeet(
            ParakeetModel::load(&path, &Quantization::Int8)
                .map_err(|e| anyhow!("Failed to load Parakeet model '{}': {e}", model.id))?,
        ),
        Engine::Whisper => LoadedEngine::Whisper(
            WhisperEngine::load(&path)
                .map_err(|e| anyhow!("Failed to load Whisper model '{}': {e}", model.id))?,
        ),
    };

    let handle: EngineHandle = Arc::new(Mutex::new(engine));
    cache.insert(model.id.to_string(), handle.clone());
    Ok(handle)
}

/// Decode any input file to the 16 kHz mono f32 samples both engines expect.
///
/// Fast path: a 16 kHz mono WAV (what the dictation/meeting capture writes) is
/// read directly via `hound`. Anything else — a different rate/channel count,
/// or a compressed format like MP3/M4A — is transcoded with ffmpeg.
fn load_audio_16k_mono(path: &Path) -> Result<Vec<f32>> {
    if let Some(samples) = try_read_wav_16k_mono(path) {
        return Ok(samples);
    }
    transcode_via_ffmpeg(path)
}

/// Read a WAV only if it's already 16 kHz mono; otherwise `None` so the caller
/// falls back to ffmpeg (which resamples/downmixes).
fn try_read_wav_16k_mono(path: &Path) -> Option<Vec<f32>> {
    let reader = hound::WavReader::open(path).ok()?;
    let spec = reader.spec();
    if spec.channels != 1 || spec.sample_rate != 16_000 {
        return None;
    }

    let mut reader = reader;
    match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<Vec<_>, _>>().ok(),
        hound::SampleFormat::Int => {
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / scale))
                .collect::<Result<Vec<_>, _>>()
                .ok()
        }
    }
}

/// Transcode `path` to 16 kHz mono f32 PCM via ffmpeg, streaming raw samples on
/// stdout (no temp file).
fn transcode_via_ffmpeg(path: &Path) -> Result<Vec<f32>> {
    let ffmpeg = audetic_core::ffmpeg::resolve_ffmpeg_binary().ok_or_else(|| {
        anyhow!("ffmpeg is required to decode this audio format but was not found")
    })?;

    let output = Command::new(&ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(path)
        .args([
            "-ar",
            "16000",
            "-ac",
            "1",
            "-f",
            "f32le",
            "-acodec",
            "pcm_f32le",
            "pipe:1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .output()
        .context("Failed to run ffmpeg for audio decode")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!("ffmpeg decode failed: {stderr}");
        bail!("ffmpeg failed to decode audio: {stderr}");
    }

    let samples: Vec<f32> = output
        .stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();

    if samples.is_empty() {
        bail!("Decoded audio is empty");
    }
    Ok(samples)
}

struct PassthroughNormalizer;

impl TranscriptionNormalizer for PassthroughNormalizer {
    fn normalize(&self, raw_output: &str) -> String {
        raw_output.trim().to_string()
    }

    fn name(&self) -> &'static str {
        "PassthroughNormalizer"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_model_id_errors() {
        let result = LocalEngineProvider::new("does-not-exist");
        let err = match result {
            Ok(_) => panic!("expected an error for unknown model id"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("Unknown local model"));
    }

    #[test]
    fn passthrough_normalizer_trims() {
        let n = PassthroughNormalizer;
        assert_eq!(n.normalize("  hi there \n"), "hi there");
    }

    /// End-to-end check that the windowing path transcribes audio past the
    /// ~10 min encoder limit without the ONNX broadcast crash. Ignored by
    /// default — needs the Parakeet model installed and a real recording:
    ///   AUDETIC_E2E_AUDIO=/path/to/long.mp3 \
    ///     cargo test -p audetic parakeet_transcribes_long_audio -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "requires installed Parakeet model + AUDETIC_E2E_AUDIO"]
    async fn parakeet_transcribes_long_audio() {
        let path = std::env::var("AUDETIC_E2E_AUDIO")
            .expect("set AUDETIC_E2E_AUDIO to a recording longer than ~10 min");
        let provider = LocalEngineProvider::new("parakeet-tdt-0.6b-v3").unwrap();
        let out = provider
            .transcribe_detailed(std::path::Path::new(&path), "en")
            .await
            .expect("long-audio transcription should not crash the encoder");
        eprintln!(
            "transcribed {} chars across {} segments",
            out.text.len(),
            out.segments.len()
        );
        assert!(!out.text.trim().is_empty(), "expected non-empty transcript");
        assert!(out.segments.len() > 1, "expected multiple merged segments");
        // Segments must climb in absolute time across window boundaries.
        let mut prev = -1.0;
        for s in &out.segments {
            assert!(
                s.start >= prev,
                "segment starts increase: {} >= {}",
                s.start,
                prev
            );
            prev = s.start;
        }
    }
}
