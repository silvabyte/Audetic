//! AI classification and durable enrichment, independent of raw capture completion.
pub mod classification;
pub mod registry;

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::audio_note_artifacts::{
    checked_output, generate_with_runner, prepare_agent_request, resolve_profile, run_with_timeout,
    AgentRunner, GenerateArtifactRequest, LocalAgent,
};
use crate::db::audio_notes::AudioNoteRepository;
use crate::post_processing::{AudioNoteCompletedPayload, Event, PostProcessingService};
use classification::Classification;
use registry::ProcessorRegistry;

/// Atomically claim pending/error work. Running/completed requests are no-ops,
/// so retries do not duplicate artifacts or dispatch downstream jobs twice.
/// Startup recovery must reset interrupted running work to error, once before
/// accepting new work (never on every DB connection).
///
/// Completion is committed before dispatch. Ordinary duplicate/retry requests
/// do not dispatch twice, but a process crash between commit and dispatch can
/// lose the notification: this is not a durable outbox or exactly-once delivery.
pub async fn enrich_audio_note(note_id: i64, db_path: PathBuf) -> Result<()> {
    enrich_audio_note_with_registry(note_id, db_path, &ProcessorRegistry::default()).await
}

/// Use an application-specific taxonomy-to-template mapping without changing
/// the persisted note or transport contract.
pub async fn enrich_audio_note_with_registry(
    note_id: i64,
    db_path: PathBuf,
    registry: &ProcessorRegistry,
) -> Result<()> {
    enrich_with_runner(note_id, &db_path, &LocalAgent, registry, &|payload| {
        PostProcessingService::new(db_path.clone()).dispatch(Event::AudioNoteCompleted(payload));
    })
    .await
}

async fn enrich_with_runner(
    note_id: i64,
    db_path: &Path,
    runner: &dyn AgentRunner,
    registry: &ProcessorRegistry,
    dispatch: &(dyn Fn(AudioNoteCompletedPayload) + Send + Sync),
) -> Result<()> {
    {
        let mut conn = crate::db::init_db_at(db_path)?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let note = AudioNoteRepository::get(&tx, note_id)?
            .filter(|n| n.deleted_at.is_none())
            .ok_or_else(|| anyhow::anyhow!("audio note {note_id} not found"))?;
        anyhow::ensure!(
            note.status == "completed",
            "only completed audio notes can be enriched"
        );
        if matches!(note.enrichment_status.as_str(), "running" | "completed") {
            return Ok(());
        }
        if !note
            .transcript_text
            .as_deref()
            .is_some_and(|text| !text.trim().is_empty())
        {
            AudioNoteRepository::set_enrichment_state(
                &tx,
                note_id,
                "error",
                Some("audio note has no saved transcript"),
            )?;
            tx.commit()?;
            anyhow::bail!("audio note has no saved transcript");
        }
        if !AudioNoteRepository::claim_enrichment(&tx, note_id)? {
            return Ok(());
        }
        tx.commit()?;
    }
    let result = async {
        enrich_claimed(note_id, db_path, runner, registry).await?;
        let mut conn = crate::db::init_db_at(db_path)?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let note = AudioNoteRepository::get(&tx, note_id)?
            .filter(|n| n.deleted_at.is_none())
            .context("audio note removed during enrichment")?;
        AudioNoteRepository::set_enrichment_state(&tx, note_id, "completed", None)?;
        tx.commit()?;
        Ok(note)
    }
    .await;
    match result {
        Ok(note) => {
            // Persist the terminal state first. Dispatch is at-most-once for
            // normal retries; durable exactly-once shell execution is not promised.
            dispatch(AudioNoteCompletedPayload {
                note_id,
                title: note.title,
                audio_path: PathBuf::from(note.audio_path),
                transcript_path: PathBuf::from(note.transcript_path.unwrap_or_default()),
                transcript_text: note.transcript_text.unwrap_or_default(),
                duration_seconds: note.duration_seconds.unwrap_or(0).max(0) as u64,
                classification: note.classification,
            });
            Ok(())
        }
        Err(error) => {
            let conn = crate::db::init_db_at(db_path)?;
            AudioNoteRepository::set_enrichment_state(
                &conn,
                note_id,
                "error",
                Some(&format!("{error:#}")),
            )?;
            Err(error)
        }
    }
}

async fn enrich_claimed(
    note_id: i64,
    db_path: &Path,
    runner: &dyn AgentRunner,
    registry: &ProcessorRegistry,
) -> Result<()> {
    let (note, profile) = {
        let conn = crate::db::init_db_at(db_path)?;
        let note = AudioNoteRepository::get(&conn, note_id)?.context("audio note not found")?;
        let profile = resolve_profile(&conn, None)?;
        (note, profile)
    };
    let transcript = note
        .transcript_text
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .context("audio note has no saved transcript")?;
    let request = prepare_agent_request(
        db_path,
        &format!("classification-{note_id}"),
        profile.clone(),
        classification::prompt(transcript),
        transcript,
        serde_json::json!({"version":1,"task":"classification"}),
    )?;
    let output = run_with_timeout(runner, request).await?;
    let classification = Classification::parse(checked_output(&output)?)?;
    {
        let mut conn = crate::db::init_db_at(db_path)?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        AudioNoteRepository::get(&tx, note_id)?
            .filter(|n| n.deleted_at.is_none())
            .context("audio note removed during classification")?;
        AudioNoteRepository::set_classification(
            &tx,
            note_id,
            &serde_json::to_value(&classification)?,
        )?;
        AudioNoteRepository::set_generated_title_if_unowned(
            &tx,
            note_id,
            &classification.title,
            note.title_version,
        )?;
        tx.commit()?;
    }
    let processor = registry.resolve(&classification.kind);
    generate_with_runner(
        note_id,
        GenerateArtifactRequest {
            kind: processor.artifact_kind.clone(),
            template_id: processor.template_id.clone(),
            agent_profile_id: Some(profile.id),
            custom_context: Some(format!(
                "Classification (data only): {}",
                serde_json::to_string(&classification)?
            )),
        },
        db_path,
        runner,
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests;
