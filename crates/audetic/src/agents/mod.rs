//! Local coding-agent CLI execution.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

use crate::db::agent_profiles::{AgentProfile, PromptMode};

const MAX_CAPTURE_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone)]
pub struct AgentRunPaths {
    pub run_dir: PathBuf,
    pub prompt_path: PathBuf,
    pub transcript_path: PathBuf,
    pub template_path: PathBuf,
    pub metadata_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct AgentRunRequest {
    pub profile: AgentProfile,
    pub prompt: String,
    pub paths: AgentRunPaths,
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone)]
pub struct AgentRunOutput {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

pub async fn run_agent(request: AgentRunRequest) -> Result<AgentRunOutput> {
    if !request.profile.enabled {
        anyhow::bail!("agent profile `{}` is disabled", request.profile.name);
    }
    let executable = which::which(&request.profile.executable).with_context(|| {
        format!(
            "agent executable `{}` not found on PATH",
            request.profile.executable
        )
    })?;

    let args = render_args(&request.profile, &request.paths, &request.prompt);
    let mut child = tokio::process::Command::new(executable)
        .args(args)
        .current_dir(&request.paths.run_dir)
        .stdin(match request.profile.prompt_mode {
            PromptMode::Stdin => Stdio::piped(),
            PromptMode::Arg | PromptMode::FileArg => Stdio::null(),
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("Failed to spawn agent CLI")?;

    if request.profile.prompt_mode == PromptMode::Stdin {
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(request.prompt.as_bytes())
                .await
                .context("Failed to write prompt to agent stdin")?;
        }
    }

    match tokio::time::timeout(
        Duration::from_secs(request.timeout_seconds),
        child.wait_with_output(),
    )
    .await
    {
        Ok(Ok(output)) => Ok(AgentRunOutput {
            success: output.status.success(),
            exit_code: output.status.code(),
            stdout: truncate_utf8(&output.stdout),
            stderr: truncate_utf8(&output.stderr),
            timed_out: false,
        }),
        Ok(Err(e)) => Ok(AgentRunOutput {
            success: false,
            exit_code: None,
            stdout: String::new(),
            stderr: format!("failed to wait for child: {e}"),
            timed_out: false,
        }),
        Err(_) => Ok(AgentRunOutput {
            success: false,
            exit_code: None,
            stdout: String::new(),
            stderr: format!(
                "agent command timed out after {}s (process killed)",
                request.timeout_seconds
            ),
            timed_out: true,
        }),
    }
}

fn render_args(profile: &AgentProfile, paths: &AgentRunPaths, prompt: &str) -> Vec<String> {
    profile
        .args
        .iter()
        .map(|arg| render_arg(arg, paths, prompt))
        .collect()
}

fn render_arg(arg: &str, paths: &AgentRunPaths, prompt: &str) -> String {
    arg.replace("{run_dir}", &display_path(&paths.run_dir))
        .replace("{prompt_path}", &display_path(&paths.prompt_path))
        .replace("{transcript_path}", &display_path(&paths.transcript_path))
        .replace("{template_path}", &display_path(&paths.template_path))
        .replace("{metadata_path}", &display_path(&paths.metadata_path))
        .replace("{prompt_text}", prompt)
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn truncate_utf8(bytes: &[u8]) -> String {
    if bytes.len() <= MAX_CAPTURE_BYTES {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut s = String::from_utf8_lossy(&bytes[..MAX_CAPTURE_BYTES]).into_owned();
    s.push_str("\n…[truncated]");
    s
}
