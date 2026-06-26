//! SQLite persistence for local coding-agent CLI profiles.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// How the rendered meeting prompt is delivered to the agent CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PromptMode {
    /// Write prompt Markdown to child stdin.
    Stdin,
    /// Replace `{prompt_text}` in argv with the full rendered prompt.
    Arg,
    /// Write `prompt.md` to the run dir and pass path placeholders in argv.
    FileArg,
}

impl PromptMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stdin => "stdin",
            Self::Arg => "arg",
            Self::FileArg => "file_arg",
        }
    }

    pub fn parse(raw: &str) -> Result<Self> {
        match raw {
            "stdin" => Ok(Self::Stdin),
            "arg" => Ok(Self::Arg),
            "file_arg" => Ok(Self::FileArg),
            other => Err(anyhow::anyhow!("unknown prompt_mode `{other}`")),
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AgentProfile {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub executable: String,
    pub args: Vec<String>,
    pub prompt_mode: PromptMode,
    pub default_profile: bool,
    pub enabled: bool,
    pub available: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct NewAgentProfile {
    pub name: String,
    pub kind: String,
    pub executable: String,
    pub args: Vec<String>,
    pub prompt_mode: PromptMode,
    pub default_profile: bool,
    pub enabled: bool,
}

pub struct AgentProfileRepository;

impl AgentProfileRepository {
    pub fn ensure_builtin_profiles(conn: &Connection) -> Result<()> {
        for profile in builtin_profiles() {
            conn.execute(
                "INSERT OR IGNORE INTO agent_profiles \
                 (name, kind, executable, args_json, prompt_mode, default_profile, enabled) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    profile.name,
                    profile.kind,
                    profile.executable,
                    serde_json::to_string(&profile.args)?,
                    profile.prompt_mode.as_str(),
                    profile.default_profile as i64,
                    profile.enabled as i64,
                ],
            )
            .context("Failed to insert built-in agent profile")?;
        }
        Ok(())
    }

    pub fn list(conn: &Connection) -> Result<Vec<AgentProfile>> {
        let mut stmt = conn
            .prepare(
                "SELECT id, name, kind, executable, args_json, prompt_mode, \
                 default_profile, enabled, created_at, updated_at \
                 FROM agent_profiles ORDER BY default_profile DESC, name ASC",
            )
            .context("Failed to prepare agent profile list")?;
        let rows = stmt
            .query_map([], row_to_profile)
            .context("Failed to query agent profiles")?;
        let mut profiles = Vec::new();
        for row in rows {
            profiles.push(row??);
        }
        Ok(profiles)
    }

    pub fn get(conn: &Connection, id: i64) -> Result<Option<AgentProfile>> {
        conn.query_row(
            "SELECT id, name, kind, executable, args_json, prompt_mode, \
             default_profile, enabled, created_at, updated_at \
             FROM agent_profiles WHERE id = ?1",
            params![id],
            row_to_profile,
        )
        .optional()
        .context("Failed to query agent profile")?
        .transpose()
    }

    pub fn first_enabled(conn: &Connection) -> Result<Option<AgentProfile>> {
        conn.query_row(
            "SELECT id, name, kind, executable, args_json, prompt_mode, \
             default_profile, enabled, created_at, updated_at \
             FROM agent_profiles WHERE enabled = 1 \
             ORDER BY default_profile DESC, id ASC LIMIT 1",
            [],
            row_to_profile,
        )
        .optional()
        .context("Failed to query default agent profile")?
        .transpose()
    }
}

fn row_to_profile(row: &Row) -> rusqlite::Result<Result<AgentProfile>> {
    let id: i64 = row.get(0)?;
    let name: String = row.get(1)?;
    let kind: String = row.get(2)?;
    let executable: String = row.get(3)?;
    let args_json: String = row.get(4)?;
    let prompt_mode: String = row.get(5)?;
    let default_profile: i64 = row.get(6)?;
    let enabled: i64 = row.get(7)?;
    let created_at: String = row.get(8)?;
    let updated_at: String = row.get(9)?;

    Ok((|| {
        Ok(AgentProfile {
            id,
            name,
            kind,
            executable: executable.clone(),
            args: serde_json::from_str(&args_json).context("invalid args_json")?,
            prompt_mode: PromptMode::parse(&prompt_mode)?,
            default_profile: default_profile != 0,
            enabled: enabled != 0,
            available: which::which(&executable).is_ok(),
            created_at,
            updated_at,
        })
    })())
}

fn builtin_profiles() -> Vec<NewAgentProfile> {
    vec![
        NewAgentProfile {
            name: "Claude Code".to_string(),
            kind: "claude".to_string(),
            executable: "claude".to_string(),
            args: vec!["-p".into(), "--permission-mode".into(), "plan".into()],
            prompt_mode: PromptMode::Stdin,
            default_profile: true,
            enabled: true,
        },
        NewAgentProfile {
            name: "Codex".to_string(),
            kind: "codex".to_string(),
            executable: "codex".to_string(),
            args: vec![
                "exec".into(),
                "--sandbox".into(),
                "read-only".into(),
                "-".into(),
            ],
            prompt_mode: PromptMode::Stdin,
            default_profile: false,
            enabled: true,
        },
        NewAgentProfile {
            name: "OpenCode".to_string(),
            kind: "opencode".to_string(),
            executable: "opencode".to_string(),
            args: vec![
                "run".into(),
                "--dir".into(),
                "{run_dir}".into(),
                "--file".into(),
                "{prompt_path}".into(),
                "Follow the attached prompt exactly. Return only the requested Markdown artifact."
                    .into(),
            ],
            prompt_mode: PromptMode::FileArg,
            default_profile: false,
            enabled: true,
        },
        NewAgentProfile {
            name: "Cursor Agent".to_string(),
            kind: "cursor_agent".to_string(),
            executable: "cursor-agent".to_string(),
            args: vec![
                "-p".into(),
                "--mode".into(),
                "ask".into(),
                "--workspace".into(),
                "{run_dir}".into(),
            ],
            prompt_mode: PromptMode::Stdin,
            default_profile: false,
            enabled: true,
        },
        NewAgentProfile {
            name: "Cursor Agent (agent alias)".to_string(),
            kind: "cursor_agent".to_string(),
            executable: "agent".to_string(),
            args: vec![
                "-p".into(),
                "--mode".into(),
                "ask".into(),
                "--workspace".into(),
                "{run_dir}".into(),
            ],
            prompt_mode: PromptMode::Stdin,
            default_profile: false,
            enabled: true,
        },
    ]
}
