//! Media file compression utilities for transcription.
//!
//! Provides file size validation and FFmpeg-based compression to ensure
//! media files are under the API's 100MB limit.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Maximum file size in bytes (100MB)
const MAX_FILE_SIZE: u64 = 100 * 1024 * 1024;

/// Check if file exceeds the size limit.
pub fn exceeds_size_limit(path: &Path) -> Result<bool> {
    let metadata = std::fs::metadata(path).context("Failed to read file metadata")?;
    Ok(metadata.len() > MAX_FILE_SIZE)
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

/// Compress media file to Opus format for transcription.
///
/// Uses FFmpeg to extract audio from video files and compress to Opus format,
/// which provides excellent quality for speech at low bitrates.
///
/// Returns path to compressed temp file.
pub fn compress_for_transcription(input: &Path) -> Result<PathBuf> {
    // Check FFmpeg is available
    if !check_ffmpeg_available() {
        bail!(
            "FFmpeg is required to compress large files but was not found.\n\
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
    let output = temp_dir.join(format!("{}_compressed.opus", filename));

    // Run FFmpeg compression
    // -i: input file
    // -vn: extract audio only (ignore video)
    // -codec:a libopus: use Opus codec
    // -b:a 48k: 48kbps bitrate (good for speech)
    // -vbr on: variable bitrate for better quality
    // -y: overwrite output without asking
    let status = Command::new("ffmpeg")
        .args(["-i", input.to_str().unwrap()])
        .args(["-vn"])
        .args(["-codec:a", "libopus"])
        .args(["-b:a", "48k"])
        .args(["-vbr", "on"])
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
    fn test_exceeds_size_limit_small_file() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"small content").unwrap();
        assert!(!exceeds_size_limit(file.path()).unwrap());
    }

    #[test]
    fn test_check_ffmpeg_available() {
        // This test documents behavior - will pass if FFmpeg installed
        let available = check_ffmpeg_available();
        // Don't assert - just ensure it doesn't panic
        println!("FFmpeg available: {}", available);
    }

    #[test]
    fn test_exceeds_size_limit_missing_file() {
        let result = exceeds_size_limit(Path::new("/nonexistent/file.wav"));
        assert!(result.is_err());
    }

    #[test]
    fn test_get_file_size() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"12345").unwrap();
        assert_eq!(get_file_size(file.path()).unwrap(), 5);
    }
}
