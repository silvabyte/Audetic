use anyhow::{bail, Context, Result};
use audetic_core::sync::{CacheLevel, HubId, PayloadAvailability, RecordId};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use sha2::{Digest, Sha256};

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::sync::protocol::{
    is_canonical_sha256, ChangeCursor, ChangeOperation, ChangePage, ChangeRecord, ChangeTarget,
    RecordKind, RecordingPayloadDescriptor, Snapshot, MAX_BLOB_BYTES, MAX_CHANGE_PAGE,
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
    source_hub_id: HubId,
    checksum: String,
    local_path: PathBuf,
    byte_size: u64,
    media_type: String,
}

impl VerifiedCacheBlob {
    /// Verify and atomically publish a source-scoped cache blob outside the
    /// authoritative Recording Payload namespace.
    pub async fn publish_for_db(
        db_path: &Path,
        source_hub_id: HubId,
        source_path: &Path,
        checksum: String,
        byte_size: u64,
        media_type: String,
    ) -> Result<Self> {
        let store = crate::sync::payload::BlobStore::new(
            cache_blob_root(db_path).join(source_hub_id.to_string()),
        );
        let stored = store
            .put_file(source_path, &checksum, byte_size, &media_type)
            .await?;
        Ok(Self {
            source_hub_id,
            checksum: stored.checksum,
            local_path: stored.path,
            byte_size: stored.byte_size,
            media_type: stored.media_type,
        })
    }
}

const CACHE_BLOB_NAMESPACE: &str = "library-cache-blobs";

pub fn cache_blob_path_for_db(
    db_path: &Path,
    source_hub_id: HubId,
    checksum: &str,
) -> Result<PathBuf> {
    if !is_canonical_sha256(checksum) {
        bail!("cache blob checksum is not canonical SHA-256");
    }
    Ok(cache_blob_root(db_path)
        .join(source_hub_id.to_string())
        .join(&checksum[..2])
        .join(checksum))
}

fn cache_blob_root(db_path: &Path) -> PathBuf {
    db_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("sync")
        .join(CACHE_BLOB_NAMESPACE)
}

fn validate_cache_blob_path(blob: &VerifiedCacheBlob) -> Result<()> {
    validate_cache_blob_path_parts(blob.source_hub_id, &blob.checksum, &blob.local_path)
}

