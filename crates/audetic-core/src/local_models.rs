//! Catalog of on-device transcription models.
//!
//! This is pure, daemon-independent data: the list of models the user can
//! download for local transcription, where each one lives on disk, and the
//! helpers both the daemon (which downloads + loads them) and the CLI (which
//! lists them) share. Downloads come straight from HuggingFace — Audetic has
//! no model CDN of its own.
//!
//! Two engine families are represented:
//!
//! - **Parakeet** (NVIDIA TDT, ONNX Runtime) — CPU-fast, the sensible default
//!   on machines without a strong GPU. A model is a *directory* of ONNX files
//!   (`encoder-model.int8.onnx`, `decoder_joint-model.int8.onnx`,
//!   `nemo128.onnx`, `vocab.txt`) — the exact layout `transcribe-rs` expects.
//! - **Whisper** (whisper.cpp, GGML) — a single `ggml-*.bin` file. Higher
//!   accuracy, slower on CPU.
//!
//! All files for a model live under `<data_dir>/models/<id>/`. For Parakeet the
//! engine loads the *directory*; for Whisper it loads the single `.bin` inside.

use std::path::{Path, PathBuf};

/// The transcription backend a model runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    /// NVIDIA Parakeet (TDT) via ONNX Runtime. Runs on CPU; auto-detects
    /// language (no explicit language selection).
    Parakeet,
    /// whisper.cpp (GGML). CPU by default; GPU when built with an accel
    /// feature. Honors an explicit language setting.
    Whisper,
}

impl Engine {
    pub fn as_str(self) -> &'static str {
        match self {
            Engine::Parakeet => "parakeet",
            Engine::Whisper => "whisper",
        }
    }
}

/// One downloadable file that makes up a model.
#[derive(Debug, Clone, Copy)]
pub struct ModelFile {
    /// File name on disk inside the model directory.
    pub name: &'static str,
    /// Fully-qualified download URL (HuggingFace `resolve/main`).
    pub url: &'static str,
    /// Expected size in bytes. Used as a sanity floor when checking whether a
    /// download is complete — we don't pin SHA256 because upstream HuggingFace
    /// repos can re-upload; a partial/truncated file is what we actually guard
    /// against, and the engine load is the final correctness gate.
    pub size_bytes: u64,
}

/// A model the user can download and select for local transcription.
#[derive(Debug, Clone, Copy)]
pub struct ModelInfo {
    /// Stable identifier — also the on-disk directory name and the value stored
    /// in `WhisperConfig.model`.
    pub id: &'static str,
    /// Human-readable name for menus.
    pub label: &'static str,
    /// One-line description (speed / language tradeoff).
    pub description: &'static str,
    /// Backend this model runs on.
    pub engine: Engine,
    /// Whether the model handles multiple languages.
    pub multilingual: bool,
    /// Whether an explicit language can be set. Parakeet auto-detects, so this
    /// is `false` for it; Whisper honors a language, so `true`.
    pub supports_language_selection: bool,
    /// Marks the recommended default in pickers.
    pub recommended: bool,
    /// Files that must all be present for the model to be usable.
    pub files: &'static [ModelFile],
}

impl ModelInfo {
    /// Total download size across all files.
    pub fn total_size_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size_bytes).sum()
    }
}

/// The model downloaded + selected when the user first enables local
/// transcription. Parakeet V3: CPU-fast, multilingual (25 European languages,
/// English included), ~671 MB.
pub const DEFAULT_MODEL_ID: &str = "parakeet-tdt-0.6b-v3";

// Shared Parakeet support files (identical across V2/V3 except encoder/decoder).
const PARAKEET_NEMO128: &str = "nemo128.onnx";
const PARAKEET_ENCODER: &str = "encoder-model.int8.onnx";
const PARAKEET_DECODER: &str = "decoder_joint-model.int8.onnx";
const PARAKEET_VOCAB: &str = "vocab.txt";

static PARAKEET_V3_FILES: &[ModelFile] = &[
    ModelFile {
        name: PARAKEET_NEMO128,
        url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/nemo128.onnx",
        size_bytes: 139_764,
    },
    ModelFile {
        name: PARAKEET_ENCODER,
        url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/encoder-model.int8.onnx",
        size_bytes: 652_183_999,
    },
    ModelFile {
        name: PARAKEET_DECODER,
        url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/decoder_joint-model.int8.onnx",
        size_bytes: 18_202_004,
    },
    ModelFile {
        name: PARAKEET_VOCAB,
        url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/vocab.txt",
        size_bytes: 9_000,
    },
];

static PARAKEET_V2_FILES: &[ModelFile] = &[
    ModelFile {
        name: PARAKEET_NEMO128,
        url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v2-onnx/resolve/main/nemo128.onnx",
        size_bytes: 139_764,
    },
    ModelFile {
        name: PARAKEET_ENCODER,
        url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v2-onnx/resolve/main/encoder-model.int8.onnx",
        size_bytes: 652_184_014,
    },
    ModelFile {
        name: PARAKEET_DECODER,
        url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v2-onnx/resolve/main/decoder_joint-model.int8.onnx",
        size_bytes: 8_998_286,
    },
    ModelFile {
        name: PARAKEET_VOCAB,
        url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v2-onnx/resolve/main/vocab.txt",
        size_bytes: 9_000,
    },
];

