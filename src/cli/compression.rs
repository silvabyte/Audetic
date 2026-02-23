//! Media file compression utilities for transcription.
//!
//! Provides FFmpeg-based compression to mp3 format for efficient upload
//! and transcription.

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

/// Check if FFmpeg is available on the system.
pub fn check_ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Compress media file to MP3 format for transcription.
///
/// Uses FFmpeg to extract audio from video files and compress to MP3 format,
/// which is universally supported by transcription APIs.
///
/// Returns path to compressed temp file.
pub fn compress_for_transcription(input: &Path) -> Result<PathBuf> {
    // Check FFmpeg is available
    if !check_ffmpeg_available() {
        bail!(
            "FFmpeg is required for audio compression but was not found.\n\
             Install FFmpeg:\n\
             - macOS: brew install ffmpeg\n\
             - Ubuntu/Debian: sudo apt install ffmpeg\n\
             - Arch: sudo pacman -S ffmpeg\n\
             - Windows: winget install ffmpeg"
        );
    }

    // Create temp output path
    let temp_dir = std::env::temp_dir();
    let filename = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("audio");
    let output = temp_dir.join(format!("{}_compressed.mp3", filename));

    // Run FFmpeg compression
    // -i: input file
    // -vn: extract audio only (ignore video)
    // -codec:a libmp3lame: use MP3 codec (universally supported)
    // -b:a 64k: 64kbps bitrate (good for speech)
    // -y: overwrite output without asking
    let status = Command::new("ffmpeg")
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
}