fn validate_cache_blob_path_parts(
    source_hub_id: HubId,
    checksum: &str,
    local_path: &Path,
) -> Result<()> {
    if !is_canonical_sha256(checksum) {
        bail!("cache blob checksum is not canonical SHA-256");
    }
    let expected_suffix = Path::new(CACHE_BLOB_NAMESPACE)
        .join(source_hub_id.to_string())
        .join(&checksum[..2])
        .join(checksum);
    if !local_path.ends_with(expected_suffix) {
        bail!("verified cache blob path is outside its source-scoped cache namespace");
    }
    Ok(())
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
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("starting cache generation")?;
        ensure_source(&tx, source_hub_id)?;
        let generation_in_progress: bool = tx.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM library_cache_generations
                WHERE source_hub_id=?1 AND active=0
             )",
            [source_hub_id.to_string()],
            |row| row.get(0),
        )?;
        if generation_in_progress {
            bail!("a cache generation is already in progress for this source Hub");
        }
        let saved_cursor = Self::source_cursor(&tx, source_hub_id)?;
        if target_cursor.cursor() < saved_cursor {
            bail!("cache generation target would move the source cursor backwards");
        }
        let baseline = if start_cursor == ChangeCursor::ZERO {
            None
        } else {
            let active = Self::active_generation(&tx, source_hub_id)?
                .context("a nonzero cache refresh requires an active baseline generation")?;
            if !active.complete
                || active.applied_cursor != start_cursor
                || active.target_cursor.cursor() != start_cursor
            {
                bail!("active Library Cache generation does not match the refresh start cursor");
            }
            if active.level != level {
                bail!("changing the Library Cache level requires a full rebuild");
            }
            Some(active.id)
        };
        tx.execute(
            "UPDATE library_cache_sources
             SET live_target_cursor=NULL,updated_at=CURRENT_TIMESTAMP
             WHERE source_hub_id=?1",
            [source_hub_id.to_string()],
        )?;
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
        if let Some(baseline) = baseline {
            clone_generation_contents(&tx, source_hub_id, baseline, id)?;
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

    pub fn live_traversal_target(
        conn: &Connection,
        source_hub_id: HubId,
    ) -> Result<Option<ChangeTarget>> {
        let target = conn
            .query_row(
                "SELECT live_target_cursor FROM library_cache_sources WHERE source_hub_id=?1",
                [source_hub_id.to_string()],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()
            .context("reading source-scoped Live Only target")?
            .flatten();
        target
            .map(|target| {
                Ok(ChangeTarget::new(ChangeCursor::new(
                    u64::try_from(target).context("negative Live Only target")?,
                )))
            })
            .transpose()
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
            .transaction_with_behavior(TransactionBehavior::Immediate)
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
    ) -> Result<()> {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
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
        if generation.level == CacheLevel::TextAndAvailableAudio {
            verify_generation_blobs(&tx, &generation)?;
        }
        tx.execute(
            "DELETE FROM library_cache_generations
             WHERE source_hub_id=?1 AND active=1 AND generation_id!=?2",
            params![source_hub_id.to_string(), generation_id],
        )?;
        tx.execute(
            "UPDATE library_cache_generations SET active=1,activated_at=CURRENT_TIMESTAMP
             WHERE source_hub_id=?1 AND generation_id=?2",
            params![source_hub_id.to_string(), generation_id],
        )?;
        tx.execute(
            "UPDATE library_cache_sources
             SET change_cursor=MAX(change_cursor,?2),live_target_cursor=NULL,
                 updated_at=CURRENT_TIMESTAMP
             WHERE source_hub_id=?1",
            params![
                source_hub_id.to_string(),
                to_i64(
                    generation.target_cursor.cursor().value(),
                    "generation target cursor"
                )?,
            ],
        )?;
        enqueue_orphaned_blobs(&tx)?;
        tx.commit().context("committing Library Cache activation")?;
        Ok(())
    }

    pub fn apply_live_only_page(
        conn: &mut Connection,
        source_hub_id: HubId,
        page: &ChangePage,
    ) -> Result<ApplyPageOutcome> {
        validate_page_shape(page)?;
        let hash = page_hash(page)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("starting Live Only cache page")?;
        ensure_source(&tx, source_hub_id)?;
        let (saved, in_progress): (i64, Option<i64>) = tx.query_row(
            "SELECT change_cursor,live_target_cursor FROM library_cache_sources
             WHERE source_hub_id=?1",
            [source_hub_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let saved = ChangeCursor::new(u64::try_from(saved).context("negative live cursor")?);
        let full_generation_in_progress: bool = tx.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM library_cache_generations
                WHERE source_hub_id=?1 AND active=0
             )",
            [source_hub_id.to_string()],
            |row| row.get(0),
        )?;
        if full_generation_in_progress {
            bail!("a full Library Cache generation is already in progress for this source Hub");
        }
        if duplicate_live_page(&tx, source_hub_id, page, &hash)? {
            tx.commit()?;
            return Ok(ApplyPageOutcome::Duplicate);
        }
        if page.after_cursor != saved {
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
            "SELECT r.record_id,r.kind,r.checksum,r.byte_size,r.media_type,r.availability
             FROM library_cache_blob_refs r
             INNER JOIN library_cache_generations g
               ON g.source_hub_id=r.source_hub_id AND g.generation_id=r.generation_id
             WHERE r.source_hub_id=?1 AND r.generation_id=?2
               AND g.cache_level='text_and_available_audio'
               AND r.availability='available'
             ORDER BY r.record_id",
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
        validate_blob_metadata(blob.byte_size, &blob.media_type)?;
        validate_cache_blob_path(blob)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("starting verified cache blob registration")?;
        verify_blob_file(&blob.local_path, &blob.checksum, blob.byte_size)?;
        let authoritative_owns_path: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM library_blobs WHERE canonical_path=?1)",
            [blob.local_path.to_string_lossy()],
            |row| row.get(0),
        )?;
        if authoritative_owns_path {
            bail!("cache and authoritative Recording Payloads must use disjoint paths");
        }
        let claimed: bool = tx.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM library_cache_blob_refs r
                INNER JOIN library_cache_generations g
                  ON g.source_hub_id=r.source_hub_id AND g.generation_id=r.generation_id
                WHERE r.source_hub_id=?1 AND r.checksum=?2
                  AND r.byte_size=?3 AND r.media_type=?4
                  AND r.availability='available'
                  AND g.cache_level='text_and_available_audio'
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
                (source_hub_id,checksum,local_path,byte_size,media_type,verified,cleanup_pending)
             VALUES(?1,?2,?3,?4,?5,1,0)
             ON CONFLICT(source_hub_id,checksum) DO UPDATE SET
                 local_path=excluded.local_path,byte_size=excluded.byte_size,
                 media_type=excluded.media_type,verified=1,cleanup_pending=0,
                 updated_at=CURRENT_TIMESTAMP
             WHERE library_cache_blobs.byte_size=excluded.byte_size
               AND library_cache_blobs.media_type=excluded.media_type
               AND library_cache_blobs.local_path=excluded.local_path",
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
        tx.execute(
            "DELETE FROM library_cache_blob_cleanup
             WHERE source_hub_id=?1 AND checksum=?2",
            params![blob.source_hub_id.to_string(), blob.checksum],
        )?;
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
             WHERE source_hub_id=?1 AND checksum=?2 AND verified=1 AND cleanup_pending=0",
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
    ) -> Result<()> {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
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
        enqueue_orphaned_blobs(&tx)?;
        tx.commit()
            .context("committing incomplete cache abandonment")?;
        Ok(())
    }

    /// Process durable cache-blob cleanup while the SQLite write lock prevents
    /// registrations from racing the final ownership check and unlink.
    pub fn process_pending_blob_cleanups(conn: &mut Connection) -> Result<()> {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("starting pending cache blob cleanup")?;
        let pending = {
            let mut statement = tx.prepare(
                "SELECT source_hub_id,checksum,local_path
                 FROM library_cache_blob_cleanup
                 ORDER BY created_at,source_hub_id,checksum",
            )?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        let mut first_error = None;
        for (source, checksum, path) in pending {
            let current: Option<(String, bool, bool)> = tx
                .query_row(
                    "SELECT local_path,verified,cleanup_pending
                     FROM library_cache_blobs
                     WHERE source_hub_id=?1 AND checksum=?2",
                    params![source, checksum],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let still_unclaimed: bool = tx.query_row(
                "SELECT NOT EXISTS(
                    SELECT 1 FROM library_cache_blob_refs
                    WHERE source_hub_id=?1 AND checksum=?2
                 )",
                params![source, checksum],
                |row| row.get(0),
            )?;
            let authoritative_owns_path: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM library_blobs WHERE canonical_path=?1)",
                [&path],
                |row| row.get(0),
            )?;

            match current {
                Some(_) if !still_unclaimed => {
                    tx.execute(
                        "UPDATE library_cache_blobs
                         SET cleanup_pending=0,updated_at=CURRENT_TIMESTAMP
                         WHERE source_hub_id=?1 AND checksum=?2",
                        params![source, checksum],
                    )?;
                    tx.execute(
                        "DELETE FROM library_cache_blob_cleanup
                         WHERE source_hub_id=?1 AND checksum=?2",
                        params![source, checksum],
                    )?;
                    continue;
                }
                Some((current_path, verified, cleanup_pending))
                    if current_path == path && !verified && cleanup_pending =>
                {
                    if authoritative_owns_path {
                        tx.execute(
                            "DELETE FROM library_cache_blobs
                             WHERE source_hub_id=?1 AND checksum=?2 AND local_path=?3
                               AND cleanup_pending=1 AND verified=0",
                            params![source, checksum, path],
                        )?;
                        continue;
                    }
                }
                _ => {
                    tx.execute(
                        "DELETE FROM library_cache_blob_cleanup
                         WHERE source_hub_id=?1 AND checksum=?2",
                        params![source, checksum],
                    )?;
                    continue;
                }
            }

            let path_buf = PathBuf::from(&path);
            let unlink = match std::fs::remove_file(&path_buf) {
                Ok(()) => sync_parent_directory(&path_buf),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    sync_parent_directory(&path_buf)
                }
                Err(error) => Err(error).with_context(|| {
                    format!("removing orphaned cache blob {}", path_buf.display())
                }),
            };
            match unlink {
                Ok(()) => {
                    tx.execute(
                        "DELETE FROM library_cache_blobs
                         WHERE source_hub_id=?1 AND checksum=?2
                           AND cleanup_pending=1 AND verified=0",
                        params![source, checksum],
                    )?;
                }
                Err(error) => {
                    let message = error.to_string();
                    tx.execute(
                        "UPDATE library_cache_blob_cleanup
                         SET attempts=attempts+1,last_error=?3,updated_at=CURRENT_TIMESTAMP
                         WHERE source_hub_id=?1 AND checksum=?2",
                        params![source, checksum, message],
                    )?;
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        tx.commit()
            .context("committing pending cache blob cleanup")?;
        if let Some(error) = first_error {
            Err(error)
        } else {
            Ok(())
        }
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
            if let Some(descriptor) = snapshot_payload(snapshot).filter(|descriptor| {
                generation.level == CacheLevel::TextAndAvailableAudio
                    && descriptor.availability == PayloadAvailability::Available
            }) {
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
         WHERE source_hub_id=?1 AND target_cursor=?2 AND after_cursor=?3",
        params![
            source_hub_id.to_string(),
            to_i64(
                page.target_cursor.cursor().value(),
                "duplicate live page target cursor"
            )?,
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

fn verify_generation_blobs(conn: &Connection, generation: &CacheGeneration) -> Result<()> {
    let claims = {
        let mut statement = conn.prepare(
            "SELECT r.checksum,r.byte_size,r.media_type,b.local_path
             FROM library_cache_blob_refs r
             LEFT JOIN library_cache_blobs b
               ON b.source_hub_id=r.source_hub_id
              AND b.checksum=r.checksum
              AND b.byte_size=r.byte_size
              AND b.media_type=r.media_type
              AND b.verified=1
              AND b.cleanup_pending=0
             WHERE r.source_hub_id=?1 AND r.generation_id=?2
               AND r.availability='available'
             ORDER BY r.checksum,r.record_id",
        )?;
        let rows = statement
            .query_map(
                params![generation.source_hub_id.to_string(), generation.id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows
    };
    for (checksum, byte_size, media_type, path) in claims {
        let path = path.context("full-audio generation is missing a verified cache blob")?;
        let byte_size = u64::try_from(byte_size).context("negative cache blob claim size")?;
        validate_blob_metadata(byte_size, &media_type)?;
        let path = Path::new(&path);
        validate_cache_blob_path_parts(generation.source_hub_id, &checksum, path)?;
        verify_blob_file(path, &checksum, byte_size)
            .with_context(|| format!("verifying full-audio cache blob for checksum {checksum}"))?;
    }
    Ok(())
}

fn enqueue_orphaned_blobs(conn: &Transaction<'_>) -> Result<()> {
    let orphans = {
        let mut statement = conn.prepare(
            "SELECT b.source_hub_id,b.checksum,b.local_path
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
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows
    };
    for (source, checksum, path) in orphans {
        conn.execute(
            "UPDATE library_cache_blobs
             SET verified=0,cleanup_pending=1,updated_at=CURRENT_TIMESTAMP
             WHERE source_hub_id=?1 AND checksum=?2",
            params![source, checksum],
        )?;
        conn.execute(
            "INSERT INTO library_cache_blob_cleanup(source_hub_id,checksum,local_path)
             VALUES(?1,?2,?3)
             ON CONFLICT(source_hub_id,checksum) DO UPDATE SET
                local_path=excluded.local_path,updated_at=CURRENT_TIMESTAMP",
            params![source, checksum, path],
        )?;
    }
    Ok(())
}

fn validate_blob_metadata(byte_size: u64, media_type: &str) -> Result<()> {
    if byte_size == 0 || byte_size > MAX_BLOB_BYTES {
        bail!("cache blob size must be between 1 and {MAX_BLOB_BYTES} bytes");
    }
    if media_type.is_empty() || media_type.len() > 255 || media_type.contains(['\r', '\n']) {
        bail!("cache blob media type is invalid");
    }
    Ok(())
}

fn verify_blob_file(
    path: &std::path::Path,
    expected_checksum: &str,
    expected_size: u64,
) -> Result<()> {
    let mut file =
        File::open(path).with_context(|| format!("opening cache blob {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("reading cache blob {}", path.display()))?;
        if read == 0 {
            break;
        }
        size = size
            .checked_add(read as u64)
            .context("cache blob size overflow")?;
        if size > expected_size || size > MAX_BLOB_BYTES {
            bail!("cache blob exceeds its claimed size");
        }
        hasher.update(&buffer[..read]);
    }
    let checksum = format!("{:x}", hasher.finalize());
    if size != expected_size || checksum != expected_checksum {
        bail!(
            "cache blob verification failed: expected {expected_checksum}/{expected_size}, received {checksum}/{size}"
        );
    }
    Ok(())
}

#[cfg(unix)]
fn sync_parent_directory(path: &std::path::Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        match File::open(parent) {
            Ok(directory) => directory
                .sync_all()
                .with_context(|| format!("syncing cache blob directory {}", parent.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("opening cache blob directory {}", parent.display()))
            }
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent_directory(_path: &std::path::Path) -> Result<()> {
    Ok(())
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
            Ok((None, None, None))
        }
        PayloadAvailability::Pending | PayloadAvailability::Available => {
            let checksum = descriptor
                .checksum
                .as_deref()
                .context("available cache payload omitted its checksum")?;
            let byte_size = descriptor
                .byte_size
                .context("available cache payload omitted its byte size")?;
            let media_type = descriptor
                .media_type
                .as_deref()
                .context("available cache payload omitted its media type")?;
            if !is_canonical_sha256(checksum) {
                bail!("cache payload checksum is not canonical SHA-256");
            }
            validate_blob_metadata(byte_size, media_type)?;
            Ok((Some(checksum), Some(byte_size), Some(media_type)))
        }
    }
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

    fn available_dictation_change(
        cursor: u64,
        record_id: RecordId,
        origin: DeviceId,
        revision: u64,
        checksum: &str,
        byte_size: u64,
        media_type: &str,
    ) -> ChangeRecord {
        let mut change = dictation_change(cursor, record_id, origin, revision, "audio");
        let Some(Snapshot::Dictation(snapshot)) = change.snapshot.as_mut() else {
            unreachable!("dictation helper always returns a dictation snapshot");
        };
        snapshot.payload.recording_payload = RecordingPayloadDescriptor {
            checksum: Some(checksum.to_owned()),
            byte_size: Some(byte_size),
            media_type: Some(media_type.to_owned()),
            availability: PayloadAvailability::Available,
        };
        change
    }

    fn dictation_deletion(cursor: u64, record_id: RecordId, revision: u64) -> ChangeRecord {
        ChangeRecord {
            cursor: ChangeCursor::new(cursor),
            operation: ChangeOperation::Delete,
            kind: RecordKind::Dictation,
            record_id,
            origin_device_id: None,
            authoritative_revision: revision,
            snapshot: None,
            changed_at: format!("2026-09-05T12:00:{revision:02}Z"),
        }
    }

    fn complete_full_audio_generation(
        conn: &mut Connection,
        source: HubId,
        record_id: RecordId,
        origin: DeviceId,
        checksum: &str,
        byte_size: u64,
        media_type: &str,
    ) -> i64 {
        let generation = LibraryCacheStore::begin_generation(
            conn,
            source,
            CacheLevel::TextAndAvailableAudio,
            ChangeCursor::ZERO,
            ChangeTarget::new(ChangeCursor::new(1)),
        )
        .unwrap();
        LibraryCacheStore::apply_validated_page(
            conn,
            source,
            generation,
            &page(
                0,
                1,
                vec![available_dictation_change(
                    1, record_id, origin, 1, checksum, byte_size, media_type,
                )],
            ),
        )
        .unwrap();
        generation
    }

    fn orphan_verified_cache_blob(
        conn: &mut Connection,
        source: HubId,
        origin: DeviceId,
        checksum: &str,
        blob_path: &std::path::Path,
        byte_size: u64,
        media_type: &str,
    ) {
        let record_id = RecordId::new();
        let first_generation = complete_full_audio_generation(
            conn, source, record_id, origin, checksum, byte_size, media_type,
        );
        LibraryCacheStore::register_verified_blob(
            conn,
            &VerifiedCacheBlob {
                source_hub_id: source,
                checksum: checksum.to_owned(),
                local_path: blob_path.to_path_buf(),
                byte_size,
                media_type: media_type.to_owned(),
            },
        )
        .unwrap();
        LibraryCacheStore::activate_complete_generation(conn, source, first_generation).unwrap();

        let second_generation = LibraryCacheStore::begin_generation(
            conn,
            source,
            CacheLevel::TextAndAvailableAudio,
            ChangeCursor::new(1),
            ChangeTarget::new(ChangeCursor::new(2)),
        )
        .unwrap();
        LibraryCacheStore::apply_validated_page(
            conn,
            source,
            second_generation,
            &page(1, 2, vec![dictation_deletion(2, record_id, 2)]),
        )
        .unwrap();
        LibraryCacheStore::activate_complete_generation(conn, source, second_generation).unwrap();
    }

    fn insert_cache_blob_row(
        conn: &Connection,
        source: HubId,
        checksum: &str,
        path: &std::path::Path,
        byte_size: u64,
        media_type: &str,
    ) {
        conn.execute(
            "INSERT OR IGNORE INTO library_cache_sources(source_hub_id) VALUES(?1)",
            [source.to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO library_cache_blobs
                (source_hub_id,checksum,local_path,byte_size,media_type,verified)
             VALUES(?1,?2,?3,?4,?5,1)",
            params![
                source.to_string(),
                checksum,
                path.to_string_lossy(),
                i64::try_from(byte_size).unwrap(),
                media_type,
            ],
        )
        .unwrap();
    }

    fn namespaced_blob_path(temp: &tempfile::TempDir, source: HubId, checksum: &str) -> PathBuf {
        let path = cache_blob_path_for_db(&temp.path().join("cache.db"), source, checksum).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        path
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
        let temp = tempfile::tempdir().unwrap();
        let bytes = b"claimed-audio";
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let source = HubId::new();
        let other_source = HubId::new();
        let record_id = RecordId::new();
        let checksum = format!("{:x}", Sha256::digest(bytes));
        let path = namespaced_blob_path(&temp, source, &checksum);
        std::fs::write(&path, bytes).unwrap();
        let change = available_dictation_change(
            1,
            record_id,
            DeviceId::new(),
            1,
            &checksum,
            bytes.len() as u64,
            "audio/wav",
        );
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

        assert!(LibraryCacheStore::register_verified_blob(
            &mut conn,
            &VerifiedCacheBlob {
                source_hub_id: other_source,
                checksum: checksum.clone(),
                local_path: path.clone(),
                byte_size: bytes.len() as u64,
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
                byte_size: bytes.len() as u64,
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

    #[tokio::test]
    async fn cache_blob_publication_is_verified_and_uses_a_disjoint_namespace() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("cache.db");
        let source_path = temp.path().join("downloaded-recording");
        let bytes = b"atomically published cache audio";
        std::fs::write(&source_path, bytes).unwrap();
        let checksum = format!("{:x}", Sha256::digest(bytes));
        let source = HubId::new();
        let mut conn = crate::db::migrate_db_at(&db_path).unwrap();
        let generation = complete_full_audio_generation(
            &mut conn,
            source,
            RecordId::new(),
            DeviceId::new(),
            &checksum,
            bytes.len() as u64,
            "audio/wav",
        );

        let blob = VerifiedCacheBlob::publish_for_db(
            &db_path,
            source,
            &source_path,
            checksum.clone(),
            bytes.len() as u64,
            "audio/wav".into(),
        )
        .await
        .unwrap();
        assert_eq!(
            blob.local_path,
            cache_blob_path_for_db(&db_path, source, &checksum).unwrap()
        );
        assert_ne!(
            blob.local_path,
            crate::sync::payload::BlobStore::for_db(&db_path)
                .canonical_path(&checksum)
                .unwrap()
        );

        LibraryCacheStore::register_verified_blob(&mut conn, &blob).unwrap();
        LibraryCacheStore::activate_complete_generation(&mut conn, source, generation).unwrap();
    }

    #[test]
    fn pending_blob_cleanup_survives_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("cache.db");
        let bytes = b"durable-cache-audio";
        let checksum = format!("{:x}", Sha256::digest(bytes));
        let source = HubId::new();
        let blob_path = namespaced_blob_path(&temp, source, &checksum);
        std::fs::write(&blob_path, bytes).unwrap();

        {
            let mut conn = crate::db::migrate_db_at(&db_path).unwrap();
            orphan_verified_cache_blob(
                &mut conn,
                source,
                DeviceId::new(),
                &checksum,
                &blob_path,
                bytes.len() as u64,
                "audio/wav",
            );
            assert!(blob_path.is_file());
        }

        let mut reopened = crate::db::open_db_at(&db_path).unwrap();
        LibraryCacheStore::process_pending_blob_cleanups(&mut reopened).unwrap();
        assert!(!blob_path.exists());
    }

    #[test]
    fn pending_blob_cleanup_retries_after_the_file_was_already_unlinked() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("cache.db");
        let bytes = b"retry-cache-audio";
        let checksum = format!("{:x}", Sha256::digest(bytes));
        let source = HubId::new();
        let blob_path = namespaced_blob_path(&temp, source, &checksum);
        std::fs::write(&blob_path, bytes).unwrap();

        {
            let mut conn = crate::db::migrate_db_at(&db_path).unwrap();
            orphan_verified_cache_blob(
                &mut conn,
                source,
                DeviceId::new(),
                &checksum,
                &blob_path,
                bytes.len() as u64,
                "audio/wav",
            );
        }
        std::fs::remove_file(&blob_path).unwrap();

        let mut reopened = crate::db::open_db_at(&db_path).unwrap();
        LibraryCacheStore::process_pending_blob_cleanups(&mut reopened).unwrap();
        LibraryCacheStore::process_pending_blob_cleanups(&mut reopened).unwrap();
        assert!(!blob_path.exists());
    }

    #[test]
    fn failed_blob_cleanup_retains_path_ownership_and_retries() {
        let temp = tempfile::tempdir().unwrap();
        let bytes = b"retry-after-failed-unlink";
        let checksum = format!("{:x}", Sha256::digest(bytes));
        let source = HubId::new();
        let blob_path = namespaced_blob_path(&temp, source, &checksum);
        std::fs::write(&blob_path, bytes).unwrap();
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        orphan_verified_cache_blob(
            &mut conn,
            source,
            DeviceId::new(),
            &checksum,
            &blob_path,
            bytes.len() as u64,
            "audio/wav",
        );

        std::fs::remove_file(&blob_path).unwrap();
        std::fs::create_dir(&blob_path).unwrap();
        assert!(LibraryCacheStore::process_pending_blob_cleanups(&mut conn).is_err());
        assert_eq!(
            conn.query_row(
                "SELECT cleanup_pending FROM library_cache_blobs
                 WHERE source_hub_id=?1 AND checksum=?2",
                params![source.to_string(), checksum],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row(
                "SELECT attempts FROM library_cache_blob_cleanup
                 WHERE source_hub_id=?1 AND checksum=?2",
                params![source.to_string(), checksum],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            1
        );

        std::fs::remove_dir(&blob_path).unwrap();
        std::fs::write(&blob_path, bytes).unwrap();
        LibraryCacheStore::process_pending_blob_cleanups(&mut conn).unwrap();
        assert!(!blob_path.exists());
        assert!(LibraryCacheStore::verified_blob(&conn, source, &checksum)
            .unwrap()
            .is_none());
    }

    #[test]
    fn stale_cleanup_cannot_delete_reintroduced_cache_ownership() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("cache.db");
        let bytes = b"reintroduced-audio";
        let checksum = format!("{:x}", Sha256::digest(bytes));
        let source = HubId::new();
        let blob_path = namespaced_blob_path(&temp, source, &checksum);
        std::fs::write(&blob_path, bytes).unwrap();
        let origin = DeviceId::new();
        let mut conn = crate::db::migrate_db_at(&db_path).unwrap();

        orphan_verified_cache_blob(
            &mut conn,
            source,
            origin,
            &checksum,
            &blob_path,
            bytes.len() as u64,
            "audio/wav",
        );

        let replacement_id = RecordId::new();
        let replacement_generation = LibraryCacheStore::begin_generation(
            &mut conn,
            source,
            CacheLevel::TextAndAvailableAudio,
            ChangeCursor::new(2),
            ChangeTarget::new(ChangeCursor::new(3)),
        )
        .unwrap();
        LibraryCacheStore::apply_validated_page(
            &mut conn,
            source,
            replacement_generation,
            &page(
                2,
                3,
                vec![available_dictation_change(
                    3,
                    replacement_id,
                    origin,
                    1,
                    &checksum,
                    bytes.len() as u64,
                    "audio/wav",
                )],
            ),
        )
        .unwrap();
        LibraryCacheStore::register_verified_blob(
            &mut conn,
            &VerifiedCacheBlob {
                source_hub_id: source,
                checksum: checksum.clone(),
                local_path: blob_path.clone(),
                byte_size: bytes.len() as u64,
                media_type: "audio/wav".into(),
            },
        )
        .unwrap();
        LibraryCacheStore::activate_complete_generation(&mut conn, source, replacement_generation)
            .unwrap();

        conn.execute(
            "INSERT INTO library_cache_blob_cleanup(source_hub_id,checksum,local_path)
             VALUES(?1,?2,?3)",
            params![source.to_string(), checksum, blob_path.to_string_lossy()],
        )
        .unwrap();

        LibraryCacheStore::process_pending_blob_cleanups(&mut conn).unwrap();
        assert!(blob_path.is_file());
        assert!(LibraryCacheStore::verified_blob(&conn, source, &checksum)
            .unwrap()
            .is_some());
    }

    #[test]
    fn authoritative_and_cache_blob_ownership_cannot_share_a_path() {
        let temp = tempfile::tempdir().unwrap();
        let bytes = b"disjoint-ownership";
        let checksum = format!("{:x}", Sha256::digest(bytes));
        let source = HubId::new();
        let path = namespaced_blob_path(&temp, source, &checksum);
        std::fs::write(&path, bytes).unwrap();
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        complete_full_audio_generation(
            &mut conn,
            source,
            RecordId::new(),
            DeviceId::new(),
            &checksum,
            bytes.len() as u64,
            "audio/wav",
        );
        conn.execute(
            "INSERT INTO library_blobs
                (checksum,canonical_path,byte_size,media_type,verified)
             VALUES(?1,?2,?3,?4,1)",
            params![
                checksum,
                path.to_string_lossy(),
                bytes.len() as i64,
                "audio/wav",
            ],
        )
        .unwrap();

        assert!(LibraryCacheStore::register_verified_blob(
            &mut conn,
            &VerifiedCacheBlob {
                source_hub_id: source,
                checksum,
                local_path: path,
                byte_size: bytes.len() as u64,
                media_type: "audio/wav".into(),
            },
        )
        .is_err());
    }

    #[test]
    fn full_audio_activation_requires_exact_verified_blob_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let bytes = b"exact-cache-audio";
        let checksum = format!("{:x}", Sha256::digest(bytes));
        let source = HubId::new();
        let other_source = HubId::new();
        let path = namespaced_blob_path(&temp, source, &checksum);
        std::fs::write(&path, bytes).unwrap();
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let generation = complete_full_audio_generation(
            &mut conn,
            source,
            RecordId::new(),
            DeviceId::new(),
            &checksum,
            bytes.len() as u64,
            "audio/wav",
        );

        assert!(
            LibraryCacheStore::activate_complete_generation(&mut conn, source, generation).is_err()
        );

        insert_cache_blob_row(
            &conn,
            other_source,
            &checksum,
            &path,
            bytes.len() as u64,
            "audio/wav",
        );
        assert!(
            LibraryCacheStore::activate_complete_generation(&mut conn, source, generation).is_err()
        );
        conn.execute("DELETE FROM library_cache_blobs", []).unwrap();

        insert_cache_blob_row(
            &conn,
            source,
            &"b".repeat(64),
            &path,
            bytes.len() as u64,
            "audio/wav",
        );
        assert!(
            LibraryCacheStore::activate_complete_generation(&mut conn, source, generation).is_err()
        );
        conn.execute("DELETE FROM library_cache_blobs", []).unwrap();

        insert_cache_blob_row(
            &conn,
            source,
            &checksum,
            &path,
            bytes.len() as u64 + 1,
            "audio/wav",
        );
        assert!(
            LibraryCacheStore::activate_complete_generation(&mut conn, source, generation).is_err()
        );
        conn.execute("DELETE FROM library_cache_blobs", []).unwrap();

        insert_cache_blob_row(
            &conn,
            source,
            &checksum,
            &path,
            bytes.len() as u64,
            "audio/mpeg",
        );
        assert!(
            LibraryCacheStore::activate_complete_generation(&mut conn, source, generation).is_err()
        );
        conn.execute("DELETE FROM library_cache_blobs", []).unwrap();

        std::fs::write(&path, vec![b'x'; bytes.len()]).unwrap();
        insert_cache_blob_row(
            &conn,
            source,
            &checksum,
            &path,
            bytes.len() as u64,
            "audio/wav",
        );
        assert!(
            LibraryCacheStore::activate_complete_generation(&mut conn, source, generation).is_err()
        );
        conn.execute("DELETE FROM library_cache_blobs", []).unwrap();

        std::fs::write(&path, bytes).unwrap();
        insert_cache_blob_row(
            &conn,
            source,
            &checksum,
            &path,
            bytes.len() as u64,
            "audio/wav",
        );
        LibraryCacheStore::activate_complete_generation(&mut conn, source, generation).unwrap();
    }

    #[test]
    fn verified_blob_registration_rejects_actual_file_mismatch() {
        let temp = tempfile::tempdir().unwrap();
        let expected = b"cache-audio";
        let wrong = b"broken-data";
        assert_eq!(expected.len(), wrong.len());
        let checksum = format!("{:x}", Sha256::digest(expected));
        let source = HubId::new();
        let wrong_path = namespaced_blob_path(&temp, source, &checksum);
        std::fs::write(&wrong_path, wrong).unwrap();
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        complete_full_audio_generation(
            &mut conn,
            source,
            RecordId::new(),
            DeviceId::new(),
            &checksum,
            expected.len() as u64,
            "audio/wav",
        );

        assert!(LibraryCacheStore::register_verified_blob(
            &mut conn,
            &VerifiedCacheBlob {
                source_hub_id: source,
                checksum,
                local_path: wrong_path,
                byte_size: expected.len() as u64,
                media_type: "audio/wav".into(),
            },
        )
        .is_err());
    }

    #[test]
    fn failed_full_audio_activation_preserves_the_active_generation_and_cursor() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let source = HubId::new();
        let origin = DeviceId::new();
        let baseline_record = RecordId::new();
        let baseline = LibraryCacheStore::begin_generation(
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
            baseline,
            &page(
                0,
                1,
                vec![dictation_change(1, baseline_record, origin, 1, "baseline")],
            ),
        )
        .unwrap();
        LibraryCacheStore::activate_complete_generation(&mut conn, source, baseline).unwrap();

        let replacement = LibraryCacheStore::begin_generation(
            &mut conn,
            source,
            CacheLevel::TextAndAvailableAudio,
            ChangeCursor::new(1),
            ChangeTarget::new(ChangeCursor::new(2)),
        )
        .unwrap();
        LibraryCacheStore::apply_validated_page(
            &mut conn,
            source,
            replacement,
            &page(
                1,
                2,
                vec![available_dictation_change(
                    2,
                    RecordId::new(),
                    origin,
                    1,
                    &"a".repeat(64),
                    10,
                    "audio/wav",
                )],
            ),
        )
        .unwrap();
        assert!(
            LibraryCacheStore::activate_complete_generation(&mut conn, source, replacement)
                .is_err()
        );

        assert_eq!(
            LibraryCacheStore::active_generation(&conn, source)
                .unwrap()
                .unwrap()
                .id,
            baseline
        );
        assert_eq!(
            LibraryCacheStore::source_cursor(&conn, source).unwrap(),
            ChangeCursor::new(1)
        );
        assert_eq!(
            LibraryCacheStore::active_items(&conn, source)
                .unwrap()
                .iter()
                .map(|item| item.record_id)
                .collect::<Vec<_>>(),
            vec![baseline_record]
        );
    }

    #[test]
    fn text_only_generation_has_no_blob_claims() {
        let bytes = b"text-only-audio";
        let checksum = format!("{:x}", Sha256::digest(bytes));
        let source = HubId::new();
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let generation = LibraryCacheStore::begin_generation(
            &mut conn,
            source,
            CacheLevel::TextForOfflineUse,
            ChangeCursor::ZERO,
            ChangeTarget::new(ChangeCursor::new(1)),
        )
        .unwrap();
        LibraryCacheStore::apply_validated_page(
            &mut conn,
            source,
            generation,
            &page(
                0,
                1,
                vec![available_dictation_change(
                    1,
                    RecordId::new(),
                    DeviceId::new(),
                    1,
                    &checksum,
                    bytes.len() as u64,
                    "audio/wav",
                )],
            ),
        )
        .unwrap();

        assert!(LibraryCacheStore::blob_claims(&conn, source, generation)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn full_audio_generation_does_not_claim_pending_or_unavailable_payloads() {
        let source = HubId::new();
        let origin = DeviceId::new();
        let mut pending = dictation_change(1, RecordId::new(), origin, 1, "pending audio");
        let Some(Snapshot::Dictation(snapshot)) = pending.snapshot.as_mut() else {
            unreachable!("dictation helper always returns a dictation snapshot");
        };
        snapshot.payload.recording_payload =
            RecordingPayloadDescriptor::pending("a".repeat(64), 10, "audio/wav".into());
        let unavailable = dictation_change(2, RecordId::new(), origin, 1, "unavailable audio");
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let generation = LibraryCacheStore::begin_generation(
            &mut conn,
            source,
            CacheLevel::TextAndAvailableAudio,
            ChangeCursor::ZERO,
            ChangeTarget::new(ChangeCursor::new(2)),
        )
        .unwrap();
        LibraryCacheStore::apply_validated_page(
            &mut conn,
            source,
            generation,
            &page(0, 2, vec![pending, unavailable]),
        )
        .unwrap();

        assert!(LibraryCacheStore::blob_claims(&conn, source, generation)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn cache_payload_descriptors_match_authoritative_size_and_media_limits() {
        let source = HubId::new();
        let origin = DeviceId::new();
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let generation = LibraryCacheStore::begin_generation(
            &mut conn,
            source,
            CacheLevel::TextForOfflineUse,
            ChangeCursor::ZERO,
            ChangeTarget::new(ChangeCursor::new(1)),
        )
        .unwrap();

        let invalid_page = |size, media_type: String| {
            let mut change = dictation_change(1, RecordId::new(), origin, 1, "invalid payload");
            let Some(Snapshot::Dictation(snapshot)) = change.snapshot.as_mut() else {
                unreachable!("dictation helper always returns a dictation snapshot");
            };
            snapshot.payload.recording_payload =
                RecordingPayloadDescriptor::pending("a".repeat(64), size, media_type);
            page(0, 1, vec![change])
        };

        assert!(LibraryCacheStore::apply_validated_page(
            &mut conn,
            source,
            generation,
            &invalid_page(MAX_BLOB_BYTES + 1, "audio/wav".into()),
        )
        .is_err());
        assert!(LibraryCacheStore::apply_validated_page(
            &mut conn,
            source,
            generation,
            &invalid_page(1, "é".repeat(128)),
        )
        .is_err());
        assert!(LibraryCacheStore::apply_validated_page(
            &mut conn,
            source,
            generation,
            &invalid_page(1, "audio/wav\r\nforged".into()),
        )
        .is_err());
    }

    #[test]
    fn a_source_allows_only_one_inactive_generation() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let source = HubId::new();
        LibraryCacheStore::begin_generation(
            &mut conn,
            source,
            CacheLevel::TextForOfflineUse,
            ChangeCursor::ZERO,
            ChangeTarget::new(ChangeCursor::new(1)),
        )
        .unwrap();

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
    fn incremental_generation_requires_the_active_cache_level() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let source = HubId::new();
        let generation = LibraryCacheStore::begin_generation(
            &mut conn,
            source,
            CacheLevel::TextForOfflineUse,
            ChangeCursor::ZERO,
            ChangeTarget::new(ChangeCursor::new(1)),
        )
        .unwrap();
        LibraryCacheStore::apply_validated_page(
            &mut conn,
            source,
            generation,
            &page(
                0,
                1,
                vec![dictation_change(
                    1,
                    RecordId::new(),
                    DeviceId::new(),
                    1,
                    "text baseline",
                )],
            ),
        )
        .unwrap();
        LibraryCacheStore::activate_complete_generation(&mut conn, source, generation).unwrap();

        assert!(LibraryCacheStore::begin_generation(
            &mut conn,
            source,
            CacheLevel::TextAndAvailableAudio,
            ChangeCursor::new(1),
            ChangeTarget::new(ChangeCursor::new(2)),
        )
        .is_err());
    }

    #[test]
    fn full_generation_supersedes_stale_live_only_traversal_after_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("cache.db");
        let source = HubId::new();
        {
            let mut conn = crate::db::migrate_db_at(&db_path).unwrap();
            LibraryCacheStore::apply_live_only_page(
                &mut conn,
                source,
                &page(
                    0,
                    2,
                    vec![dictation_change(
                        1,
                        RecordId::new(),
                        DeviceId::new(),
                        1,
                        "live traversal",
                    )],
                ),
            )
            .unwrap();
        }

        let mut reopened = crate::db::open_db_at(&db_path).unwrap();
        assert_eq!(
            LibraryCacheStore::live_traversal_target(&reopened, source).unwrap(),
            Some(ChangeTarget::new(ChangeCursor::new(2)))
        );
        LibraryCacheStore::begin_generation(
            &mut reopened,
            source,
            CacheLevel::TextForOfflineUse,
            ChangeCursor::ZERO,
            ChangeTarget::new(ChangeCursor::new(2)),
        )
        .unwrap();
        assert_eq!(
            LibraryCacheStore::live_traversal_target(&reopened, source).unwrap(),
            None
        );
        assert!(LibraryCacheStore::apply_live_only_page(
            &mut reopened,
            source,
            &page(
                1,
                2,
                vec![dictation_change(
                    2,
                    RecordId::new(),
                    DeviceId::new(),
                    1,
                    "stale live worker",
                )],
            ),
        )
        .is_err());
    }

    #[test]
    fn full_generation_blocks_live_only_traversal_after_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("cache.db");
        let source = HubId::new();
        {
            let mut conn = crate::db::migrate_db_at(&db_path).unwrap();
            LibraryCacheStore::begin_generation(
                &mut conn,
                source,
                CacheLevel::TextForOfflineUse,
                ChangeCursor::ZERO,
                ChangeTarget::new(ChangeCursor::new(1)),
            )
            .unwrap();
        }

        let mut reopened = crate::db::open_db_at(&db_path).unwrap();
        assert!(LibraryCacheStore::apply_live_only_page(
            &mut reopened,
            source,
            &page(
                0,
                1,
                vec![dictation_change(
                    1,
                    RecordId::new(),
                    DeviceId::new(),
                    1,
                    "full traversal",
                )],
            ),
        )
        .is_err());
    }

    #[test]
    fn repeated_live_only_idle_pages_allow_later_progress() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let source = HubId::new();
        let idle = page(0, 0, Vec::new());

        assert_eq!(
            LibraryCacheStore::apply_live_only_page(&mut conn, source, &idle).unwrap(),
            ApplyPageOutcome::Applied
        );
        assert_eq!(
            LibraryCacheStore::apply_live_only_page(&mut conn, source, &idle).unwrap(),
            ApplyPageOutcome::Duplicate
        );

        let progress = page(
            0,
            1,
            vec![dictation_change(
                1,
                RecordId::new(),
                DeviceId::new(),
                1,
                "progress after idle",
            )],
        );
        assert_eq!(
            LibraryCacheStore::apply_live_only_page(&mut conn, source, &progress).unwrap(),
            ApplyPageOutcome::Applied
        );
        assert_eq!(
            LibraryCacheStore::source_cursor(&conn, source).unwrap(),
            ChangeCursor::new(1)
        );
        assert_eq!(
            LibraryCacheStore::apply_live_only_page(&mut conn, source, &idle).unwrap(),
            ApplyPageOutcome::Duplicate
        );
    }
}
