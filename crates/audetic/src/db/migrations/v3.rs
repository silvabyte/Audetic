//! Version 3: a user override for the inferred classification kind.

use anyhow::{Context, Result};
use rusqlite::Connection;

pub(super) fn apply(conn: &Connection) -> Result<Vec<String>> {
    conn.execute_batch(
        "ALTER TABLE audio_notes ADD COLUMN classification_kind_override TEXT CHECK(
            classification_kind_override IS NULL OR (
                length(classification_kind_override) BETWEEN 1 AND 64
                AND substr(classification_kind_override, 1, 1) GLOB '[a-z]'
                AND classification_kind_override NOT GLOB '*[^a-z0-9_-]*'
            )
         );
         DROP INDEX IF EXISTS idx_audio_notes_kind;
         CREATE INDEX idx_audio_notes_effective_kind ON audio_notes(
            COALESCE(
                classification_kind_override,
                json_extract(CASE WHEN json_valid(classification) THEN classification END, '$.kind')
            )
         );",
    )
    .context("Failed to add classification kind override")?;
    Ok(Vec::new())
}
