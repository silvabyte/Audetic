//! Transcript-derived Meeting Title generation through configured local agents.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

use crate::agents::{run_agent, AgentRunPaths, AgentRunRequest};
use crate::db::agent_profiles::AgentProfileRepository;
use crate::db::meetings::MeetingRepository;

use super::MeetingPhase;

const TITLE_AGENT_TIMEOUT_SECONDS: u64 = 120;

/// Generate and persist a title when the meeting still has no title owner.
/// A concurrent Manual Title causes the final guarded write to be discarded.
pub async fn generate_meeting_title(meeting_id: i64) -> Result<Option<String>> {
    generate_meeting_title_at(meeting_id, &crate::global::db_file()?).await
}

async fn generate_meeting_title_at(meeting_id: i64, db_path: &Path) -> Result<Option<String>> {
    let (transcript, title_version, profile) = {
        let conn = crate::db::open_db_at(db_path).context("Failed to open audetic database")?;
        AgentProfileRepository::ensure_builtin_profiles(&conn)?;
        let meeting = MeetingRepository::get(&conn, meeting_id)?
            .ok_or_else(|| anyhow::anyhow!("meeting {meeting_id} not found"))?;
        if meeting.status != MeetingPhase::Completed.as_str() || meeting.title.is_some() {
            return Ok(None);
        }
        let transcript = meeting
            .transcript_text
            .filter(|text| !text.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("meeting {meeting_id} has no transcript text"))?;
        let profile = AgentProfileRepository::first_available(&conn)?
            .ok_or_else(|| anyhow::anyhow!("no available enabled agent profiles configured"))?;
        (transcript, meeting.title_version, profile)
    };

    let data_dir = db_path
        .parent()
        .context("Audetic database path has no parent directory")?;
    let paths = prepare_title_run(data_dir, meeting_id, &transcript, &profile.name)?;
    let prompt = std::fs::read_to_string(&paths.prompt_path)
        .with_context(|| format!("Failed to read title prompt at {:?}", paths.prompt_path))?;
    let run_dir = paths.run_dir.clone();
    let output = run_agent(AgentRunRequest {
        profile,
        prompt,
        paths,
        timeout_seconds: TITLE_AGENT_TIMEOUT_SECONDS,
    })
    .await;
    let _ = std::fs::remove_dir_all(run_dir);
    let output = output?;
    if !output.success {
        anyhow::bail!(
            "title agent failed{}: {}",
            output
                .exit_code
                .map(|code| format!(" with exit code {code}"))
                .unwrap_or_default(),
            output.stderr.trim()
        );
    }
    let title = normalize_generated_title(&output.stdout)
        .ok_or_else(|| anyhow::anyhow!("title agent returned an invalid Generated Title"))?;

    let conn = crate::db::open_db_at(db_path).context("Failed to reopen audetic database")?;
    if MeetingRepository::set_generated_title_if_unowned(&conn, meeting_id, &title, title_version)?
    {
        info!("Generated title for meeting {}: {}", meeting_id, title);
        Ok(Some(title))
    } else {
        Ok(None)
    }
}

/// Start title generation without joining it to meeting completion.
pub fn spawn_title_generation(meeting_id: i64) {
    let Ok(db_path) = crate::global::db_file() else {
        warn!("Best-effort title generation could not resolve the database path");
        return;
    };
    spawn_title_generation_at(meeting_id, db_path);
}

pub(crate) fn spawn_title_generation_at(meeting_id: i64, db_path: PathBuf) {
    tokio::spawn(async move {
        if let Err(error) = generate_meeting_title_at(meeting_id, &db_path).await {
            warn!(
                "Best-effort title generation failed for meeting {}: {:#}",
                meeting_id, error
            );
        }
    });
}

/// Validate a user-requested regeneration and release any current title.
pub fn prepare_title_regeneration(meeting_id: i64) -> Result<()> {
    let conn = crate::db::open_db().context("Failed to open audetic database")?;
    AgentProfileRepository::ensure_builtin_profiles(&conn)?;
    let meeting = MeetingRepository::get(&conn, meeting_id)?
        .ok_or_else(|| anyhow::anyhow!("meeting {meeting_id} not found"))?;
    if meeting.status != MeetingPhase::Completed.as_str() {
        anyhow::bail!(
            "meeting {meeting_id} is in state `{}`; only completed meetings can regenerate titles",
            meeting.status
        );
    }
    if meeting
        .transcript_text
        .as_deref()
        .is_none_or(|transcript| transcript.trim().is_empty())
    {
        anyhow::bail!("meeting {meeting_id} has no transcript text");
    }
    if AgentProfileRepository::first_available(&conn)?.is_none() {
        anyhow::bail!("no available enabled agent profiles configured");
    }
    if !MeetingRepository::release_title_for_regeneration(&conn, meeting_id)? {
        anyhow::bail!("meeting {meeting_id} could not release its title for regeneration");
    }
    Ok(())
}

