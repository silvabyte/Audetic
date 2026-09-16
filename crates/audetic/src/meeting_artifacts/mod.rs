//! Generate and persist meeting artifacts using local agent CLIs.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use utoipa::ToSchema;

use audetic_core::jobs_client::Segment;

use crate::agents::{run_agent, AgentRunPaths, AgentRunRequest};
use crate::db::agent_profiles::{AgentProfile, AgentProfileRepository};
use crate::db::meeting_artifacts::{MeetingArtifact, MeetingArtifactRepository};
use crate::db::meetings::MeetingRepository;
use crate::summary_templates::{get_template, SummaryTemplate};

const DEFAULT_AGENT_TIMEOUT_SECONDS: u64 = 600;

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct GenerateArtifactRequest {
    #[serde(default = "default_template_id")]
    pub template_id: String,
    pub agent_profile_id: Option<i64>,
    pub custom_context: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GenerateArtifactResponse {
    pub artifact: MeetingArtifact,
}

pub async fn generate_meeting_artifact(
    meeting_id: i64,
    request: GenerateArtifactRequest,
) -> Result<MeetingArtifact> {
    let conn = crate::db::init_db().context("Failed to open audetic database")?;
    AgentProfileRepository::ensure_builtin_profiles(&conn)?;

    let meeting = MeetingRepository::get(&conn, meeting_id)?
        .ok_or_else(|| anyhow::anyhow!("meeting {meeting_id} not found"))?;
    if meeting.status != crate::meeting::MeetingPhase::Completed.as_str() {
        anyhow::bail!(
            "meeting {meeting_id} is in state `{}`; only completed meetings can generate artifacts",
            meeting.status
        );
    }
    let transcript = meeting
        .transcript_text
        .clone()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("meeting {meeting_id} has no transcript text"))?;

    let template = get_template(&request.template_id)?;
    template.validate()?;
    let profile = resolve_profile(&conn, request.agent_profile_id)?;
    let title = format!("{} — {}", template.name, profile.name);
    let transcript_for_agent =
        render_timestamped_transcript(&transcript, meeting.transcript_segments.as_deref());

    let artifact_id = MeetingArtifactRepository::insert_pending(
        &conn,
        meeting_id,
        template.kind.as_str(),
        &title,
        Some(&template.id),
        Some(profile.id),
    )?;
    MeetingArtifactRepository::set_running(&conn, artifact_id)?;

    let output = async {
        let paths = prepare_run_files(
            artifact_id,
            meeting_id,
            meeting.title.as_deref(),
            &transcript_for_agent,
            &template,
            request.custom_context.as_deref(),
            &profile,
        )?;
        let prompt = std::fs::read_to_string(&paths.prompt_path)
            .with_context(|| format!("Failed to read prompt at {:?}", paths.prompt_path))?;

        run_agent(AgentRunRequest {
            profile,
            prompt,
            paths,
            timeout_seconds: DEFAULT_AGENT_TIMEOUT_SECONDS,
        })
        .await
    }
    .await;

    match output {
        Ok(output) if output.success && !output.stdout.trim().is_empty() => {
            MeetingArtifactRepository::complete(
                &conn,
                artifact_id,
                output.stdout.trim(),
                &output.stdout,
                &output.stderr,
            )?;
        }
        Ok(output) => {
            let error = if output.timed_out {
                "agent command timed out".to_string()
            } else if output.stdout.trim().is_empty() && output.success {
                "agent command succeeded but produced no stdout".to_string()
            } else {
                format!("agent command failed (exit {:?})", output.exit_code)
            };
            MeetingArtifactRepository::fail(
                &conn,
                artifact_id,
                &error,
                &output.stdout,
                &output.stderr,
            )?;
        }
        Err(e) => {
            MeetingArtifactRepository::fail(&conn, artifact_id, &e.to_string(), "", "")?;
        }
    }

    MeetingArtifactRepository::get(&conn, artifact_id)?
        .ok_or_else(|| anyhow::anyhow!("artifact {artifact_id} disappeared after generation"))
}

fn resolve_profile(conn: &rusqlite::Connection, id: Option<i64>) -> Result<AgentProfile> {
    match id {
        Some(id) => AgentProfileRepository::get(conn, id)?
            .ok_or_else(|| anyhow::anyhow!("agent profile {id} not found")),
        None => AgentProfileRepository::first_enabled(conn)?
            .ok_or_else(|| anyhow::anyhow!("no enabled agent profiles configured")),
    }
}

