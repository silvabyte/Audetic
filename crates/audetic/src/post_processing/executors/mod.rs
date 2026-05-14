//! Executor strategies for each [`Action`](super::Action) variant.

pub mod command;

use anyhow::Result;
use async_trait::async_trait;

use super::action::Action;

/// Captured output and exit status from a single execution. Used by the
/// `test` endpoint to surface the dry-run result back to the UI.
#[derive(Debug, Clone)]
pub struct ExecutionOutcome {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

/// Strategy for running one variant of [`Action`].
#[async_trait]
pub trait Executor: Send + Sync {
    async fn execute(&self, payload: &serde_json::Value) -> Result<ExecutionOutcome>;
}

/// Resolve an [`Action`] to the executor that runs it.
pub fn executor_for(action: &Action) -> Box<dyn Executor> {
    match action {
        Action::Command {
            command,
            timeout_seconds,
        } => Box::new(command::CommandExecutor::new(
            command.clone(),
            *timeout_seconds,
        )),
    }
}
