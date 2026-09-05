use anyhow::{bail, Context, Result};
use audetic_core::sync::{CacheLevel, HubId, PayloadAvailability, RecordId};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use sha2::{Digest, Sha256};

use std::path::PathBuf;

use crate::sync::protocol::{
    is_canonical_sha256, ChangeCursor, ChangeOperation, ChangePage, ChangeRecord, ChangeTarget,
    RecordKind, RecordingPayloadDescriptor, Snapshot, MAX_CHANGE_PAGE,
};

use super::library_change_feed::{kind_name, parse_kind};
use super::library_codec::{decode_snapshot, encode_change, encode_snapshot, STORED_CODEC_V1};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CacheGeneration {
    pub id: i64,
    pub source_hub_id: HubId,
    pub level: CacheLevel,
    pub start_cursor: ChangeCursor,
    pub target_cursor: ChangeTarget,
    pub applied_cursor: ChangeCursor,
    pub complete: bool,
    pub active: bool,
}

#[derive(Clone, Debug)]
pub struct CacheItem {
    pub record_id: RecordId,
    pub kind: RecordKind,
    pub authoritative_revision: u64,
    pub snapshot: Snapshot,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CacheTombstone {
    pub record_id: RecordId,
    pub kind: RecordKind,
    pub authoritative_revision: u64,
    pub deleted_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CacheBlobClaim {
    pub record_id: RecordId,
    pub kind: RecordKind,
    pub descriptor: RecordingPayloadDescriptor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedCacheBlob {
    pub source_hub_id: HubId,
    pub checksum: String,
    pub local_path: PathBuf,
    pub byte_size: u64,
    pub media_type: String,
}

/// A post-commit filesystem cleanup instruction. The repository only removes
/// DB ownership and never performs filesystem I/O itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CacheCleanup {
    pub source_hub_id: HubId,
    pub checksum: String,
    pub path: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplyPageOutcome {
    Applied,
    Duplicate,
}

/// Source-scoped durable Library Cache boundary.
pub struct LibraryCacheStore;

impl LibraryCacheStore {
    pub fn begin_generation(
        conn: &mut Connection,
        source_hub_id: HubId,
        level: CacheLevel,
        start_cursor: ChangeCursor,
        target_cursor: ChangeTarget,
    ) -> Result<i64> {
        if level == CacheLevel::LiveOnly {
            bail!("Live Only uses the deletion overlay, not a cache generation");
        }
        if start_cursor > target_cursor.cursor() {
            bail!("generation start cursor is past its target");
        }
        let tx = conn.transaction().context("starting cache generation")?;
        ensure_source(&tx, source_hub_id)?;
        let saved_cursor = Self::source_cursor(&tx, source_hub_id)?;
        if target_cursor.cursor() < saved_cursor {
            bail!("cache generation target would move the source cursor backwards");
        }
        tx.execute(
            "INSERT INTO library_cache_generations
                (source_hub_id,cache_level,start_cursor,target_cursor,applied_cursor)
             VALUES(?1,?2,?3,?4,?3)",
            params![
                source_hub_id.to_string(),
                level.as_str(),
                to_i64(start_cursor.value(), "generation start cursor")?,
                to_i64(target_cursor.cursor().value(), "generation target cursor")?,
            ],
        )
        .context("creating source-scoped Library Cache generation")?;
        let id = tx.last_insert_rowid();
        if start_cursor != ChangeCursor::ZERO {
            let active = Self::active_generation(&tx, source_hub_id)?
                .context("a nonzero cache refresh requires an active baseline generation")?;
            if !active.complete
                || active.applied_cursor != start_cursor
                || active.target_cursor.cursor() != start_cursor
            {
                bail!("active Library Cache generation does not match the refresh start cursor");
            }
            clone_generation_contents(&tx, source_hub_id, active.id, id)?;
        }
        tx.commit().context("committing cache generation")?;
        Ok(id)
    }

    pub fn generation(
        conn: &Connection,
        source_hub_id: HubId,
        generation_id: i64,
    ) -> Result<Option<CacheGeneration>> {
        conn.query_row(
            "SELECT generation_id,source_hub_id,cache_level,start_cursor,target_cursor,
                    applied_cursor,complete,active
             FROM library_cache_generations
             WHERE source_hub_id=?1 AND generation_id=?2",
            params![source_hub_id.to_string(), generation_id],
            generation_from_row,
        )
        .optional()
        .context("reading source-scoped Library Cache generation")
        .and_then(|value| value.transpose())
    }

    pub fn active_generation(
        conn: &Connection,
        source_hub_id: HubId,
    ) -> Result<Option<CacheGeneration>> {
        conn.query_row(
            "SELECT generation_id,source_hub_id,cache_level,start_cursor,target_cursor,
                    applied_cursor,complete,active
             FROM library_cache_generations
             WHERE source_hub_id=?1 AND active=1",
            [source_hub_id.to_string()],
            generation_from_row,
        )
        .optional()
        .context("reading active source-scoped Library Cache generation")
        .and_then(|value| value.transpose())
    }

    pub fn source_cursor(conn: &Connection, source_hub_id: HubId) -> Result<ChangeCursor> {
        let cursor = conn
            .query_row(
                "SELECT change_cursor FROM library_cache_sources WHERE source_hub_id=?1",
                [source_hub_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .context("reading source-scoped Library Cache cursor")?
            .unwrap_or(0);
        Ok(ChangeCursor::new(
            u64::try_from(cursor).context("negative source cache cursor")?,
        ))
    }

    pub fn apply_validated_page(
        conn: &mut Connection,
        source_hub_id: HubId,
        generation_id: i64,
        page: &ChangePage,
    ) -> Result<ApplyPageOutcome> {
        validate_page_shape(page)?;
        let page_hash = page_hash(page)?;
        let tx = conn
            .transaction()
            .context("starting Library Cache page transaction")?;
        let generation = Self::generation(&tx, source_hub_id, generation_id)?
            .context("Library Cache generation does not exist for this source Hub")?;
        if generation.active || generation.complete {
            if duplicate_generation_page(&tx, &generation, page, &page_hash)? {
                tx.commit()?;
                return Ok(ApplyPageOutcome::Duplicate);
            }
            bail!("cannot append to a complete or active Library Cache generation");
        }
        if page.target_cursor != generation.target_cursor {
            bail!("Library Cache page changed the generation target");
        }
        if page.after_cursor != generation.applied_cursor {
            if duplicate_generation_page(&tx, &generation, page, &page_hash)? {
                tx.commit()?;
                return Ok(ApplyPageOutcome::Duplicate);
            }
            bail!("Library Cache page has a gap or inconsistent overlap");
        }
        for change in &page.changes {
            apply_generation_change(&tx, &generation, change)?;
        }
        tx.execute(
            "INSERT INTO library_cache_applied_pages
                (source_hub_id,generation_id,after_cursor,through_cursor,target_cursor,page_hash)
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                source_hub_id.to_string(),
                generation_id,
                to_i64(page.after_cursor.value(), "page after cursor")?,
                to_i64(page.through_cursor.value(), "page through cursor")?,
                to_i64(page.target_cursor.cursor().value(), "page target cursor")?,
                page_hash,
            ],
        )?;
        tx.execute(
            "UPDATE library_cache_generations
             SET applied_cursor=?3,
                 complete=?4,
                 completed_at=CASE WHEN ?4=1 THEN CURRENT_TIMESTAMP ELSE NULL END
             WHERE source_hub_id=?1 AND generation_id=?2",
            params![
                source_hub_id.to_string(),
                generation_id,
                to_i64(page.through_cursor.value(), "page through cursor")?,
                page.complete,
            ],
        )?;
        tx.commit().context("committing Library Cache page")?;
        Ok(ApplyPageOutcome::Applied)
    }

    /// Activate an exactly-complete generation and save its source cursor in
    /// the same transaction. The previous active generation remains untouched
    /// unless every precondition succeeds.
    pub fn activate_complete_generation(
        conn: &mut Connection,
        source_hub_id: HubId,
        generation_id: i64,
    ) -> Result<Vec<CacheCleanup>> {
        let tx = conn
            .transaction()
            .context("starting Library Cache activation")?;
        let generation = Self::generation(&tx, source_hub_id, generation_id)?
            .context("Library Cache generation does not exist for this source Hub")?;
        if !generation.complete || generation.applied_cursor != generation.target_cursor.cursor() {
            bail!("Library Cache generation has not reached its exact target");
        }
        ensure_source(&tx, source_hub_id)?;
        if generation.target_cursor.cursor() < Self::source_cursor(&tx, source_hub_id)? {
            bail!("Library Cache activation would move the source cursor backwards");
        }
        tx.execute(
            "UPDATE library_cache_generations SET active=0
             WHERE source_hub_id=?1 AND active=1",
            [source_hub_id.to_string()],
        )?;
        tx.execute(
            "UPDATE library_cache_generations SET active=1,activated_at=CURRENT_TIMESTAMP
             WHERE source_hub_id=?1 AND generation_id=?2",
            params![source_hub_id.to_string(), generation_id],
        )?;
        tx.execute(
            "UPDATE library_cache_sources
             SET change_cursor=MAX(change_cursor,?2),updated_at=CURRENT_TIMESTAMP
             WHERE source_hub_id=?1",
            params![
                source_hub_id.to_string(),
                to_i64(
                    generation.target_cursor.cursor().value(),
                    "generation target cursor"
                )?,
            ],
        )?;
        tx.execute(
            "DELETE FROM library_cache_generations
             WHERE source_hub_id=?1 AND generation_id!=?2 AND complete=1",
            params![source_hub_id.to_string(), generation_id],
        )?;
        let cleanup = collect_orphaned_blobs(&tx)?;
        tx.commit().context("committing Library Cache activation")?;
        Ok(cleanup)
    }

    pub fn apply_live_only_page(
        conn: &mut Connection,
        source_hub_id: HubId,
        page: &ChangePage,
    ) -> Result<ApplyPageOutcome> {
        validate_page_shape(page)?;
        let hash = page_hash(page)?;
        let tx = conn
            .transaction()
            .context("starting Live Only cache page")?;
        ensure_source(&tx, source_hub_id)?;
        let (saved, in_progress): (i64, Option<i64>) = tx.query_row(
            "SELECT change_cursor,live_target_cursor FROM library_cache_sources
             WHERE source_hub_id=?1",
            [source_hub_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let saved = ChangeCursor::new(u64::try_from(saved).context("negative live cursor")?);
        if page.after_cursor != saved {
            if duplicate_live_page(&tx, source_hub_id, page, &hash)? {
                tx.commit()?;
                return Ok(ApplyPageOutcome::Duplicate);
            }
            bail!("Live Only page has a gap or inconsistent overlap");
        }
        if let Some(expected) = in_progress {
            let expected = u64::try_from(expected).context("negative live target")?;
            if page.target_cursor.cursor().value() != expected {
                bail!("Live Only traversal changed its immutable target");
            }
        }
        for change in &page.changes {
            if change.operation == ChangeOperation::Delete {
                apply_live_deletion(&tx, source_hub_id, change)?;
            }
        }
        tx.execute(
            "INSERT INTO library_cache_live_pages
                (source_hub_id,after_cursor,through_cursor,target_cursor,page_hash)
             VALUES(?1,?2,?3,?4,?5)",
            params![
                source_hub_id.to_string(),
                to_i64(page.after_cursor.value(), "live page after cursor")?,
                to_i64(page.through_cursor.value(), "live page through cursor")?,
                to_i64(
                    page.target_cursor.cursor().value(),
                    "live page target cursor"
                )?,
                hash,
            ],
        )?;
        tx.execute(
            "UPDATE library_cache_sources
             SET change_cursor=?2,
                 live_target_cursor=CASE WHEN ?3=1 THEN NULL ELSE ?4 END,
                 updated_at=CURRENT_TIMESTAMP
             WHERE source_hub_id=?1",
            params![
                source_hub_id.to_string(),
                to_i64(page.through_cursor.value(), "live page through cursor")?,
                page.complete,
                to_i64(
                    page.target_cursor.cursor().value(),
                    "live page target cursor"
                )?,
            ],
        )?;
        tx.commit().context("committing Live Only cache page")?;
        Ok(ApplyPageOutcome::Applied)
    }

    pub fn active_items(conn: &Connection, source_hub_id: HubId) -> Result<Vec<CacheItem>> {
        let mut statement = conn.prepare(
            "SELECT i.record_id,i.kind,i.authoritative_revision,i.codec_version,i.item_json
             FROM library_cache_items i
             JOIN library_cache_generations g
               ON g.source_hub_id=i.source_hub_id AND g.generation_id=i.generation_id
             WHERE i.source_hub_id=?1 AND g.active=1 AND g.complete=1
             ORDER BY CASE i.kind WHEN 'dictation' THEN 0 WHEN 'meeting' THEN 1 ELSE 2 END,
                      i.record_id",
        )?;
        let rows = statement.query_map([source_hub_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, u16>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        rows.map(|row| {
            let (record_id, kind, revision, codec, json) = row?;
            let record_id = parse_id(&record_id, "cached record ID")?;
            let kind = parse_kind(&kind)?;
            let snapshot = decode_snapshot(codec, &json)?;
            if snapshot.record_id() != record_id || snapshot.kind() != kind {
                bail!("cached item identity disagrees with its stored body");
            }
            Ok(CacheItem {
                record_id,
                kind,
                authoritative_revision: u64::try_from(revision)
                    .context("negative cached authoritative revision")?,
                snapshot,
            })
        })
        .collect()
    }

    pub fn active_tombstones(
        conn: &Connection,
        source_hub_id: HubId,
    ) -> Result<Vec<CacheTombstone>> {
        let mut statement = conn.prepare(
            "SELECT t.record_id,t.kind,t.authoritative_revision,t.deleted_at
             FROM library_cache_tombstones t
             JOIN library_cache_generations g
               ON g.source_hub_id=t.source_hub_id AND g.generation_id=t.generation_id
             WHERE t.source_hub_id=?1 AND g.active=1 AND g.complete=1
             ORDER BY t.record_id",
        )?;
        let rows = statement.query_map([source_hub_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        rows.map(|row| {
            let (id, kind, revision, deleted_at) = row?;
            Ok(CacheTombstone {
                record_id: parse_id(&id, "cached tombstone record ID")?,
                kind: parse_kind(&kind)?,
                authoritative_revision: u64::try_from(revision)
                    .context("negative cached tombstone revision")?,
                deleted_at,
            })
        })
        .collect()
    }

    pub fn blob_claims(
        conn: &Connection,
        source_hub_id: HubId,
        generation_id: i64,
    ) -> Result<Vec<CacheBlobClaim>> {
        // Requiring the generation under this source closes cross-Hub probing.
        Self::generation(conn, source_hub_id, generation_id)?
            .context("Library Cache generation does not exist for this source Hub")?;
        let mut statement = conn.prepare(
            "SELECT record_id,kind,checksum,byte_size,media_type,availability
             FROM library_cache_blob_refs
             WHERE source_hub_id=?1 AND generation_id=?2
             ORDER BY record_id",
        )?;
        let rows =
            statement.query_map(params![source_hub_id.to_string(), generation_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })?;
        rows.map(|row| {
            let (id, kind, checksum, size, media_type, availability) = row?;
            Ok(CacheBlobClaim {
                record_id: parse_id(&id, "cached blob claim record ID")?,
                kind: parse_kind(&kind)?,
                descriptor: RecordingPayloadDescriptor {
                    checksum,
                    byte_size: size
                        .map(|value| {
                            u64::try_from(value).context("negative cached blob claim size")
                        })
                        .transpose()?,
                    media_type,
                    availability: parse_availability(&availability)?,
                },
            })
        })
        .collect()
    }

    pub fn register_verified_blob(conn: &mut Connection, blob: &VerifiedCacheBlob) -> Result<()> {
        if !is_canonical_sha256(&blob.checksum) {
            bail!("verified cache blob checksum is not canonical SHA-256");
        }
        if blob.local_path.as_os_str().is_empty() || blob.media_type.is_empty() {
            bail!("verified cache blob path and media type are required");
        }
        let tx = conn
            .transaction()
            .context("starting verified cache blob registration")?;
        let claimed: bool = tx.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM library_cache_blob_refs
                WHERE source_hub_id=?1 AND checksum=?2 AND byte_size=?3 AND media_type=?4
             )",
            params![
                blob.source_hub_id.to_string(),
                blob.checksum,
                to_i64(blob.byte_size, "cached blob size")?,
                blob.media_type,
            ],
            |row| row.get(0),
        )?;
        if !claimed {
            bail!("source Hub has no matching cache claim for this blob");
        }
        let changed = tx.execute(
            "INSERT INTO library_cache_blobs
                (source_hub_id,checksum,local_path,byte_size,media_type,verified)
             VALUES(?1,?2,?3,?4,?5,1)
             ON CONFLICT(source_hub_id,checksum) DO UPDATE SET
                local_path=excluded.local_path,byte_size=excluded.byte_size,
                media_type=excluded.media_type,verified=1,updated_at=CURRENT_TIMESTAMP
             WHERE library_cache_blobs.byte_size=excluded.byte_size
               AND library_cache_blobs.media_type=excluded.media_type",
            params![
                blob.source_hub_id.to_string(),
                blob.checksum,
                blob.local_path.to_string_lossy(),
                to_i64(blob.byte_size, "cached blob size")?,
                blob.media_type,
            ],
        )?;
        if changed != 1 {
            bail!("verified cache blob conflicts with previously stored metadata");
        }
        tx.commit()
            .context("committing verified cache blob registration")
    }

    pub fn verified_blob(
        conn: &Connection,
        source_hub_id: HubId,
        checksum: &str,
    ) -> Result<Option<VerifiedCacheBlob>> {
        conn.query_row(
            "SELECT checksum,local_path,byte_size,media_type
             FROM library_cache_blobs
             WHERE source_hub_id=?1 AND checksum=?2 AND verified=1",
            params![source_hub_id.to_string(), checksum],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?
        .map(|(checksum, path, size, media_type)| {
            Ok(VerifiedCacheBlob {
                source_hub_id,
                checksum,
                local_path: path.into(),
                byte_size: u64::try_from(size).context("negative verified cache blob size")?,
                media_type,
            })
        })
        .transpose()
    }

    pub fn live_overlay_contains(
        conn: &Connection,
        source_hub_id: HubId,
        record_id: RecordId,
    ) -> Result<bool> {
        conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM library_cache_live_overlay
                WHERE source_hub_id=?1 AND record_id=?2
             )",
            params![source_hub_id.to_string(), record_id.to_string()],
            |row| row.get(0),
        )
        .context("checking source-scoped Live Only deletion overlay")
    }

    pub fn abandon_incomplete_generation(
        conn: &mut Connection,
        source_hub_id: HubId,
        generation_id: i64,
    ) -> Result<Vec<CacheCleanup>> {
        let tx = conn
            .transaction()
            .context("starting incomplete cache abandonment")?;
        let generation = Self::generation(&tx, source_hub_id, generation_id)?
            .context("Library Cache generation does not exist for this source Hub")?;
        if generation.complete || generation.active {
            bail!("only an incomplete inactive generation may be abandoned");
        }
        tx.execute(
            "DELETE FROM library_cache_generations
             WHERE source_hub_id=?1 AND generation_id=?2",
            params![source_hub_id.to_string(), generation_id],
        )?;
        let cleanup = collect_orphaned_blobs(&tx)?;
        tx.commit()
            .context("committing incomplete cache abandonment")?;
        Ok(cleanup)
    }
}

fn clone_generation_contents(
    conn: &Connection,
    source_hub_id: HubId,
    from_generation_id: i64,
    to_generation_id: i64,
) -> Result<()> {
    let source = source_hub_id.to_string();
    conn.execute(
        "INSERT INTO library_cache_items
            (source_hub_id,generation_id,record_id,kind,authoritative_revision,
             codec_version,item_json)
         SELECT source_hub_id,?3,record_id,kind,authoritative_revision,codec_version,item_json
         FROM library_cache_items
         WHERE source_hub_id=?1 AND generation_id=?2",
        params![source, from_generation_id, to_generation_id],
    )?;
    conn.execute(
        "INSERT INTO library_cache_tombstones
            (source_hub_id,generation_id,record_id,kind,authoritative_revision,deleted_at)
         SELECT source_hub_id,?3,record_id,kind,authoritative_revision,deleted_at
         FROM library_cache_tombstones
         WHERE source_hub_id=?1 AND generation_id=?2",
        params![source, from_generation_id, to_generation_id],
    )?;
    conn.execute(
        "INSERT INTO library_cache_blob_refs
            (source_hub_id,generation_id,record_id,payload_role,kind,checksum,
             byte_size,media_type,availability)
         SELECT source_hub_id,?3,record_id,payload_role,kind,checksum,
                byte_size,media_type,availability
         FROM library_cache_blob_refs
         WHERE source_hub_id=?1 AND generation_id=?2",
        params![source, from_generation_id, to_generation_id],
    )?;
    Ok(())
}

fn apply_generation_change(
    conn: &Connection,
    generation: &CacheGeneration,
    change: &ChangeRecord,
) -> Result<()> {
    validate_change(change)?;
    let source = generation.source_hub_id.to_string();
    let id = change.record_id.to_string();
    let current_item: Option<(String, i64)> = conn
        .query_row(
            "SELECT kind,authoritative_revision FROM library_cache_items
             WHERE source_hub_id=?1 AND generation_id=?2 AND record_id=?3",
            params![source, generation.id, id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let current_tombstone: Option<(String, i64)> = conn
        .query_row(
            "SELECT kind,authoritative_revision FROM library_cache_tombstones
             WHERE source_hub_id=?1 AND generation_id=?2 AND record_id=?3",
            params![source, generation.id, id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    for (kind, revision) in current_item.iter().chain(current_tombstone.iter()) {
        if kind != kind_name(change.kind) {
            bail!("cached record kind is immutable");
        }
        let revision = u64::try_from(*revision).context("negative cached revision")?;
        if change.authoritative_revision <= revision {
            bail!("change overlaps an equal or newer cached revision");
        }
    }

    match change.operation {
        ChangeOperation::Delete => {
            conn.execute(
                "DELETE FROM library_cache_items
                 WHERE source_hub_id=?1 AND generation_id=?2 AND record_id=?3",
                params![source, generation.id, id],
            )?;
            conn.execute(
                "INSERT INTO library_cache_tombstones
                    (source_hub_id,generation_id,record_id,kind,authoritative_revision,deleted_at)
                 VALUES(?1,?2,?3,?4,?5,?6)
                 ON CONFLICT(source_hub_id,generation_id,record_id) DO UPDATE SET
                    authoritative_revision=excluded.authoritative_revision,
                    deleted_at=excluded.deleted_at",
                params![
                    source,
                    generation.id,
                    id,
                    kind_name(change.kind),
                    to_i64(change.authoritative_revision, "change revision")?,
                    change.changed_at,
                ],
            )?;
        }
        ChangeOperation::Upsert | ChangeOperation::PayloadAvailability => {
            if current_tombstone.is_some() {
                bail!("a cached tombstone cannot be restored by a later upsert");
            }
            let snapshot = change
                .snapshot
                .as_ref()
                .context("non-delete change omitted its snapshot")?;
            if let Snapshot::Artifact(artifact) = snapshot {
                let parent_exists: bool = conn.query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM library_cache_items
                        WHERE source_hub_id=?1 AND generation_id=?2 AND record_id=?3
                          AND kind='meeting'
                     )",
                    params![source, generation.id, artifact.parent_record_id.to_string()],
                    |row| row.get(0),
                )?;
                if !parent_exists {
                    bail!("cached artifact parent meeting is absent");
                }
            }
            conn.execute(
                "INSERT INTO library_cache_items
                    (source_hub_id,generation_id,record_id,kind,authoritative_revision,
                     codec_version,item_json)
                 VALUES(?1,?2,?3,?4,?5,?6,?7)
                 ON CONFLICT(source_hub_id,generation_id,record_id) DO UPDATE SET
                    authoritative_revision=excluded.authoritative_revision,
                    codec_version=excluded.codec_version,item_json=excluded.item_json",
                params![
                    source,
                    generation.id,
                    id,
                    kind_name(change.kind),
                    to_i64(change.authoritative_revision, "change revision")?,
                    STORED_CODEC_V1,
                    encode_snapshot(snapshot)?,
                ],
            )?;
            conn.execute(
                "DELETE FROM library_cache_blob_refs
                 WHERE source_hub_id=?1 AND generation_id=?2 AND record_id=?3",
                params![source, generation.id, id],
            )?;
            if let Some(descriptor) = snapshot_payload(snapshot) {
                let (checksum, size, media_type) = descriptor_parts(descriptor)?;
                conn.execute(
                    "INSERT INTO library_cache_blob_refs
                        (source_hub_id,generation_id,record_id,kind,checksum,byte_size,
                         media_type,availability)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                    params![
                        source,
                        generation.id,
                        id,
                        kind_name(change.kind),
                        checksum,
                        size.map(|value| to_i64(value, "payload byte size"))
                            .transpose()?,
                        media_type,
                        availability_name(descriptor.availability),
                    ],
                )?;
            }
        }
    }
    Ok(())
}

fn apply_live_deletion(
    conn: &Connection,
    source_hub_id: HubId,
    change: &ChangeRecord,
) -> Result<()> {
    validate_change(change)?;
    let current: Option<(String, i64)> = conn
        .query_row(
            "SELECT kind,authoritative_revision FROM library_cache_live_overlay
             WHERE source_hub_id=?1 AND record_id=?2",
            params![source_hub_id.to_string(), change.record_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((kind, revision)) = current {
        if kind != kind_name(change.kind)
            || change.authoritative_revision
                <= u64::try_from(revision).context("negative overlay revision")?
        {
            bail!("Live Only deletion overlaps an inconsistent revision");
        }
    }
    conn.execute(
        "INSERT INTO library_cache_live_overlay
            (source_hub_id,record_id,kind,authoritative_revision,deleted_at,change_cursor)
         VALUES(?1,?2,?3,?4,?5,?6)
         ON CONFLICT(source_hub_id,record_id) DO UPDATE SET
            authoritative_revision=excluded.authoritative_revision,
            deleted_at=excluded.deleted_at,change_cursor=excluded.change_cursor",
        params![
            source_hub_id.to_string(),
            change.record_id.to_string(),
            kind_name(change.kind),
            to_i64(change.authoritative_revision, "overlay revision")?,
            change.changed_at,
            to_i64(change.cursor.value(), "overlay cursor")?,
        ],
    )?;
    Ok(())
}

fn validate_page_shape(page: &ChangePage) -> Result<()> {
    if page.changes.len() > MAX_CHANGE_PAGE {
        bail!("change page exceeds the maximum of {MAX_CHANGE_PAGE} records");
    }
    let target = page.target_cursor.cursor();
    if page.after_cursor > target
        || page.through_cursor < page.after_cursor
        || page.through_cursor > target
    {
        bail!("change page cursor bounds are malformed");
    }
    let mut expected = page.after_cursor;
    for change in &page.changes {
        expected = expected
            .checked_next()
            .context("change cursor space is exhausted")?;
        if change.cursor != expected || change.cursor > target {
            bail!("change page contains a gap, overlap, or post-target row");
        }
        validate_change(change)?;
    }
    if page.changes.is_empty() {
        if page.through_cursor != page.after_cursor
            || !page.complete
            || page.through_cursor != target
        {
            bail!("an empty change page must explicitly complete at its unchanged target");
        }
    } else if expected != page.through_cursor {
        bail!("change page through cursor does not match its final row");
    }
    if page.complete != (page.through_cursor == target) {
        bail!("change page completion does not exactly match its target");
    }
    Ok(())
}

fn validate_change(change: &ChangeRecord) -> Result<()> {
    if change.cursor == ChangeCursor::ZERO || change.authoritative_revision == 0 {
        bail!("change cursor and authoritative revision must be positive");
    }
    if change.changed_at.is_empty() {
        bail!("change timestamp must not be empty");
    }
    match change.operation {
        ChangeOperation::Delete if change.snapshot.is_some() => {
            bail!("delete change contains a live snapshot")
        }
        ChangeOperation::Delete => Ok(()),
        ChangeOperation::Upsert | ChangeOperation::PayloadAvailability => {
            let snapshot = change
                .snapshot
                .as_ref()
                .context("non-delete change omitted its snapshot")?;
            if matches!(snapshot, Snapshot::Delete(_))
                || snapshot.record_id() != change.record_id
                || snapshot.kind() != change.kind
                || change.origin_device_id != Some(snapshot.origin_device_id())
            {
                bail!("change envelope disagrees with its self-contained snapshot");
            }
            validate_snapshot(snapshot)?;
            Ok(())
        }
    }
}

fn validate_snapshot(snapshot: &Snapshot) -> Result<()> {
    let (declared_kind, schema_version, local_version) = match snapshot {
        Snapshot::Dictation(value) => (value.kind, value.schema_version, value.local_version),
        Snapshot::Meeting(value) => (value.kind, value.schema_version, value.local_version),
        Snapshot::Artifact(value) => (value.kind, value.schema_version, value.local_version),
        Snapshot::Delete(_) => bail!("deletion snapshot cannot be stored as a cache item"),
    };
    if declared_kind != snapshot.kind() || schema_version != 1 || local_version == 0 {
        bail!("cache snapshot kind or version is unsupported");
    }
    if let Some(descriptor) = snapshot_payload(snapshot) {
        descriptor_parts(descriptor)?;
    }
    Ok(())
}

fn duplicate_generation_page(
    conn: &Connection,
    generation: &CacheGeneration,
    page: &ChangePage,
    hash: &str,
) -> Result<bool> {
    if page.through_cursor > generation.applied_cursor {
        return Ok(false);
    }
    page_matches(
        conn,
        "SELECT through_cursor,target_cursor,page_hash
         FROM library_cache_applied_pages
         WHERE source_hub_id=?1 AND generation_id=?2 AND after_cursor=?3",
        params![
            generation.source_hub_id.to_string(),
            generation.id,
            to_i64(page.after_cursor.value(), "duplicate page after cursor")?,
        ],
        page,
        hash,
    )
}

fn duplicate_live_page(
    conn: &Connection,
    source_hub_id: HubId,
    page: &ChangePage,
    hash: &str,
) -> Result<bool> {
    page_matches(
        conn,
        "SELECT through_cursor,target_cursor,page_hash
         FROM library_cache_live_pages
         WHERE source_hub_id=?1 AND after_cursor=?2",
        params![
            source_hub_id.to_string(),
            to_i64(
                page.after_cursor.value(),
                "duplicate live page after cursor"
            )?,
        ],
        page,
        hash,
    )
}

fn page_matches<P: rusqlite::Params>(
    conn: &Connection,
    sql: &str,
    params: P,
    page: &ChangePage,
    hash: &str,
) -> Result<bool> {
    let stored: Option<(i64, i64, String)> = conn
        .query_row(sql, params, |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .optional()?;
    Ok(stored.is_some_and(|(through, target, stored_hash)| {
        u64::try_from(through).ok() == Some(page.through_cursor.value())
            && u64::try_from(target).ok() == Some(page.target_cursor.cursor().value())
            && stored_hash == hash
    }))
}

fn page_hash(page: &ChangePage) -> Result<String> {
    let mut digest = Sha256::new();
    digest.update(b"audetic-library-cache-page-v1\0");
    for value in [
        page.after_cursor.value(),
        page.through_cursor.value(),
        page.target_cursor.cursor().value(),
        u64::from(page.complete),
    ] {
        digest.update(value.to_be_bytes());
    }
    for change in &page.changes {
        digest.update(change.cursor.value().to_be_bytes());
        let body = encode_change(change)?;
        digest.update((body.len() as u64).to_be_bytes());
        digest.update(body.as_bytes());
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn collect_orphaned_blobs(conn: &Transaction<'_>) -> Result<Vec<CacheCleanup>> {
    let orphans = {
        let mut statement = conn.prepare(
            "SELECT b.source_hub_id,b.checksum,b.local_path,
                    EXISTS(SELECT 1 FROM library_blobs a
                           WHERE a.canonical_path=b.local_path)
             FROM library_cache_blobs b
             WHERE NOT EXISTS(
                SELECT 1 FROM library_cache_blob_refs r
                WHERE r.source_hub_id=b.source_hub_id AND r.checksum=b.checksum
             )
             ORDER BY b.source_hub_id,b.checksum",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, bool>(3)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows
    };
    let mut cleanup = Vec::new();
    for (source, checksum, path, authoritative_owned) in orphans {
        conn.execute(
            "DELETE FROM library_cache_blobs WHERE source_hub_id=?1 AND checksum=?2",
            params![source, checksum],
        )?;
        if !authoritative_owned {
            cleanup.push(CacheCleanup {
                source_hub_id: parse_id(&source, "cache blob source Hub ID")?,
                checksum,
                path: path.into(),
            });
        }
    }
    Ok(cleanup)
}

fn ensure_source(conn: &Connection, source_hub_id: HubId) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO library_cache_sources(source_hub_id) VALUES(?1)",
        [source_hub_id.to_string()],
    )?;
    Ok(())
}

fn generation_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<CacheGeneration>> {
    let source: String = row.get(1)?;
    let level: String = row.get(2)?;
    let start: i64 = row.get(3)?;
    let target: i64 = row.get(4)?;
    let applied: i64 = row.get(5)?;
    Ok((|| {
        Ok(CacheGeneration {
            id: row.get(0)?,
            source_hub_id: parse_id(&source, "cache generation source Hub ID")?,
            level: level.parse().map_err(anyhow::Error::msg)?,
            start_cursor: ChangeCursor::new(
                u64::try_from(start).context("negative generation start cursor")?,
            ),
            target_cursor: ChangeTarget::new(ChangeCursor::new(
                u64::try_from(target).context("negative generation target cursor")?,
            )),
            applied_cursor: ChangeCursor::new(
                u64::try_from(applied).context("negative generation applied cursor")?,
            ),
            complete: row.get(6)?,
            active: row.get(7)?,
        })
    })())
}

fn snapshot_payload(snapshot: &Snapshot) -> Option<&RecordingPayloadDescriptor> {
    match snapshot {
        Snapshot::Dictation(value) => Some(&value.payload.recording_payload),
        Snapshot::Meeting(value) => Some(&value.payload.recording_payload),
        Snapshot::Artifact(_) | Snapshot::Delete(_) => None,
    }
}

type DescriptorParts<'a> = (Option<&'a str>, Option<u64>, Option<&'a str>);

fn descriptor_parts(descriptor: &RecordingPayloadDescriptor) -> Result<DescriptorParts<'_>> {
    match descriptor.availability {
        PayloadAvailability::Unavailable | PayloadAvailability::NeedsAttention => {
            if descriptor.checksum.is_some()
                || descriptor.byte_size.is_some()
                || descriptor.media_type.is_some()
            {
                bail!("unavailable cache payload contains blob metadata");
            }
        }
        PayloadAvailability::Pending | PayloadAvailability::Available => {
            if descriptor.checksum.is_none()
                || descriptor.byte_size.is_none_or(|size| size == 0)
                || descriptor.media_type.as_deref().is_none_or(str::is_empty)
            {
                bail!("available cache payload omitted required blob metadata");
            }
            if !descriptor
                .checksum
                .as_deref()
                .is_some_and(is_canonical_sha256)
            {
                bail!("cache payload checksum is not canonical SHA-256");
            }
        }
    }
    Ok((
        descriptor.checksum.as_deref(),
        descriptor.byte_size,
        descriptor.media_type.as_deref(),
    ))
}

fn availability_name(value: PayloadAvailability) -> &'static str {
    match value {
        PayloadAvailability::Available => "available",
        PayloadAvailability::Pending => "pending",
        PayloadAvailability::Unavailable => "unavailable",
        PayloadAvailability::NeedsAttention => "needs_attention",
    }
}

fn parse_availability(value: &str) -> Result<PayloadAvailability> {
    match value {
        "available" => Ok(PayloadAvailability::Available),
        "pending" => Ok(PayloadAvailability::Pending),
        "unavailable" => Ok(PayloadAvailability::Unavailable),
        "needs_attention" => Ok(PayloadAvailability::NeedsAttention),
        _ => bail!("invalid cached payload availability {value:?}"),
    }
}

fn parse_id<T>(value: &str, field: &str) -> Result<T>
where
    T: std::str::FromStr<Err = String>,
{
    value
        .parse()
        .map_err(anyhow::Error::msg)
        .with_context(|| format!("invalid {field}"))
}

fn to_i64(value: u64, field: &str) -> Result<i64> {
    i64::try_from(value).with_context(|| format!("{field} exceeds SQLite integer range"))
}

#[cfg(test)]
mod tests {
    use audetic_core::sync::DeviceId;

    use crate::sync::protocol::{DictationPayload, DictationSnapshot};

    use super::*;

    fn dictation_change(
        cursor: u64,
        record_id: RecordId,
        origin: DeviceId,
        revision: u64,
        text: &str,
    ) -> ChangeRecord {
        ChangeRecord {
            cursor: ChangeCursor::new(cursor),
            operation: ChangeOperation::Upsert,
            kind: RecordKind::Dictation,
            record_id,
            origin_device_id: Some(origin),
            authoritative_revision: revision,
            snapshot: Some(Snapshot::Dictation(DictationSnapshot {
                kind: RecordKind::Dictation,
                schema_version: 1,
                record_id,
                origin_device_id: origin,
                local_version: revision,
                created_at: "2026-09-05T10:00:00Z".into(),
                updated_at: format!("2026-09-05T10:00:{revision:02}Z"),
                payload: DictationPayload {
                    text: text.into(),
                    recording_payload: Default::default(),
                },
            })),
            changed_at: format!("2026-09-05T10:00:{revision:02}Z"),
        }
    }

    fn page(after: u64, target: u64, changes: Vec<ChangeRecord>) -> ChangePage {
        let through = changes.last().map_or(after, |change| change.cursor.value());
        ChangePage {
            target_cursor: ChangeTarget::new(ChangeCursor::new(target)),
            after_cursor: ChangeCursor::new(after),
            through_cursor: ChangeCursor::new(through),
            complete: through == target,
            changes,
        }
    }

    #[test]
    fn generations_are_source_scoped_incremental_and_atomically_activated() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let source = HubId::new();
        let other_source = HubId::new();
        let origin = DeviceId::new();
        let first_id = RecordId::new();
        let second_id = RecordId::new();

        let first_generation = LibraryCacheStore::begin_generation(
            &mut conn,
            source,
            CacheLevel::TextForOfflineUse,
            ChangeCursor::ZERO,
            ChangeTarget::new(ChangeCursor::new(1)),
        )
        .unwrap();
        let first_page = page(0, 1, vec![dictation_change(1, first_id, origin, 1, "one")]);
        assert_eq!(
            LibraryCacheStore::apply_validated_page(
                &mut conn,
                source,
                first_generation,
                &first_page,
            )
            .unwrap(),
            ApplyPageOutcome::Applied
        );
        LibraryCacheStore::activate_complete_generation(&mut conn, source, first_generation)
            .unwrap();
        assert_eq!(
            LibraryCacheStore::active_items(&conn, source)
                .unwrap()
                .len(),
            1
        );
        assert!(LibraryCacheStore::active_items(&conn, other_source)
            .unwrap()
            .is_empty());
        assert!(LibraryCacheStore::begin_generation(
            &mut conn,
            other_source,
            CacheLevel::TextForOfflineUse,
            ChangeCursor::new(1),
            ChangeTarget::new(ChangeCursor::new(2)),
        )
        .is_err());

        let second_generation = LibraryCacheStore::begin_generation(
            &mut conn,
            source,
            CacheLevel::TextForOfflineUse,
            ChangeCursor::new(1),
            ChangeTarget::new(ChangeCursor::new(2)),
        )
        .unwrap();
        assert_eq!(
            LibraryCacheStore::active_generation(&conn, source)
                .unwrap()
                .unwrap()
                .id,
            first_generation
        );
        assert_eq!(
            LibraryCacheStore::active_items(&conn, source)
                .unwrap()
                .len(),
            1
        );

        let second_page = page(1, 2, vec![dictation_change(2, second_id, origin, 1, "two")]);
        LibraryCacheStore::apply_validated_page(&mut conn, source, second_generation, &second_page)
            .unwrap();
        assert_eq!(
            LibraryCacheStore::apply_validated_page(
                &mut conn,
                source,
                second_generation,
                &second_page,
            )
            .unwrap(),
            ApplyPageOutcome::Duplicate
        );
        LibraryCacheStore::activate_complete_generation(&mut conn, source, second_generation)
            .unwrap();
        let items = LibraryCacheStore::active_items(&conn, source).unwrap();
        assert_eq!(items.len(), 2);
        assert!(items.iter().any(|item| item.record_id == first_id));
        assert!(items.iter().any(|item| item.record_id == second_id));
        assert_eq!(
            LibraryCacheStore::source_cursor(&conn, source)
                .unwrap()
                .value(),
            2
        );
        assert!(LibraryCacheStore::begin_generation(
            &mut conn,
            source,
            CacheLevel::TextForOfflineUse,
            ChangeCursor::ZERO,
            ChangeTarget::new(ChangeCursor::new(1)),
        )
        .is_err());
    }

    #[test]
    fn a_failed_page_rolls_back_every_change_in_that_page() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let source = HubId::new();
        let origin = DeviceId::new();
        let generation = LibraryCacheStore::begin_generation(
            &mut conn,
            source,
            CacheLevel::TextForOfflineUse,
            ChangeCursor::ZERO,
            ChangeTarget::new(ChangeCursor::new(2)),
        )
        .unwrap();
        let repeated_id = RecordId::new();
        let bad_page = page(
            0,
            2,
            vec![
                dictation_change(1, repeated_id, origin, 1, "accepted first"),
                dictation_change(2, repeated_id, origin, 1, "invalid overlap"),
            ],
        );

        assert!(
            LibraryCacheStore::apply_validated_page(&mut conn, source, generation, &bad_page,)
                .is_err()
        );
        let stored = LibraryCacheStore::generation(&conn, source, generation)
            .unwrap()
            .unwrap();
        assert_eq!(stored.applied_cursor, ChangeCursor::ZERO);
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM library_cache_items WHERE generation_id=?1",
                [generation],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn live_only_pages_save_only_source_scoped_deletions() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let source = HubId::new();
        let other_source = HubId::new();
        let record_id = RecordId::new();
        let deletion = ChangeRecord {
            cursor: ChangeCursor::new(1),
            operation: ChangeOperation::Delete,
            kind: RecordKind::Meeting,
            record_id,
            origin_device_id: None,
            authoritative_revision: 1,
            snapshot: None,
            changed_at: "2026-09-05T11:00:00Z".into(),
        };
        let page = page(0, 1, vec![deletion]);

        LibraryCacheStore::apply_live_only_page(&mut conn, source, &page).unwrap();
        assert!(LibraryCacheStore::live_overlay_contains(&conn, source, record_id).unwrap());
        assert!(!LibraryCacheStore::live_overlay_contains(&conn, other_source, record_id).unwrap());
        assert_eq!(
            LibraryCacheStore::apply_live_only_page(&mut conn, source, &page).unwrap(),
            ApplyPageOutcome::Duplicate
        );
    }

    #[test]
    fn verified_blobs_require_an_exact_source_scoped_generation_claim() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let source = HubId::new();
        let other_source = HubId::new();
        let record_id = RecordId::new();
        let checksum = "a".repeat(64);
        let mut change = dictation_change(1, record_id, DeviceId::new(), 1, "audio");
        if let Some(Snapshot::Dictation(snapshot)) = change.snapshot.as_mut() {
            snapshot.payload.recording_payload =
                RecordingPayloadDescriptor::pending(checksum.clone(), 10, "audio/wav".into());
        }
        let generation = LibraryCacheStore::begin_generation(
            &mut conn,
            source,
            CacheLevel::TextAndAvailableAudio,
            ChangeCursor::ZERO,
            ChangeTarget::new(ChangeCursor::new(1)),
        )
        .unwrap();
        LibraryCacheStore::apply_validated_page(
            &mut conn,
            source,
            generation,
            &page(0, 1, vec![change]),
        )
        .unwrap();
        let claims = LibraryCacheStore::blob_claims(&conn, source, generation).unwrap();
        assert_eq!(claims.len(), 1);
        assert_eq!(
            claims[0].descriptor.checksum.as_deref(),
            Some(checksum.as_str())
        );

        let path = PathBuf::from("/tmp/audetic-cache-test-blob");
        assert!(LibraryCacheStore::register_verified_blob(
            &mut conn,
            &VerifiedCacheBlob {
                source_hub_id: other_source,
                checksum: checksum.clone(),
                local_path: path.clone(),
                byte_size: 10,
                media_type: "audio/wav".into(),
            }
        )
        .is_err());
        LibraryCacheStore::register_verified_blob(
            &mut conn,
            &VerifiedCacheBlob {
                source_hub_id: source,
                checksum: checksum.clone(),
                local_path: path.clone(),
                byte_size: 10,
                media_type: "audio/wav".into(),
            },
        )
        .unwrap();
        let stored = LibraryCacheStore::verified_blob(&conn, source, &checksum)
            .unwrap()
            .unwrap();
        assert_eq!(stored.local_path, path);
        assert!(
            LibraryCacheStore::verified_blob(&conn, other_source, &checksum)
                .unwrap()
                .is_none()
        );
    }
}
