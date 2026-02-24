//! Integration tests for transcription compression
//!
//! ## Prerequisites
//! - FFmpeg must be installed
//! - A video file must be placed at `tests/fixtures/large_video.mp4`
//!
//! ## Running tests
//! ```bash
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

    let output = compress_for_transcription(input).unwrap();

    // Verify output exists and is smaller
    assert!(output.exists(), "Output file should exist");
    let output_size = get_file_size(&output).unwrap();
    assert!(
        output_size < input_size,
        "Compressed ({output_size}) should be smaller than input ({input_size})"
    );
    assert!(
        output_size < input_size,
        "Compressed file should be smaller than input, got {}MB",
        output_size / 1024 / 1024
    );

    // Verify it's an MP3 file
    assert_eq!(output.extension().unwrap(), "mp3");

    cleanup_temp_file(&output);
    assert!(!output.exists(), "Cleanup should remove temp file");
}

#[test]
fn test_is_already_compressed() {
    use audetic::cli::compression::is_already_compressed;
    assert!(is_already_compressed(Path::new("test.mp3")));
    assert!(is_already_compressed(Path::new("test.MP3")));
    assert!(is_already_compressed(Path::new("test.opus")));
    assert!(is_already_compressed(Path::new("test.OPUS")));
    assert!(!is_already_compressed(Path::new("test.wav")));
    assert!(!is_already_compressed(Path::new("test.mp4")));
    assert!(!is_already_compressed(Path::new("test")));
}

#[test]
fn test_compression_works_on_small_file() {
    if !ffmpeg_available() {
        eprintln!("Skipping: FFmpeg not installed");
        return;
    }

    let input = Path::new("tests/fixtures/test.wav");
    if !input.exists() {
        eprintln!("Skipping: Test fixture not found at tests/fixtures/test.wav");
        return;
    }

    use audetic::cli::compression::{cleanup_temp_file, compress_for_transcription};

    let output = compress_for_transcription(input).unwrap();

    assert!(output.exists(), "Output file should exist");
    assert_eq!(output.extension().unwrap(), "mp3");

    cleanup_temp_file(&output);
}

#[test]
fn test_ffmpeg_check_does_not_panic() {
    use audetic::cli::compression::check_ffmpeg_available;
    // Just verify it doesn't panic - result depends on system
    let _available = check_ffmpeg_available();
}
