//! Post-meeting hook abstraction and shell command implementation.
//!
//! After a meeting transcription completes, an optional hook can run to
//! process the results (e.g., generate meeting minutes via AI, file in
//! knowledge base, etc.).

use anyhow::Result;
use async_trait::async_trait;
use std::path::PathBuf;
use std::time::Duration;
use tracing::{info, warn};

/// Environment variable names for meeting metadata passed to hooks.
pub mod hook_env {
    pub const MEETING_ID: &str = "AUDETIC_MEETING_ID";
    pub const MEETING_TITLE: &str = "AUDETIC_MEETING_TITLE";
    pub const AUDIO_PATH: &str = "AUDETIC_AUDIO_PATH";
    pub const TRANSCRIPT_PATH: &str = "AUDETIC_TRANSCRIPT_PATH";
    pub const DURATION_SECONDS: &str = "AUDETIC_DURATION_SECONDS";
}

/// Result of a completed meeting, passed to hooks for post-processing.
pub struct MeetingResult {
    pub meeting_id: i64,
    pub title: Option<String>,
    pub audio_path: PathBuf,
    pub transcript_path: PathBuf,
    pub transcript_text: String,
    pub duration_seconds: u64,
}

/// Post-meeting processing hook.
/// v1: shell command. Future: webhooks, workflow pipelines.
#[async_trait]
pub trait PostMeetingHook: Send + Sync {
    async fn execute(&self, result: &MeetingResult) -> Result<()>;
}

/// Executes a shell command with meeting data.
/// - Pipes transcript_text to stdin
/// - Sets environment variables for meeting metadata
/// - Kills process on timeout
/// - Non-zero exit code logs warning but does not fail
pub struct ShellCommandHook {
    command: String,
    timeout: Duration,
}

impl ShellCommandHook {
    pub fn new(command: String, timeout_seconds: u64) -> Self {
        Self {
            command,
            timeout: Duration::from_secs(timeout_seconds),
        }
    }
}

#[async_trait]
impl PostMeetingHook for ShellCommandHook {
    async fn execute(&self, result: &MeetingResult) -> Result<()> {
        info!(
            "Running post-meeting hook for meeting {}: {}",
            result.meeting_id, self.command
        );

        let mut child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&self.command)
            .env(hook_env::MEETING_ID, result.meeting_id.to_string())
            .env(
                hook_env::MEETING_TITLE,
                result.title.as_deref().unwrap_or(""),
            )
            .env(hook_env::AUDIO_PATH, result.audio_path.to_string_lossy().as_ref())
            .env(
                hook_env::TRANSCRIPT_PATH,
                result.transcript_path.to_string_lossy().as_ref(),
            )
            .env(hook_env::DURATION_SECONDS, result.duration_seconds.to_string())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;

        // Write transcript to stdin
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            let _ = stdin.write_all(result.transcript_text.as_bytes()).await;
            // Drop stdin to signal EOF
        }

        // Wait with timeout (kill_on_drop handles cleanup on timeout)
        match tokio::time::timeout(self.timeout, child.wait_with_output()).await {
            Ok(Ok(output)) => {
                if output.status.success() {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    if !stdout.is_empty() {
                        info!("Post-meeting hook stdout: {}", stdout.trim());
                    }
                    info!("Post-meeting hook completed successfully");
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    warn!(
                        "Post-meeting hook exited with status {}: {}",
                        output.status,
                        stderr.trim()
                    );
                }
            }
            Ok(Err(e)) => {
                warn!("Post-meeting hook failed to execute: {}", e);
            }
            Err(_) => {
                warn!(
                    "Post-meeting hook timed out after {}s (process will be killed)",
                    self.timeout.as_secs()
                );
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hook_env_constants() {
        assert_eq!(hook_env::MEETING_ID, "AUDETIC_MEETING_ID");
        assert_eq!(hook_env::MEETING_TITLE, "AUDETIC_MEETING_TITLE");
        assert_eq!(hook_env::AUDIO_PATH, "AUDETIC_AUDIO_PATH");
        assert_eq!(hook_env::TRANSCRIPT_PATH, "AUDETIC_TRANSCRIPT_PATH");
        assert_eq!(hook_env::DURATION_SECONDS, "AUDETIC_DURATION_SECONDS");
    }

    #[test]
    fn test_shell_command_hook_creation() {
        let hook = ShellCommandHook::new("echo hello".to_string(), 3600);
        assert_eq!(hook.command, "echo hello");
        assert_eq!(hook.timeout, Duration::from_secs(3600));
    }

    #[tokio::test]
    async fn test_shell_command_hook_success() {
        let hook = ShellCommandHook::new("cat".to_string(), 10);
        let result = MeetingResult {
            meeting_id: 1,
            title: Some("Test Meeting".to_string()),
            audio_path: PathBuf::from("/tmp/test.mp3"),
            transcript_path: PathBuf::from("/tmp/test.txt"),
            transcript_text: "Hello world".to_string(),
            duration_seconds: 60,
        };

        // `cat` should succeed — reads stdin and writes to stdout
        assert!(hook.execute(&result).await.is_ok());
    }

    #[tokio::test]
    async fn test_shell_command_hook_env_vars() {
        let hook = ShellCommandHook::new(
            "echo $AUDETIC_MEETING_ID $AUDETIC_MEETING_TITLE".to_string(),
            10,
        );
        let result = MeetingResult {
            meeting_id: 42,
            title: Some("Standup".to_string()),
            audio_path: PathBuf::from("/tmp/test.mp3"),
            transcript_path: PathBuf::from("/tmp/test.txt"),
            transcript_text: "test".to_string(),
            duration_seconds: 300,
        };

        // Should not fail even though it's just echo
        assert!(hook.execute(&result).await.is_ok());
    }

    #[tokio::test]
    async fn test_shell_command_hook_nonzero_exit() {
        let hook = ShellCommandHook::new("exit 1".to_string(), 10);
        let result = MeetingResult {
            meeting_id: 1,
            title: None,
            audio_path: PathBuf::from("/tmp/test.mp3"),
            transcript_path: PathBuf::from("/tmp/test.txt"),
            transcript_text: "test".to_string(),
            duration_seconds: 60,
        };

        // Non-zero exit should NOT cause an error — just logs a warning
        assert!(hook.execute(&result).await.is_ok());
    }
}