static WHISPER_LARGE_V3_FILES: &[ModelFile] = &[ModelFile {
    name: "ggml-large-v3.bin",
    url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3.bin",
    size_bytes: 3_095_033_483,
}];

static WHISPER_BASE_EN_FILES: &[ModelFile] = &[ModelFile {
    name: "ggml-base.en.bin",
    url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin",
    size_bytes: 147_964_211,
}];

static CATALOG: &[ModelInfo] = &[
    ModelInfo {
        id: "parakeet-tdt-0.6b-v3",
        label: "Parakeet V3 (multilingual)",
        description: "Fast on CPU. 25 European languages incl. English, auto-detected.",
        engine: Engine::Parakeet,
        multilingual: true,
        supports_language_selection: false,
        recommended: true,
        files: PARAKEET_V3_FILES,
    },
    ModelInfo {
        id: "parakeet-tdt-0.6b-v2",
        label: "Parakeet V2 (English)",
        description: "Fast on CPU. English only, best English accuracy of the two Parakeets.",
        engine: Engine::Parakeet,
        multilingual: false,
        supports_language_selection: false,
        recommended: false,
        files: PARAKEET_V2_FILES,
    },
    ModelInfo {
        id: "whisper-large-v3",
        label: "Whisper large-v3 (accuracy)",
        description: "Highest accuracy, multilingual. Slow on CPU — best with a GPU.",
        engine: Engine::Whisper,
        multilingual: true,
        supports_language_selection: true,
        recommended: false,
        files: WHISPER_LARGE_V3_FILES,
    },
    ModelInfo {
        id: "whisper-base.en",
        label: "Whisper base.en (English, light)",
        description: "Small and responsive on CPU. English only, lower accuracy.",
        engine: Engine::Whisper,
        multilingual: false,
        supports_language_selection: true,
        recommended: false,
        files: WHISPER_BASE_EN_FILES,
    },
];

/// All known models, in display order.
pub fn catalog() -> &'static [ModelInfo] {
    CATALOG
}

/// Look up a model by id.
pub fn find(id: &str) -> Option<&'static ModelInfo> {
    CATALOG.iter().find(|m| m.id == id)
}

/// Root directory holding every model's subdirectory: `<data_dir>/models`.
pub fn models_root(data_dir: &Path) -> PathBuf {
    data_dir.join("models")
}

/// Directory for one model's files: `<data_dir>/models/<id>`.
pub fn model_dir(data_dir: &Path, id: &str) -> PathBuf {
    models_root(data_dir).join(id)
}

/// Path the engine loads. Parakeet loads its directory; Whisper loads the
/// single `.bin` file inside the directory.
pub fn model_load_path(data_dir: &Path, model: &ModelInfo) -> PathBuf {
    let dir = model_dir(data_dir, model.id);
    match model.engine {
        Engine::Parakeet => dir,
        Engine::Whisper => dir.join(model.files[0].name),
    }
}

/// Whether every file for a model is present and at least 90% of its expected
/// size (guards against truncated / interrupted downloads). The engine load is
/// the final correctness gate; this is a cheap pre-check.
pub fn is_installed(data_dir: &Path, model: &ModelInfo) -> bool {
    let dir = model_dir(data_dir, model.id);
    model.files.iter().all(|f| {
        let path = dir.join(f.name);
        match std::fs::metadata(&path) {
            Ok(meta) => meta.len() >= (f.size_bytes as f64 * 0.9) as u64,
            Err(_) => false,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_model_is_in_catalog_and_recommended() {
        let model = find(DEFAULT_MODEL_ID).expect("default model must exist in catalog");
        assert!(model.recommended);
        assert_eq!(model.engine, Engine::Parakeet);
    }

    #[test]
    fn every_model_has_at_least_one_file_and_unique_id() {
        let mut seen = std::collections::HashSet::new();
        for model in catalog() {
            assert!(!model.files.is_empty(), "{} has no files", model.id);
            assert!(seen.insert(model.id), "duplicate model id {}", model.id);
            assert!(model.total_size_bytes() > 0);
        }
    }

    #[test]
    fn parakeet_loads_directory_whisper_loads_file() {
        let data = Path::new("/data");
        let v3 = find("parakeet-tdt-0.6b-v3").unwrap();
        assert_eq!(
            model_load_path(data, v3),
            data.join("models/parakeet-tdt-0.6b-v3")
        );

        let whisper = find("whisper-base.en").unwrap();
        assert_eq!(
            model_load_path(data, whisper),
            data.join("models/whisper-base.en/ggml-base.en.bin")
        );
    }
}
