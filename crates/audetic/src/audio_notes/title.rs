//! Transcript-derived AudioNote Title generation through configured local agents.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

use crate::agents::{run_agent, AgentRunPaths, AgentRunRequest};
use crate::db::agent_profiles::AgentProfileRepository;
use crate::db::audio_notes::AudioNoteRepository;

use super::AudioNotePhase;

const TITLE_AGENT_TIMEOUT_SECONDS: u64 = 120;

/// Generate and persist a title when the note still has no title owner.
/// A concurrent Manual Title causes the final guarded write to be discarded.
pub async fn generate_audio_note_title(note_id: i64, db_path: &Path) -> Result<Option<String>> {
    let (transcript, title_version, profile) = {
        let conn = crate::db::init_db_at(db_path).context("Failed to open audetic database")?;
        AgentProfileRepository::ensure_builtin_profiles(&conn)?;
        let note = AudioNoteRepository::get(&conn, note_id)?
            .ok_or_else(|| anyhow::anyhow!("audio note {note_id} not found"))?;
        if note.status != AudioNotePhase::Completed.as_str() || note.title.is_some() {
            return Ok(None);
        }
        let transcript = note
            .transcript_text
            .filter(|text| !text.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("audio note {note_id} has no transcript text"))?;
        let profile = AgentProfileRepository::first_available(&conn)?
            .ok_or_else(|| anyhow::anyhow!("no available enabled agent profiles configured"))?;
        (transcript, note.title_version, profile)
    };

    let data_dir = db_path
        .parent()
        .context("Audetic database path has no parent directory")?;
    let paths = prepare_title_run(data_dir, note_id, &transcript, &profile.name)?;
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

    let conn = crate::db::init_db_at(db_path).context("Failed to reopen audetic database")?;
    if AudioNoteRepository::set_generated_title_if_unowned(&conn, note_id, &title, title_version)? {
        info!("Generated title for audio note {}: {}", note_id, title);
        Ok(Some(title))
    } else {
        Ok(None)
    }
}

/// Explicit user-requested regeneration only. Automatic titles belong to the
/// classification pipeline, so capture must never call this function.
pub(crate) fn spawn_title_generation_at(note_id: i64, db_path: PathBuf) {
    tokio::spawn(async move {
        if let Err(error) = generate_audio_note_title(note_id, &db_path).await {
            warn!(
                "Title generation failed for audio note {}: {:#}",
                note_id, error
            );
        }
    });
}

/// Validate a user-requested regeneration and release any current title.
pub fn prepare_title_regeneration(note_id: i64, db_path: &Path) -> Result<()> {
    let conn = crate::db::init_db_at(db_path).context("Failed to open audetic database")?;
    AgentProfileRepository::ensure_builtin_profiles(&conn)?;
    let note = AudioNoteRepository::get(&conn, note_id)?
        .ok_or_else(|| anyhow::anyhow!("audio note {note_id} not found"))?;
    if note.status != AudioNotePhase::Completed.as_str() {
        anyhow::bail!(
            "audio note {note_id} is in state `{}`; only completed notes can regenerate titles",
            note.status
        );
    }
    if note
        .transcript_text
        .as_deref()
        .is_none_or(|transcript| transcript.trim().is_empty())
    {
        anyhow::bail!("audio note {note_id} has no transcript text");
    }
    if AgentProfileRepository::first_available(&conn)?.is_none() {
        anyhow::bail!("no available enabled agent profiles configured");
    }
    if !AudioNoteRepository::release_title_for_regeneration(&conn, note_id)? {
        anyhow::bail!("audio note {note_id} could not release its title for regeneration");
    }
    Ok(())
}

fn prepare_title_run(
    data_dir: &Path,
    note_id: i64,
    transcript: &str,
    profile_name: &str,
) -> Result<AgentRunPaths> {
    let run_dir = data_dir
        .join("agent-runs")
        .join(format!("title-{note_id}-{}", uuid::Uuid::new_v4().simple()));
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
        "note_id": note_id,
        "purpose": "audio_note_title",
        "agent_profile": profile_name,
    }))
    .context("Failed to serialize title run metadata")?;
    std::fs::write(&metadata_path, metadata)
        .with_context(|| format!("Failed to write title run metadata to {metadata_path:?}"))?;
    std::fs::write(&prompt_path, render_title_prompt(note_id, &transcript_path))
        .with_context(|| format!("Failed to write title prompt to {prompt_path:?}"))?;

    Ok(AgentRunPaths {
        run_dir,
        prompt_path,
        transcript_path,
        template_path,
        metadata_path,
    })
}

fn render_title_prompt(note_id: i64, transcript_path: &std::path::Path) -> String {
    format!(
        r#"Create a concise Audio Note Title for note {note_id}.

Read the transcript at `{}`.

Return only the title as one plain-text line.
- Use 3 to 8 specific words describing the main topic or decision.
- Do not include dates, attendee or person names, quotation marks, or trailing punctuation.
- Do not include Markdown, labels, explanations, or generic phrases such as "Audio Notes".
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
