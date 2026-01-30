//! Integration tests for the transcribe command
//!
//! These tests require a running transcription-manager server.
//! Skip with: cargo test --test transcribe_integration -- --ignored

use std::process::Command;

#[test]
#[ignore] // Requires running transcription server
fn test_transcribe_audio_file() {
    // This test requires:
    // 1. A running transcription-manager at localhost:3141
    // 2. A test audio file at tests/fixtures/test.wav

    let output = Command::new("cargo")
        .args([
            "run",
            "--",
            "transcribe",
            "tests/fixtures/test.wav",
            "--api-url",
            "http://localhost:3141/api/v1/jobs",
        ])
        .output()
        .expect("Failed to run command");

    assert!(output.status.success(), "Command failed: {:?}", output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.is_empty(), "No transcription output");
}

#[test]
#[ignore] // Requires running transcription server
fn test_transcribe_with_output_file() {
    let output_path = "/tmp/transcription_test_output.txt";

    let output = Command::new("cargo")
        .args([
            "run",
            "--",
            "transcribe",
            "tests/fixtures/test.wav",
            "-o",
            output_path,
            "--api-url",
            "http://localhost:3141/api/v1/jobs",
        ])
        .output()
        .expect("Failed to run command");

    assert!(output.status.success());
    assert!(std::path::Path::new(output_path).exists());
    std::fs::remove_file(output_path).ok();
}

#[test]
fn test_transcribe_missing_file() {
    let output = Command::new("cargo")
        .args(["run", "--", "transcribe", "nonexistent.wav"])
        .output()
        .expect("Failed to run command");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not found") || stderr.contains("No such file"),
        "Expected 'not found' error, got: {}",
        stderr
    );
}

#[test]
fn test_transcribe_unsupported_format() {
    // Create a temp file with unsupported extension
    let path = "/tmp/test_unsupported.xyz";
    std::fs::write(path, b"test").unwrap();

    let output = Command::new("cargo")
        .args(["run", "--", "transcribe", path])
        .output()
        .expect("Failed to run command");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Unsupported format"),
        "Expected 'Unsupported format' error, got: {}",
        stderr
    );

    std::fs::remove_file(path).ok();
}
