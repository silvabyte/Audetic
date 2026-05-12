//! Media file compression utilities for transcription.
//!
//! Provides FFmpeg-based compression to mp3 format for efficient upload
//! and transcription.

use crate::system::ffmpeg::resolve_ffmpeg_binary;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Check if a file is already in a compressed audio format suitable for upload.
///
/// Files already in a compressed audio format (mp3, opus) are sent as-is.
pub fn is_already_compressed(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("mp3") || e.eq_ignore_ascii_case("opus"))
        .unwrap_or(false)
}

/// Get file size in bytes.
pub fn get_file_size(path: &Path) -> Result<u64> {
    let metadata = std::fs::metadata(path).context("Failed to read file metadata")?;
    Ok(metadata.len())
}

/// Check if FFmpeg is available — either as the app-local sidecar binary in
/// the daemon's exe dir or on the system PATH. See `system::ffmpeg` for the
/// resolution order.
pub fn check_ffmpeg_available() -> bool {
    crate::system::ffmpeg::check_available()
}

/// Compress media file to MP3 format for transcription.
///
/// Uses FFmpeg to extract audio from video files and compress to MP3 format,
/// which is universally supported by transcription APIs.
///
/// Returns path to compressed temp file.
pub fn compress_for_transcription(input: &Path) -> Result<PathBuf> {
    // Resolve which ffmpeg to invoke — app-local sidecar wins over PATH so a
    // daemon-managed install is deterministic. The "FFmpeg is required..."
    // wording below is load-bearing: the renderer pattern-matches `/ffmpeg/i`
    // on meeting errors to route the user to the onboarding card.
    let ffmpeg = match resolve_ffmpeg_binary() {
        Some(path) => path,
        None => bail!(
            "FFmpeg is required for audio compression but was not found.\n\
             Install FFmpeg:\n\
             - macOS: brew install ffmpeg\n\
             - Ubuntu/Debian: sudo apt install ffmpeg\n\
             - Arch: sudo pacman -S ffmpeg\n\
             - Windows: winget install ffmpeg"
        ),
    };

    // Create temp output path. The random component keeps concurrent
    // compressions of same-named inputs (e.g. parallel `audetic transcribe`
    // calls, or parallel test threads) from writing to the same file — which
    // would make ffmpeg read a half-written input/output and fail.
    let temp_dir = std::env::temp_dir();
    let filename = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("audio");
    let output = temp_dir.join(format!(
        "{filename}-{}-compressed.mp3",
        uuid::Uuid::new_v4().simple()
    ));

    // Run FFmpeg compression
    // -i: input file
    // -vn: extract audio only (ignore video)
    // -codec:a libmp3lame: use MP3 codec (universally supported)
    // -b:a 64k: 64kbps bitrate (good for speech)
    // -y: overwrite output without asking
    let status = Command::new(&ffmpeg)
        .args(["-i", input.to_str().unwrap()])
        .args(["-vn"])
        .args(["-codec:a", "libmp3lame"])
        .args(["-b:a", "64k"])
        .args(["-y"])
        .arg(&output)
        .output()
        .context("Failed to run FFmpeg")?;

    if !status.status.success() {
        let stderr = String::from_utf8_lossy(&status.stderr);
        bail!("FFmpeg compression failed: {}", stderr);
    }

    // Verify the output file exists and is smaller
    if !output.exists() {
        bail!("FFmpeg did not produce output file");
    }

    Ok(output)
}

/// Remove temporary compressed file.
pub fn cleanup_temp_file(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Prepare a media file for upload to the transcription API.
///
/// Returns `(upload_path, temp_to_cleanup)`:
/// - If the input is already in a compressed audio format (mp3/opus) or
///   `skip_compression` is true, returns `(path, None)` and no temp file is
///   created.
/// - Otherwise compresses to mp3 in the system temp directory and returns
///   `(temp_path, Some(temp_path))` so the caller can delete the temp file
///   after upload.
///
/// On compression failure, returns the underlying error. Callers should NOT
/// fall back to uploading the uncompressed input — for long meetings or video
/// files this will exceed the API size limit. Surface the error instead.
pub fn prepare_for_upload(
    path: &Path,
    skip_compression: bool,
) -> Result<(PathBuf, Option<PathBuf>)> {
    if is_already_compressed(path) || skip_compression {
        return Ok((path.to_path_buf(), None));
    }

    let compressed = compress_for_transcription(path)?;
    Ok((compressed.clone(), Some(compressed)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_check_ffmpeg_available() {
        // This test documents behavior - will pass if FFmpeg installed
        let available = check_ffmpeg_available();
        // Don't assert - just ensure it doesn't panic
        println!("FFmpeg available: {}", available);
    }

    #[test]
    fn test_is_already_compressed() {
        assert!(is_already_compressed(Path::new("test.mp3")));
        assert!(is_already_compressed(Path::new("test.MP3")));
        assert!(is_already_compressed(Path::new("test.opus")));
        assert!(is_already_compressed(Path::new("test.OPUS")));
        assert!(!is_already_compressed(Path::new("test.wav")));
        assert!(!is_already_compressed(Path::new("test.mp4")));
        assert!(!is_already_compressed(Path::new("test")));
    }

    #[test]
    fn test_get_file_size() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"12345").unwrap();
        assert_eq!(get_file_size(file.path()).unwrap(), 5);
    }

    #[test]
    fn test_prepare_for_upload_already_compressed() {
        let path = PathBuf::from("/tmp/test_prepare_already_compressed.mp3");
        std::fs::write(&path, b"fake mp3").unwrap();

        let (upload_path, temp) = prepare_for_upload(&path, false).unwrap();
        assert_eq!(upload_path, path);
        assert!(temp.is_none());

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn test_prepare_for_upload_skip_flag() {
        let path = PathBuf::from("/tmp/test_prepare_skip_flag.wav");
        std::fs::write(&path, b"fake wav").unwrap();

        let (upload_path, temp) = prepare_for_upload(&path, true).unwrap();
        assert_eq!(upload_path, path);
        assert!(temp.is_none());

        std::fs::remove_file(&path).unwrap();
    }
}