fn prepare_title_run(
    data_dir: &Path,
    meeting_id: i64,
    transcript: &str,
    profile_name: &str,
) -> Result<AgentRunPaths> {
    let run_dir = data_dir.join("agent-runs").join(format!(
        "title-{meeting_id}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&run_dir)
        .with_context(|| format!("Failed to create title run dir at {run_dir:?}"))?;
    let prompt_path = run_dir.join("prompt.md");
    let transcript_path = run_dir.join("transcript.md");
    let template_path = run_dir.join("title-contract.json");
    let metadata_path = run_dir.join("metadata.json");
    std::fs::write(&transcript_path, transcript)
        .with_context(|| format!("Failed to write transcript to {transcript_path:?}"))?;
    std::fs::write(
        &template_path,
        r#"{"minimum_words":3,"maximum_words":8,"format":"plain text"}"#,
    )
    .with_context(|| format!("Failed to write title contract to {template_path:?}"))?;
    let metadata = serde_json::to_string_pretty(&serde_json::json!({
        "meeting_id": meeting_id,
        "purpose": "meeting_title",
        "agent_profile": profile_name,
    }))
    .context("Failed to serialize title run metadata")?;
    std::fs::write(&metadata_path, metadata)
        .with_context(|| format!("Failed to write title run metadata to {metadata_path:?}"))?;
    std::fs::write(
        &prompt_path,
        render_title_prompt(meeting_id, &transcript_path),
    )
    .with_context(|| format!("Failed to write title prompt to {prompt_path:?}"))?;

    Ok(AgentRunPaths {
        run_dir,
        prompt_path,
        transcript_path,
        template_path,
        metadata_path,
    })
}

fn render_title_prompt(meeting_id: i64, transcript_path: &std::path::Path) -> String {
    format!(
        r#"Create a concise Meeting Title for Audetic meeting {meeting_id}.

Read the transcript at `{}`.

Return only the title as one plain-text line.
- Use 3 to 8 specific words describing the main topic or decision.
- Do not include dates, attendee or person names, quotation marks, or trailing punctuation.
- Do not include Markdown, labels, explanations, or generic phrases such as "Meeting Notes".
- Do not edit files or run commands.
"#,
        transcript_path.display()
    )
}

/// Normalize one local-agent response into the Generated Title contract.
/// Invalid output is discarded rather than persisting agent explanation text.
pub fn normalize_generated_title(output: &str) -> Option<String> {
    if output.lines().count() != 1 {
        return None;
    }

    let title = output
        .trim()
        .trim_matches(['"', '\'', '`'])
        .trim_end_matches(['.', ',', ';', ':', '!', '?'])
        .trim();
    if title.contains(['"', '\'', '`']) {
        return None;
    }

    let words: Vec<&str> = title.split_whitespace().collect();
    if !(3..=8).contains(&words.len()) || words.iter().any(|word| looks_like_date(word)) {
        return None;
    }

    Some(title.to_string())
}

fn looks_like_date(word: &str) -> bool {
    let candidate = word.trim_matches(|character: char| {
        !character.is_ascii_alphanumeric() && !matches!(character, '-' | '/')
    });
    let normalized = candidate
        .trim_matches(|character: char| !character.is_ascii_alphanumeric())
        .to_ascii_lowercase();
    const MONTHS: [&str; 24] = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
        "jan",
        "feb",
        "mar",
        "apr",
        "jun",
        "jul",
        "aug",
        "sep",
        "sept",
        "oct",
        "nov",
        "dec",
    ];
    if MONTHS.contains(&normalized.as_str()) {
        return true;
    }
    if normalized.len() == 4
        && normalized
            .chars()
            .all(|character| character.is_ascii_digit())
    {
        return true;
    }
    let separators = candidate
        .chars()
        .filter(|character| matches!(character, '-' | '/'))
        .count();
    separators > 0
        && candidate
            .chars()
            .all(|character| character.is_ascii_digit() || matches!(character, '-' | '/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_compliant_agent_title() {
        assert_eq!(
            normalize_generated_title("\"Reducing Checkout Latency Spikes.\""),
            Some("Reducing Checkout Latency Spikes".to_string())
        );
    }

    #[test]
    fn rejects_titles_outside_public_generation_contract() {
        assert_eq!(normalize_generated_title("Weekly sync"), None);
        assert_eq!(
            normalize_generated_title("Planning Review September 2 2026"),
            None
        );
        assert_eq!(
            normalize_generated_title("Planning Review 09/02/2026"),
            None
        );
        assert_eq!(
            normalize_generated_title("Specific Planning Review\nHope this helps"),
            None
        );
    }

    #[test]
    fn prompt_states_the_public_title_constraints() {
        let prompt = render_title_prompt(42, std::path::Path::new("/tmp/transcript.md"));
        assert!(prompt.contains("3 to 8 specific words"));
        assert!(prompt.contains("dates, attendee or person names"));
        assert!(prompt.contains("quotation marks, or trailing punctuation"));
    }
}
