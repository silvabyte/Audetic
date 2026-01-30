//! Integration tests for transcription compression
//!
//! Note: These tests require FFmpeg to be installed

use std::process::Command;

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
#[ignore] // Run with: cargo test -- --ignored
fn test_compression_produces_smaller_file() {
    if !ffmpeg_available() {
        eprintln!("Skipping test: FFmpeg not installed");
        return;
    }

    // Create a test WAV file or use a fixture
    // Compress it
    // Verify output is smaller and valid
}