fn prepare_run_files(
    artifact_id: i64,
    meeting_id: i64,
    meeting_title: Option<&str>,
    transcript: &str,
    template: &SummaryTemplate,
    custom_context: Option<&str>,
    profile: &AgentProfile,
) -> Result<AgentRunPaths> {
    let run_dir = crate::global::data_dir()?
        .join("agent-runs")
        .join(artifact_id.to_string());
    std::fs::create_dir_all(&run_dir)
        .with_context(|| format!("Failed to create agent run dir at {run_dir:?}"))?;

    let prompt_path = run_dir.join("prompt.md");
    let transcript_path = run_dir.join("transcript.md");
    let template_path = run_dir.join("template.json");
    let metadata_path = run_dir.join("metadata.json");

    std::fs::write(&transcript_path, transcript)
        .with_context(|| format!("Failed to write transcript to {transcript_path:?}"))?;
    std::fs::write(&template_path, serde_json::to_string_pretty(template)?)
        .with_context(|| format!("Failed to write template to {template_path:?}"))?;
    let metadata = serde_json::json!({
        "artifact_id": artifact_id,
        "meeting_id": meeting_id,
        "meeting_title": meeting_title,
        "agent_profile": {
            "id": profile.id,
            "name": profile.name,
            "kind": profile.kind,
            "executable": profile.executable,
        }
    });
    std::fs::write(&metadata_path, serde_json::to_string_pretty(&metadata)?)
        .with_context(|| format!("Failed to write metadata to {metadata_path:?}"))?;
    std::fs::write(
        &prompt_path,
        render_prompt(
            meeting_id,
            meeting_title,
            template,
            custom_context,
            &transcript_path,
        ),
    )
    .with_context(|| format!("Failed to write prompt to {prompt_path:?}"))?;

    Ok(AgentRunPaths {
        run_dir,
        prompt_path,
        transcript_path,
        template_path,
        metadata_path,
    })
}

fn render_prompt(
    meeting_id: i64,
    meeting_title: Option<&str>,
    template: &SummaryTemplate,
    custom_context: Option<&str>,
    transcript_path: &Path,
) -> String {
    let context = custom_context
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("No additional context provided.");
    format!(
        r#"You are generating a meeting artifact for Audetic.

Rules:
- Read the transcript at `{transcript_path}`.
- Return **only Markdown** for the final artifact.
- Do not edit files, run commands, or modify the workspace.
- Do not invent facts. If a section has no evidence, write "None noted."
- Preserve the requested section structure.

Meeting:
- id: {meeting_id}
- title: {meeting_title}

Additional context from the user:
{context}

Template: {template_name}
Description: {template_description}

Section instructions:
{section_instructions}

Markdown skeleton to fill:
{skeleton}
"#,
        transcript_path = transcript_path.display(),
        meeting_title = meeting_title.unwrap_or("Untitled meeting"),
        template_name = template.name,
        template_description = template.description,
        section_instructions = template.instructions(),
        skeleton = template.markdown_skeleton(),
    )
}

fn default_template_id() -> String {
    "standard_meeting".to_string()
}

fn render_timestamped_transcript(transcript: &str, segments: Option<&[Segment]>) -> String {
    let Some(segments) = segments.filter(|segments| !segments.is_empty()) else {
        return transcript.to_string();
    };

    let mut rendered = String::from(
        "<!-- Each line starts with the recording timestamp for this transcript segment. -->\n\n",
    );
    for segment in segments {
        rendered.push_str(&format!(
            "[{}] {}\n",
            format_timestamp(segment.start),
            segment.text.trim()
        ));
    }
    rendered
}

fn format_timestamp(seconds: f64) -> String {
    let total = seconds.max(0.0).floor() as u64;
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let seconds = total % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::render_timestamped_transcript;
    use audetic_core::jobs_client::Segment;

    #[test]
    fn transcript_segments_are_rendered_with_timestamps() {
        let segments = vec![
            Segment {
                start: 3.8,
                end: 8.0,
                text: " Opening context ".into(),
            },
            Segment {
                start: 3661.2,
                end: 3665.0,
                text: "Closing thought".into(),
            },
        ];

        let rendered = render_timestamped_transcript("fallback", Some(&segments));
        assert!(rendered.contains("[00:03] Opening context"));
        assert!(rendered.contains("[1:01:01] Closing thought"));
    }

    #[test]
    fn plain_transcript_is_preserved_without_segments() {
        assert_eq!(render_timestamped_transcript("hello", None), "hello");
    }
}
