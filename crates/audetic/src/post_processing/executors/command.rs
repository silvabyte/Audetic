//! Run a shell command with the event JSON envelope on stdin.
//!
//! Bounded by a per-job timeout; `kill_on_drop` ensures a stuck child
//! is reaped when we abandon the wait. Non-zero exits are not errors —
//! they're surfaced via the [`ExecutionOutcome`] for the caller to log
//! or display.

use anyhow::{Context, Result};
use async_trait::async_trait;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

use super::{ExecutionOutcome, Executor};

/// Cap the captured stream length so a runaway child can't pin
/// arbitrary memory through the test endpoint. Anything past this is
/// truncated with a marker.
const MAX_CAPTURE_BYTES: usize = 64 * 1024;

pub struct CommandExecutor {
    command: String,
    timeout: Duration,
}

impl CommandExecutor {
    pub fn new(command: String, timeout_seconds: u64) -> Self {
        Self {
            command,
            timeout: Duration::from_secs(timeout_seconds),
        }
    }
}

#[async_trait]
impl Executor for CommandExecutor {
    async fn execute(&self, payload: &serde_json::Value) -> Result<ExecutionOutcome> {
        let mut child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&self.command)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;

        let stdin = child.stdin.take();
        let stdout = child.stdout.take().context("command stdout pipe missing")?;
        let stderr = child.stderr.take().context("command stderr pipe missing")?;
        let bytes = serde_json::to_vec(payload)?;
        let interaction = async {
            let write = async {
                if let Some(mut stdin) = stdin {
                    // Commands may intentionally ignore stdin/close it early.
                    let _ = stdin.write_all(&bytes).await;
                }
                Ok::<(), std::io::Error>(())
            };
            let (_, stdout, stderr, status) = tokio::try_join!(
                write,
                capture_bounded(stdout),
                capture_bounded(stderr),
                child.wait()
            )?;
            Ok::<_, std::io::Error>((status, stdout, stderr))
        };

        // Include prompt delivery and concurrently drain output: a large audio
        // transcript or a child writing before reading cannot bypass timeout.
        match tokio::time::timeout(self.timeout, interaction).await {
            Ok(Ok((status, stdout, stderr))) => Ok(ExecutionOutcome {
                success: status.success(),
                exit_code: status.code(),
                stdout,
                stderr,
                timed_out: false,
            }),
            Ok(Err(e)) => Ok(ExecutionOutcome {
                success: false,
                exit_code: None,
                stdout: String::new(),
                stderr: format!("failed to wait for child: {e}"),
                timed_out: false,
            }),
            Err(_) => {
                // Await direct-child cleanup before reporting timeout. Process
                // trees spawned by arbitrary configured commands are not a sandbox.
                let _ = child.kill().await;
                Ok(ExecutionOutcome {
                    success: false,
                    exit_code: None,
                    stdout: String::new(),
                    stderr: format!(
                        "command timed out after {}s (process killed)",
                        self.timeout.as_secs()
                    ),
                    timed_out: true,
                })
            }
        }
    }
}

async fn capture_bounded(mut stream: impl AsyncRead + Unpin) -> std::io::Result<String> {
    let mut captured = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let read = stream.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let remaining = (MAX_CAPTURE_BYTES + 1).saturating_sub(captured.len());
        captured.extend_from_slice(&buffer[..read.min(remaining)]);
        // Continue draining after the limit so the child never blocks on its
        // output pipe, but retain only a bounded prefix in memory.
    }
    Ok(truncate_utf8(&captured))
}

fn truncate_utf8(bytes: &[u8]) -> String {
    if bytes.len() <= MAX_CAPTURE_BYTES {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut s = String::from_utf8_lossy(&bytes[..MAX_CAPTURE_BYTES]).into_owned();
    s.push_str("\n…[truncated]");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn cat_echoes_payload_on_stdout() {
        let exec = CommandExecutor::new("cat".to_string(), 5);
        let payload = json!({"event": "audio_note.completed", "data": {"note_id": 1}});
        let out = exec.execute(&payload).await.unwrap();
        assert!(out.success);
        assert_eq!(out.exit_code, Some(0));
        assert!(out.stdout.contains("audio_note.completed"));
        assert!(!out.timed_out);
    }

    #[tokio::test]
    async fn nonzero_exit_is_not_error_but_marks_failure() {
        let exec = CommandExecutor::new("exit 7".to_string(), 5);
        let out = exec.execute(&json!({})).await.unwrap();
        assert!(!out.success);
        assert_eq!(out.exit_code, Some(7));
    }

    #[tokio::test]
    async fn timeout_marks_timed_out() {
        let exec = CommandExecutor::new("sleep 5".to_string(), 1);
        let out = exec.execute(&json!({})).await.unwrap();
        assert!(!out.success);
        assert!(out.timed_out);
        assert!(out.stderr.contains("timed out"));
    }

    #[tokio::test]
    async fn large_payload_to_nonreading_command_still_times_out() {
        let exec = CommandExecutor::new("exec sleep 10".to_string(), 1);
        let payload = json!({"transcript_text":"x".repeat(1_000_000)});
        let out = tokio::time::timeout(Duration::from_secs(3), exec.execute(&payload))
            .await
            .expect("stdin delivery must be covered by the job timeout")
            .unwrap();
        assert!(out.timed_out);
        assert!(!out.success);
    }

    #[tokio::test]
    async fn captured_output_is_bounded_without_blocking_the_writer() {
        let exec = CommandExecutor::new("cat".to_string(), 5);
        let out = exec
            .execute(&json!({"transcript_text":"x".repeat(1_000_000)}))
            .await
            .unwrap();
        assert!(out.success);
        assert!(out.stdout.ends_with("…[truncated]"));
        assert!(out.stdout.len() < MAX_CAPTURE_BYTES + 100);
    }
}
