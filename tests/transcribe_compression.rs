//! Integration tests for transcription compression
//!
//! ## Prerequisites
//! - FFmpeg must be installed
//! - A large video file (>100MB) must be placed at `tests/fixtures/large_video.mp4`
//!
//! ## Running tests
//! ```bash
//! # Copy any video >100MB to the fixtures directory
//! cp /path/to/your/video.mp4 tests/fixtures/large_video.mp4
//! cargo test --test transcribe_compression
//! ```

use std::path::Path;
use std::process::Command;

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn test_compression_produces_smaller_file() {
    if !ffmpeg_available() {
        eprintln!("Skipping: FFmpeg not installed");
        return;
    }

    let input = Path::new("tests/fixtures/large_video.mp4");
    if !input.exists() {
        eprintln!("Skipping: Test fixture not found at tests/fixtures/large_video.mp4");
        return;
    }

    use audetic::cli::compression::{cleanup_temp_file, compress_for_transcription, get_file_size};

    let input_size = get_file_size(input).unwrap();
    assert!(
        input_size > 100 * 1024 * 1024,
        "Test file should be >100MB, got {}MB",
        input_size / 1024 / 1024
    );

    let output = compress_for_transcription(input).unwrap();

    // Verify output exists and is smaller
    assert!(output.exists(), "Output file should exist");
    let output_size = get_file_size(&output).unwrap();
    assert!(
        output_size < input_size,
        "Compressed ({output_size}) should be smaller than input ({input_size})"
    );
    assert!(
        output_size < 100 * 1024 * 1024,
        "Should be under 100MB limit, got {}MB",
        output_size / 1024 / 1024
    );

    // Verify it's an Opus file
    assert_eq!(output.extension().unwrap(), "opus");

    cleanup_temp_file(&output);
    assert!(!output.exists(), "Cleanup should remove temp file");
}

#[test]
fn test_exceeds_size_limit_with_large_file() {
    let input = Path::new("tests/fixtures/large_video.mp4");
    if !input.exists() {
        eprintln!("Skipping: Test fixture not found");
        return;
    }

    use audetic::cli::compression::exceeds_size_limit;
    assert!(
        exceeds_size_limit(input).unwrap(),
        "Large video should exceed size limit"
    );
}

#[test]
fn test_small_file_does_not_exceed_limit() {
    let input = Path::new("tests/fixtures/test.wav");
    if !input.exists() {
        eprintln!("Skipping: Test fixture not found");
        return;
    }

    use audetic::cli::compression::exceeds_size_limit;
    assert!(
        !exceeds_size_limit(input).unwrap(),
        "Small file should not exceed limit"
    );
}

#[test]
fn test_ffmpeg_check_does_not_panic() {
    use audetic::cli::compression::check_ffmpeg_available;
    // Just verify it doesn't panic - result depends on system
    let _available = check_ffmpeg_available();
}
