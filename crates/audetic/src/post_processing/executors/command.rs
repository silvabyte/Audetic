//! Run a shell command with the event JSON envelope on stdin.
//!
//! Bounded by a per-job timeout; `kill_on_drop` ensures a stuck child
//! is reaped when we abandon the wait. Non-zero exits are not errors —
//! they're surfaced via the [`ExecutionOutcome`] for the caller to log
//! or display.

use anyhow::Result;
use async_trait::async_trait;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

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

        if let Some(mut stdin) = child.stdin.take() {
            let bytes = serde_json::to_vec(payload)?;
            let _ = stdin.write_all(&bytes).await;
            // Drop stdin to signal EOF.
        }

        match tokio::time::timeout(self.timeout, child.wait_with_output()).await {
            Ok(Ok(output)) => Ok(ExecutionOutcome {
                success: output.status.success(),
                exit_code: output.status.code(),
                stdout: truncate_utf8(&output.stdout),
                stderr: truncate_utf8(&output.stderr),
                timed_out: false,
            }),
            Ok(Err(e)) => Ok(ExecutionOutcome {
                success: false,
                exit_code: None,
                stdout: String::new(),
                stderr: format!("failed to wait for child: {e}"),
                timed_out: false,
            }),
            Err(_) => Ok(ExecutionOutcome {
                success: false,
                exit_code: None,
                stdout: String::new(),
                stderr: format!(
                    "command timed out after {}s (process killed)",
                    self.timeout.as_secs()
                ),
                timed_out: true,
            }),
        }
    }
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
        let payload = json!({"event": "dictation.completed", "data": {"id": 1}});
        let out = exec.execute(&payload).await.unwrap();
        assert!(out.success);
        assert_eq!(out.exit_code, Some(0));
        assert!(out.stdout.contains("dictation.completed"));
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
}
