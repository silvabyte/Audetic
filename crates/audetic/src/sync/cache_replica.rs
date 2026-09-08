//! Bounded, source-scoped Library Cache refresh worker.

use anyhow::{anyhow, bail, Context, Result};
use audetic_core::sync::{CacheLevel, HubConnection, HubId, PayloadAvailability};
use tokio_util::sync::CancellationToken;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::db::library_cache::{
    CacheBlobClaim, CacheGeneration, LibraryCacheStore, VerifiedCacheBlob,
};

use super::clock::{SyncClock, SystemSyncClock};
use super::observer::{NoopWorkerObserver, WorkerEvent, WorkerObserver};
use super::protocol::{ChangeCursor, ChangePage, ChangeTarget, RecordKind, MAX_CHANGE_PAGE};
use super::transport::{
    HubChangeSource, HubTransferError, RemotePayloadSource, StreamingPayloadResponse,
};

const CACHE_POLL_INTERVAL: Duration = Duration::from_secs(1);

pub(crate) struct CacheReplica {
    db_path: PathBuf,
    hub: HubConnection,
    level: CacheLevel,
    role_epoch: u64,
    changes: Arc<dyn HubChangeSource>,
    payloads: Arc<dyn RemotePayloadSource>,
    clock: Arc<dyn SyncClock>,
    observer: Arc<dyn WorkerObserver>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RefreshOutcome {
    Completed,
    Cancelled,
}

#[derive(Debug)]
struct BlobClaimGroup {
    checksum: String,
    byte_size: u64,
    representatives: Vec<PayloadRepresentative>,
}

#[derive(Debug)]
struct PayloadRepresentative {
    record_id: audetic_core::sync::RecordId,
    kind: RecordKind,
    media_type: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PayloadOutcome {
    Registered,
    SnapshotAdvanced,
    Cancelled,
}

impl CacheReplica {
    pub(crate) fn new(
        db_path: PathBuf,
        hub: HubConnection,
        level: CacheLevel,
        changes: Arc<dyn HubChangeSource>,
        payloads: Arc<dyn RemotePayloadSource>,
    ) -> Self {
        Self {
            db_path,
            hub,
            level,
            role_epoch: 0,
            changes,
            payloads,
            clock: Arc::new(SystemSyncClock),
            observer: Arc::new(NoopWorkerObserver),
        }
    }

    pub(crate) fn with_role_epoch(mut self, role_epoch: u64) -> Self {
        self.role_epoch = role_epoch;
        self
    }

    pub(crate) fn with_clock(mut self, clock: Arc<dyn SyncClock>) -> Self {
        self.clock = clock;
        self
    }

    pub(crate) fn with_observer(mut self, observer: Arc<dyn WorkerObserver>) -> Self {
        self.observer = observer;
        self
    }

    #[cfg(test)]
    pub(crate) async fn process_once(&self) -> Result<()> {
        self.process_once_cancellable(&CancellationToken::new())
            .await
    }

    pub(crate) async fn run(self, cancellation: CancellationToken) {
        self.observer.observe(WorkerEvent::CacheReplicaStarted {
            role_epoch: self.role_epoch,
        });
        while !cancellation.is_cancelled() {
            self.observer
                .observe(WorkerEvent::CacheReplicaCycleStarted {
                    role_epoch: self.role_epoch,
                });
            match self.process_once_cancellable(&cancellation).await {
                Ok(()) if !cancellation.is_cancelled() => {
                    self.observer
                        .observe(WorkerEvent::CacheReplicaCycleSucceeded {
                            role_epoch: self.role_epoch,
                        });
                }
                Err(error) if !cancellation.is_cancelled() => {
                    tracing::warn!(
                        %error,
                        role_epoch = self.role_epoch,
                        "Library Cache refresh cycle failed"
                    );
                    self.observer.observe(WorkerEvent::CacheReplicaCycleFailed {
                        role_epoch: self.role_epoch,
                        error: error.to_string(),
                    });
                }
                Ok(()) | Err(_) => break,
            }
            self.clock.sleep(CACHE_POLL_INTERVAL, &cancellation).await;
        }
        self.observer.observe(WorkerEvent::CacheReplicaStopped {
            role_epoch: self.role_epoch,
        });
    }

    pub(crate) async fn process_once_cancellable(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        if cancellation.is_cancelled() {
            return Ok(());
        }

        let mut failures = Vec::new();
        if let Err(error) = self.process_pending_blob_cleanups() {
            failures.push(format!("pre-refresh cache blob cleanup failed: {error}"));
        }
        if cancellation.is_cancelled() {
            return Ok(());
        }

        let refresh = match self.level {
            CacheLevel::LiveOnly => self.refresh_live_only(cancellation).await,
            CacheLevel::TextForOfflineUse | CacheLevel::TextAndAvailableAudio => {
                self.refresh_generation(cancellation).await
            }
        };
        match refresh {
            Ok(RefreshOutcome::Cancelled) => return Ok(()),
            Ok(RefreshOutcome::Completed) => {}
            Err(error) => failures.push(format!("Library Cache refresh failed: {error}")),
        }

        if cancellation.is_cancelled() {
            return Ok(());
        }
        if let Err(error) = self.process_pending_blob_cleanups() {
            failures.push(format!("post-refresh cache blob cleanup failed: {error}"));
        }
        if failures.is_empty() {
            Ok(())
        } else {
            bail!(failures.join("; "))
        }
    }

    fn process_pending_blob_cleanups(&self) -> Result<()> {
        Self::cleanup_cache_files(&self.db_path, self.hub.hub_id)
    }

    pub(crate) fn cleanup_cache_files(db_path: &std::path::Path, source: HubId) -> Result<()> {
        let mut connection = crate::db::open_db_at(db_path)?;
        let pending = LibraryCacheStore::process_pending_blob_cleanups(&mut connection);
        let untracked =
            LibraryCacheStore::remove_untracked_blob_files(&connection, db_path, source);
        match (pending, untracked) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(pending), Err(untracked)) => {
                bail!("pending cache cleanup failed: {pending}; untracked cache cleanup failed: {untracked}")
            }
        }
    }

    async fn refresh_live_only(&self, cancellation: &CancellationToken) -> Result<RefreshOutcome> {
        let source = self.hub.hub_id;
        {
            let mut connection = crate::db::open_db_at(&self.db_path)?;
            LibraryCacheStore::transition_to_live_only(&mut connection, source)?;
        }
        if cancellation.is_cancelled() {
            return Ok(RefreshOutcome::Cancelled);
        }

        loop {
            let (after, target) = {
                let connection = crate::db::open_db_at(&self.db_path)?;
                (
                    LibraryCacheStore::source_cursor(&connection, source)?,
                    LibraryCacheStore::live_traversal_target(&connection, source)?,
                )
            };
            let page = match self.page_changes(after, target, cancellation).await? {
                Some(page) => page,
                None => return Ok(RefreshOutcome::Cancelled),
            };
            if cancellation.is_cancelled() {
                return Ok(RefreshOutcome::Cancelled);
            }
            let complete = page.complete;
            let mut connection = crate::db::open_db_at(&self.db_path)?;
            LibraryCacheStore::apply_live_only_page(&mut connection, source, &page)?;
            if complete {
                return Ok(RefreshOutcome::Completed);
            }
            if cancellation.is_cancelled() {
                return Ok(RefreshOutcome::Cancelled);
            }
        }
    }

    async fn refresh_generation(&self, cancellation: &CancellationToken) -> Result<RefreshOutcome> {
        let source = self.hub.hub_id;
        let mut generation = {
            let connection = crate::db::open_db_at(&self.db_path)?;
            LibraryCacheStore::inactive_generation(&connection, source)?
        };
        if let Some(existing) = generation
            .as_ref()
            .filter(|value| value.level != self.level)
        {
            let mut connection = crate::db::open_db_at(&self.db_path)?;
            LibraryCacheStore::discard_inactive_generation(&mut connection, source, existing.id)?;
            generation = None;
        }
        if cancellation.is_cancelled() {
            return Ok(RefreshOutcome::Cancelled);
        }

        if generation.is_none() {
            let (active, source_cursor) = {
                let connection = crate::db::open_db_at(&self.db_path)?;
                (
                    LibraryCacheStore::active_generation(&connection, source)?,
                    LibraryCacheStore::source_cursor(&connection, source)?,
                )
            };
            let exact_baseline = active.as_ref().is_some_and(|value| {
                value.level == self.level
                    && value.complete
                    && value.applied_cursor == source_cursor
                    && value.target_cursor.cursor() == source_cursor
            });
            let start = if exact_baseline {
                source_cursor
            } else {
                ChangeCursor::ZERO
            };
            let first_page = match self.page_changes(start, None, cancellation).await? {
                Some(page) => page,
                None => return Ok(RefreshOutcome::Cancelled),
            };
            if exact_baseline
                && first_page.complete
                && first_page.target_cursor.cursor() == start
                && first_page.after_cursor == start
                && first_page.through_cursor == start
                && first_page.changes.is_empty()
            {
                return Ok(RefreshOutcome::Completed);
            }
            if cancellation.is_cancelled() {
                return Ok(RefreshOutcome::Cancelled);
            }
            let generation_id = {
                let mut connection = crate::db::open_db_at(&self.db_path)?;
                LibraryCacheStore::begin_generation(
                    &mut connection,
                    source,
                    self.level,
                    start,
                    first_page.target_cursor,
                )?
            };
            if cancellation.is_cancelled() {
                return Ok(RefreshOutcome::Cancelled);
            }
            {
                let mut connection = crate::db::open_db_at(&self.db_path)?;
                LibraryCacheStore::apply_validated_page(
                    &mut connection,
                    source,
                    generation_id,
                    &first_page,
                )?;
                generation = LibraryCacheStore::generation(&connection, source, generation_id)?;
            }
        }

        let mut generation = generation.context("inactive cache generation disappeared")?;
        while !generation.complete {
            let page = match self
                .page_changes(
                    generation.applied_cursor,
                    Some(generation.target_cursor),
                    cancellation,
                )
                .await?
            {
                Some(page) => page,
                None => return Ok(RefreshOutcome::Cancelled),
            };
            if cancellation.is_cancelled() {
                return Ok(RefreshOutcome::Cancelled);
            }
            let mut connection = crate::db::open_db_at(&self.db_path)?;
            LibraryCacheStore::apply_validated_page(&mut connection, source, generation.id, &page)?;
            generation = LibraryCacheStore::generation(&connection, source, generation.id)?
                .context("inactive cache generation disappeared after applying a page")?;
        }
        if cancellation.is_cancelled() {
            return Ok(RefreshOutcome::Cancelled);
        }

        if self.level == CacheLevel::TextAndAvailableAudio {
            match self
                .materialize_generation_blobs(&generation, cancellation)
                .await?
            {
                RefreshOutcome::Cancelled => return Ok(RefreshOutcome::Cancelled),
                RefreshOutcome::Completed => {}
            }
            let still_exists = {
                let connection = crate::db::open_db_at(&self.db_path)?;
                LibraryCacheStore::generation(&connection, source, generation.id)?.is_some()
            };
            if !still_exists {
                return Ok(RefreshOutcome::Completed);
            }
        }
        if cancellation.is_cancelled() {
            return Ok(RefreshOutcome::Cancelled);
        }
        let mut connection = crate::db::open_db_at(&self.db_path)?;
        LibraryCacheStore::activate_complete_generation(&mut connection, source, generation.id)?;
        Ok(RefreshOutcome::Completed)
    }

    async fn page_changes(
        &self,
        after: ChangeCursor,
        target: Option<ChangeTarget>,
        cancellation: &CancellationToken,
    ) -> Result<Option<ChangePage>> {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Ok(None),
            response = self.changes.page_changes(&self.hub, after, target, MAX_CHANGE_PAGE) => {
                response.map(Some).map_err(anyhow::Error::new)
            }
        }
    }

    async fn materialize_generation_blobs(
        &self,
        generation: &CacheGeneration,
        cancellation: &CancellationToken,
    ) -> Result<RefreshOutcome> {
        let claims = {
            let connection = crate::db::open_db_at(&self.db_path)?;
            LibraryCacheStore::blob_claims(&connection, generation.source_hub_id, generation.id)?
        };
        let groups = group_blob_claims(claims)?;
        for group in groups {
            if cancellation.is_cancelled() {
                return Ok(RefreshOutcome::Cancelled);
            }
            if self.reuse_registered_blob(generation.source_hub_id, &group)? {
                continue;
            }
            if let Some(blob) = self.recover_unregistered_blob(generation.source_hub_id, &group)? {
                if cancellation.is_cancelled() {
                    return Ok(RefreshOutcome::Cancelled);
                }
                let mut connection = crate::db::open_db_at(&self.db_path)?;
                LibraryCacheStore::register_verified_blob(&mut connection, &blob)?;
                continue;
            }
            match self
                .download_blob(generation.source_hub_id, &group, cancellation)
                .await?
            {
                PayloadOutcome::Registered => {}
                PayloadOutcome::SnapshotAdvanced => {
                    let mut connection = crate::db::open_db_at(&self.db_path)?;
                    LibraryCacheStore::discard_inactive_generation(
                        &mut connection,
                        generation.source_hub_id,
                        generation.id,
                    )?;
                    return Ok(RefreshOutcome::Completed);
                }
                PayloadOutcome::Cancelled => return Ok(RefreshOutcome::Cancelled),
            }
        }
        Ok(RefreshOutcome::Completed)
    }

    fn reuse_registered_blob(
        &self,
        source: audetic_core::sync::HubId,
        group: &BlobClaimGroup,
    ) -> Result<bool> {
        let connection = crate::db::open_db_at(&self.db_path)?;
        let Some(blob) = LibraryCacheStore::verified_blob(&connection, source, &group.checksum)?
        else {
            return Ok(false);
        };
        Ok(blob.has_byte_size(group.byte_size) && blob.verify().is_ok())
    }

    fn recover_unregistered_blob(
        &self,
        source: audetic_core::sync::HubId,
        group: &BlobClaimGroup,
    ) -> Result<Option<VerifiedCacheBlob>> {
        let media_type = group
            .representatives
            .first()
            .context("cache blob group has no representative")?
            .media_type
            .clone();
        let recovered = match VerifiedCacheBlob::recover_published_for_db(
            &self.db_path,
            source,
            group.checksum.clone(),
            group.byte_size,
            media_type,
        ) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(
                    %error,
                    checksum = %group.checksum,
                    "source-scoped cache file needs repair"
                );
                None
            }
        };
        Ok(recovered)
    }

    async fn download_blob(
        &self,
        source: audetic_core::sync::HubId,
        group: &BlobClaimGroup,
        cancellation: &CancellationToken,
    ) -> Result<PayloadOutcome> {
        for representative in &group.representatives {
            let response = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Ok(PayloadOutcome::Cancelled),
                response = self.payloads.stream_payload(
                    &self.hub,
                    representative.record_id,
                    representative.kind,
                    None,
                ) => response,
            };
            let response = match response {
                Ok(response) => response,
                Err(HubTransferError::Http {
                    status: 404 | 409, ..
                }) => continue,
                Err(error) => return Err(anyhow!(error)),
            };
            match classify_payload_response(&response, group.byte_size, &representative.media_type)?
            {
                PayloadResponseDisposition::TryAnotherClaim => continue,
                PayloadResponseDisposition::SnapshotAdvanced => {
                    return Ok(PayloadOutcome::SnapshotAdvanced)
                }
                PayloadResponseDisposition::Stream => {}
            }
            if cancellation.is_cancelled() {
                return Ok(PayloadOutcome::Cancelled);
            }
            let published = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Ok(PayloadOutcome::Cancelled),
                published = VerifiedCacheBlob::publish_stream_for_db(
                    &self.db_path,
                    source,
                    group.checksum.clone(),
                    group.byte_size,
                    representative.media_type.clone(),
                    response.body,
                ) => published,
            };
            let blob = match published {
                Ok(blob) => blob,
                Err(error)
                    if error
                        .downcast_ref::<crate::sync::payload::BlobIntegrityError>()
                        .is_some() =>
                {
                    return Ok(PayloadOutcome::SnapshotAdvanced);
                }
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("streaming cache blob {}", group.checksum));
                }
            };
            if cancellation.is_cancelled() {
                return Ok(PayloadOutcome::Cancelled);
            }
            let mut connection = crate::db::open_db_at(&self.db_path)?;
            LibraryCacheStore::register_verified_blob(&mut connection, &blob)?;
            return Ok(PayloadOutcome::Registered);
        }
        Ok(PayloadOutcome::SnapshotAdvanced)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PayloadResponseDisposition {
    Stream,
    TryAnotherClaim,
    SnapshotAdvanced,
}

fn classify_payload_response(
    response: &StreamingPayloadResponse,
    expected_byte_size: u64,
    expected_media_type: &str,
) -> Result<PayloadResponseDisposition> {
    match response.status {
        200 => {}
        404 | 409 => return Ok(PayloadResponseDisposition::TryAnotherClaim),
        status @ (408 | 425 | 429 | 500..=599) => {
            bail!("Home Hub returned retryable HTTP {status} while downloading a cache blob")
        }
        status => bail!("Home Hub returned HTTP {status} instead of a full payload"),
    }
    if response.metadata.content_range.is_some() {
        bail!("Home Hub returned Content-Range with a full cache payload");
    }
    if response
        .metadata
        .content_length
        .is_some_and(|length| length != expected_byte_size)
    {
        return Ok(PayloadResponseDisposition::SnapshotAdvanced);
    }
    let Some(content_type) = response.metadata.content_type.as_ref() else {
        return Ok(PayloadResponseDisposition::SnapshotAdvanced);
    };
    let Ok(content_type) = content_type.to_str() else {
        return Ok(PayloadResponseDisposition::SnapshotAdvanced);
    };
    if content_type != expected_media_type {
        return Ok(PayloadResponseDisposition::SnapshotAdvanced);
    }
    Ok(PayloadResponseDisposition::Stream)
}

fn group_blob_claims(claims: Vec<CacheBlobClaim>) -> Result<Vec<BlobClaimGroup>> {
    let mut groups: Vec<BlobClaimGroup> = Vec::new();
    for claim in claims {
        if claim.descriptor.availability != PayloadAvailability::Available {
            bail!("cache blob claim is not available");
        }
        let checksum = claim
            .descriptor
            .checksum
            .context("cache blob claim omitted its checksum")?;
        let byte_size = claim
            .descriptor
            .byte_size
            .context("cache blob claim omitted its byte size")?;
        let media_type = claim
            .descriptor
            .media_type
            .context("cache blob claim omitted its media type")?;
        if groups
            .iter()
            .any(|group| group.checksum == checksum && group.byte_size != byte_size)
        {
            bail!("cache blob claims disagree on byte size for checksum {checksum}");
        }
        let representative = PayloadRepresentative {
            record_id: claim.record_id,
            kind: claim.kind,
            media_type,
        };
        if let Some(group) = groups.iter_mut().find(|group| group.checksum == checksum) {
            group.representatives.push(representative);
        } else {
            groups.push(BlobClaimGroup {
                checksum,
                byte_size,
                representatives: vec![representative],
            });
        }
    }
    Ok(groups)
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use audetic_core::sync::{
        CacheLevel, DeviceId, HubConnection, HubId, PayloadAvailability, RecordId,
    };
    use bytes::Bytes;
    use futures_util::StreamExt;
    use sha2::{Digest, Sha256};
    use tokio::sync::Notify;
    use tokio_util::sync::CancellationToken;

    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use crate::db::library_cache::{cache_blob_path_for_db, LibraryCacheStore, VerifiedCacheBlob};
    use crate::sync::protocol::{
        ChangeCursor, ChangeOperation, ChangePage, ChangeRecord, ChangeTarget, DictationPayload,
        DictationSnapshot, RecordKind, RecordingPayloadDescriptor, Snapshot, MAX_CHANGE_PAGE,
    };
    use crate::sync::transport::{
        HubChangeSource, HubTransferError, PayloadMetadata, RemotePayloadSource,
        StreamingPayloadResponse,
    };

    use super::*;

    struct ScriptedChanges {
        pages: Mutex<VecDeque<Result<ChangePage, HubTransferError>>>,
        calls: Mutex<Vec<(ChangeCursor, Option<ChangeTarget>, usize)>>,
    }

    impl ScriptedChanges {
        fn new(pages: Vec<ChangePage>) -> Self {
            Self {
                pages: Mutex::new(pages.into_iter().map(Ok).collect()),
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl HubChangeSource for ScriptedChanges {
        async fn page_changes(
            &self,
            _hub: &HubConnection,
            after: ChangeCursor,
            target: Option<ChangeTarget>,
            limit: usize,
        ) -> Result<ChangePage, HubTransferError> {
            self.calls.lock().unwrap().push((after, target, limit));
            self.pages
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected change page request")
        }
    }

    struct BlockingChanges {
        started: Arc<Notify>,
    }

    #[async_trait]
    impl HubChangeSource for BlockingChanges {
        async fn page_changes(
            &self,
            _hub: &HubConnection,
            _after: ChangeCursor,
            _target: Option<ChangeTarget>,
            _limit: usize,
        ) -> Result<ChangePage, HubTransferError> {
            self.started.notify_one();
            std::future::pending().await
        }
    }

    struct UnusedPayloads;

    #[async_trait]
    impl RemotePayloadSource for UnusedPayloads {
        async fn stream_payload(
            &self,
            _hub: &HubConnection,
            _id: RecordId,
            _kind: RecordKind,
            _range: Option<&str>,
        ) -> Result<StreamingPayloadResponse, HubTransferError> {
            panic!("text cache must not request payloads")
        }
    }

    struct ScriptedPayloads {
        responses: Mutex<VecDeque<Result<StreamingPayloadResponse, HubTransferError>>>,
        calls: Mutex<Vec<(RecordId, RecordKind, Option<String>)>>,
    }

    impl ScriptedPayloads {
        fn new(responses: Vec<Result<StreamingPayloadResponse, HubTransferError>>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl RemotePayloadSource for ScriptedPayloads {
        async fn stream_payload(
            &self,
            _hub: &HubConnection,
            id: RecordId,
            kind: RecordKind,
            range: Option<&str>,
        ) -> Result<StreamingPayloadResponse, HubTransferError> {
            self.calls
                .lock()
                .unwrap()
                .push((id, kind, range.map(str::to_owned)));
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected payload request")
        }
    }

    struct BlockingBodyPayloads {
        started: Arc<Notify>,
        expected_size: u64,
    }

    #[async_trait]
    impl RemotePayloadSource for BlockingBodyPayloads {
        async fn stream_payload(
            &self,
            _hub: &HubConnection,
            _id: RecordId,
            _kind: RecordKind,
            _range: Option<&str>,
        ) -> Result<StreamingPayloadResponse, HubTransferError> {
            let started = self.started.clone();
            let body = futures_util::stream::once(async move {
                started.notify_one();
                Ok(Bytes::from_static(b"partial"))
            })
            .chain(futures_util::stream::pending::<
                Result<Bytes, HubTransferError>,
            >());
            Ok(StreamingPayloadResponse {
                status: 200,
                metadata: PayloadMetadata {
                    content_type: Some("audio/wav".parse().unwrap()),
                    content_length: Some(self.expected_size),
                    ..PayloadMetadata::default()
                },
                body: Box::pin(body),
            })
        }
    }

    #[derive(Default)]
    struct EventLog(Mutex<Vec<WorkerEvent>>);

    impl WorkerObserver for EventLog {
        fn observe(&self, event: WorkerEvent) {
            self.0.lock().unwrap().push(event);
        }
    }

    struct CancelOnSleep;

    #[async_trait]
    impl SyncClock for CancelOnSleep {
        fn now(&self) -> chrono::DateTime<chrono::Utc> {
            chrono::DateTime::parse_from_rfc3339("2030-01-02T03:04:05Z")
                .unwrap()
                .with_timezone(&chrono::Utc)
        }

        async fn sleep(&self, _duration: Duration, cancellation: &CancellationToken) {
            cancellation.cancel();
        }
    }

    fn hub(source: HubId) -> HubConnection {
        HubConnection {
            base_url: "https://hub.example.ts.net/audetic/".into(),
            hub_id: source,
            owner_login: "owner@example.com".into(),
        }
    }

    fn dictation_change(cursor: u64, id: RecordId, text: &str) -> ChangeRecord {
        let origin = DeviceId::new();
        ChangeRecord {
            cursor: ChangeCursor::new(cursor),
            operation: ChangeOperation::Upsert,
            kind: RecordKind::Dictation,
            record_id: id,
            origin_device_id: Some(origin),
            authoritative_revision: 1,
            snapshot: Some(Snapshot::Dictation(DictationSnapshot {
                kind: RecordKind::Dictation,
                schema_version: 1,
                record_id: id,
                origin_device_id: origin,
                local_version: 1,
                created_at: "2026-09-08T10:00:00Z".into(),
                updated_at: "2026-09-08T10:00:00Z".into(),
                payload: DictationPayload {
                    text: text.into(),
                    recording_payload: Default::default(),
                },
            })),
            changed_at: "2026-09-08T10:00:00Z".into(),
        }
    }

    fn deletion(cursor: u64, id: RecordId) -> ChangeRecord {
        ChangeRecord {
            cursor: ChangeCursor::new(cursor),
            operation: ChangeOperation::Delete,
            kind: RecordKind::Dictation,
            record_id: id,
            origin_device_id: None,
            authoritative_revision: 2,
            snapshot: None,
            changed_at: "2026-09-08T11:00:00Z".into(),
        }
    }

    fn available_change(
        cursor: u64,
        id: RecordId,
        checksum: &str,
        byte_size: u64,
        media_type: &str,
    ) -> ChangeRecord {
        let mut change = dictation_change(cursor, id, "audio");
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

    fn payload_response(
        status: u16,
        body: &'static [u8],
        content_length: Option<u64>,
        content_type: Option<&str>,
    ) -> StreamingPayloadResponse {
        StreamingPayloadResponse {
            status,
            metadata: PayloadMetadata {
                content_type: content_type.map(|value| value.parse().unwrap()),
                content_length,
                ..PayloadMetadata::default()
            },
            body: Box::pin(futures_util::stream::once(async move {
                Ok(Bytes::from_static(body))
            })),
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

    fn activate_text_baseline(db_path: &std::path::Path, source: HubId, id: RecordId) {
        let mut conn = crate::db::open_db_at(db_path).unwrap();
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
            &page(0, 1, vec![dictation_change(1, id, "baseline")]),
        )
        .unwrap();
        LibraryCacheStore::activate_complete_generation(&mut conn, source, generation).unwrap();
    }

    #[tokio::test]
    async fn live_only_evicts_generations_and_traverses_one_fixed_target() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("cache.db");
        crate::db::migrate_db_at(&db_path).unwrap();
        let source = HubId::new();
        activate_text_baseline(&db_path, source, RecordId::new());
        let deleted = RecordId::new();
        let changes = Arc::new(ScriptedChanges::new(vec![
            page(1, 3, vec![dictation_change(2, RecordId::new(), "ignored")]),
            page(2, 3, vec![deletion(3, deleted)]),
        ]));
        let worker = CacheReplica::new(
            db_path.clone(),
            hub(source),
            CacheLevel::LiveOnly,
            changes.clone(),
            Arc::new(UnusedPayloads),
        );

        worker
            .process_once_cancellable(&CancellationToken::new())
            .await
            .unwrap();

        let conn = crate::db::open_db_at(&db_path).unwrap();
        assert!(LibraryCacheStore::active_generation(&conn, source)
            .unwrap()
            .is_none());
        assert_eq!(
            LibraryCacheStore::source_cursor(&conn, source).unwrap(),
            ChangeCursor::new(3)
        );
        assert!(LibraryCacheStore::live_overlay_contains(&conn, source, deleted).unwrap());
        assert_eq!(
            *changes.calls.lock().unwrap(),
            vec![
                (ChangeCursor::new(1), None, MAX_CHANGE_PAGE),
                (
                    ChangeCursor::new(2),
                    Some(ChangeTarget::new(ChangeCursor::new(3))),
                    MAX_CHANGE_PAGE,
                ),
            ]
        );
    }

    #[tokio::test]
    async fn text_refresh_resumes_the_persisted_inactive_generation() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("cache.db");
        let mut conn = crate::db::migrate_db_at(&db_path).unwrap();
        let source = HubId::new();
        let generation = LibraryCacheStore::begin_generation(
            &mut conn,
            source,
            CacheLevel::TextForOfflineUse,
            ChangeCursor::ZERO,
            ChangeTarget::new(ChangeCursor::new(3)),
        )
        .unwrap();
        LibraryCacheStore::apply_validated_page(
            &mut conn,
            source,
            generation,
            &page(
                0,
                3,
                vec![dictation_change(1, RecordId::new(), "before restart")],
            ),
        )
        .unwrap();
        drop(conn);
        let changes = Arc::new(ScriptedChanges::new(vec![page(
            1,
            3,
            vec![
                dictation_change(2, RecordId::new(), "resumed"),
                dictation_change(3, RecordId::new(), "complete"),
            ],
        )]));
        let worker = CacheReplica::new(
            db_path.clone(),
            hub(source),
            CacheLevel::TextForOfflineUse,
            changes.clone(),
            Arc::new(UnusedPayloads),
        );

        worker.process_once().await.unwrap();

        let conn = crate::db::open_db_at(&db_path).unwrap();
        let active = LibraryCacheStore::active_generation(&conn, source)
            .unwrap()
            .unwrap();
        assert_eq!(active.id, generation);
        assert_eq!(active.applied_cursor, ChangeCursor::new(3));
        assert_eq!(
            LibraryCacheStore::active_items(&conn, source)
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            *changes.calls.lock().unwrap(),
            vec![(
                ChangeCursor::new(1),
                Some(ChangeTarget::new(ChangeCursor::new(3))),
                MAX_CHANGE_PAGE,
            )]
        );
    }

    #[tokio::test]
    async fn full_audio_groups_equal_claims_and_tries_another_record_after_404() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("cache.db");
        crate::db::migrate_db_at(&db_path).unwrap();
        let source = HubId::new();
        let bytes = b"one payload for two claims";
        let checksum = format!("{:x}", Sha256::digest(bytes));
        let left = RecordId::new();
        let right = RecordId::new();
        let (first, second) = if left.to_string() < right.to_string() {
            (left, right)
        } else {
            (right, left)
        };
        let changes = Arc::new(ScriptedChanges::new(vec![page(
            0,
            2,
            vec![
                available_change(1, first, &checksum, bytes.len() as u64, "audio/wav"),
                available_change(2, second, &checksum, bytes.len() as u64, "audio/wav"),
            ],
        )]));
        let payloads = Arc::new(ScriptedPayloads::new(vec![
            Ok(payload_response(404, b"", Some(0), None)),
            Ok(payload_response(
                200,
                bytes,
                Some(bytes.len() as u64),
                Some("audio/wav"),
            )),
        ]));
        let worker = CacheReplica::new(
            db_path.clone(),
            hub(source),
            CacheLevel::TextAndAvailableAudio,
            changes,
            payloads.clone(),
        );

        worker.process_once().await.unwrap();

        let conn = crate::db::open_db_at(&db_path).unwrap();
        assert!(LibraryCacheStore::active_generation(&conn, source)
            .unwrap()
            .is_some());
        let stored = LibraryCacheStore::verified_blob(&conn, source, &checksum)
            .unwrap()
            .unwrap();
        stored.verify().unwrap();
        assert_eq!(
            *payloads.calls.lock().unwrap(),
            vec![
                (first, RecordKind::Dictation, None),
                (second, RecordKind::Dictation, None),
            ]
        );
    }

    #[test]
    fn equal_checksums_with_conflicting_sizes_are_rejected() {
        let checksum = "a".repeat(64);
        let claims = vec![
            CacheBlobClaim {
                record_id: RecordId::new(),
                kind: RecordKind::Dictation,
                descriptor: RecordingPayloadDescriptor {
                    checksum: Some(checksum.clone()),
                    byte_size: Some(10),
                    media_type: Some("audio/wav".into()),
                    availability: PayloadAvailability::Available,
                },
            },
            CacheBlobClaim {
                record_id: RecordId::new(),
                kind: RecordKind::Meeting,
                descriptor: RecordingPayloadDescriptor {
                    checksum: Some(checksum),
                    byte_size: Some(11),
                    media_type: Some("audio/mpeg".into()),
                    availability: PayloadAvailability::Available,
                },
            },
        ];

        assert!(group_blob_claims(claims)
            .unwrap_err()
            .to_string()
            .contains("disagree on byte size"));
    }

    #[tokio::test]
    async fn full_audio_reuses_identical_bytes_across_media_types() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("cache.db");
        crate::db::migrate_db_at(&db_path).unwrap();
        let source = HubId::new();
        let bytes = b"identical bytes with media aliases";
        let checksum = format!("{:x}", Sha256::digest(bytes));
        let left = RecordId::new();
        let right = RecordId::new();
        let (first, second) = if left.to_string() < right.to_string() {
            (left, right)
        } else {
            (right, left)
        };
        let payloads = Arc::new(ScriptedPayloads::new(vec![
            Ok(payload_response(404, b"", Some(0), None)),
            Ok(payload_response(
                200,
                bytes,
                Some(bytes.len() as u64),
                Some("audio/mpeg"),
            )),
        ]));
        let worker = CacheReplica::new(
            db_path.clone(),
            hub(source),
            CacheLevel::TextAndAvailableAudio,
            Arc::new(ScriptedChanges::new(vec![page(
                0,
                2,
                vec![
                    available_change(1, first, &checksum, bytes.len() as u64, "audio/wav"),
                    available_change(2, second, &checksum, bytes.len() as u64, "audio/mpeg"),
                ],
            )])),
            payloads.clone(),
        );

        worker.process_once().await.unwrap();

        let conn = crate::db::open_db_at(&db_path).unwrap();
        assert!(LibraryCacheStore::active_generation(&conn, source)
            .unwrap()
            .is_some());
        assert_eq!(payloads.calls.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn full_audio_tries_another_record_after_409() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("cache.db");
        crate::db::migrate_db_at(&db_path).unwrap();
        let source = HubId::new();
        let bytes = b"payload retained by another record";
        let checksum = format!("{:x}", Sha256::digest(bytes));
        let left = RecordId::new();
        let right = RecordId::new();
        let (first, second) = if left.to_string() < right.to_string() {
            (left, right)
        } else {
            (right, left)
        };
        let payloads = Arc::new(ScriptedPayloads::new(vec![
            Ok(payload_response(409, b"", Some(0), None)),
            Ok(payload_response(
                200,
                bytes,
                Some(bytes.len() as u64),
                Some("audio/wav"),
            )),
        ]));
        let worker = CacheReplica::new(
            db_path.clone(),
            hub(source),
            CacheLevel::TextAndAvailableAudio,
            Arc::new(ScriptedChanges::new(vec![page(
                0,
                2,
                vec![
                    available_change(1, first, &checksum, bytes.len() as u64, "audio/wav"),
                    available_change(2, second, &checksum, bytes.len() as u64, "audio/wav"),
                ],
            )])),
            payloads.clone(),
        );

        worker.process_once().await.unwrap();

        let conn = crate::db::open_db_at(&db_path).unwrap();
        assert!(LibraryCacheStore::active_generation(&conn, source)
            .unwrap()
            .is_some());
        assert_eq!(payloads.calls.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn retryable_body_failure_preserves_complete_generation_for_resume() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("cache.db");
        crate::db::migrate_db_at(&db_path).unwrap();
        let source = HubId::new();
        let bytes = b"complete retry payload";
        let checksum = format!("{:x}", Sha256::digest(bytes));
        let record_id = RecordId::new();
        let changes = Arc::new(ScriptedChanges::new(vec![page(
            0,
            1,
            vec![available_change(
                1,
                record_id,
                &checksum,
                bytes.len() as u64,
                "audio/wav",
            )],
        )]));
        let interrupted = StreamingPayloadResponse {
            status: 200,
            metadata: PayloadMetadata {
                content_length: Some(bytes.len() as u64),
                content_type: Some("audio/wav".parse().unwrap()),
                ..PayloadMetadata::default()
            },
            body: Box::pin(futures_util::stream::iter([
                Ok(Bytes::from_static(b"partial")),
                Err(HubTransferError::Transport("body interrupted".into())),
            ])),
        };
        let first = CacheReplica::new(
            db_path.clone(),
            hub(source),
            CacheLevel::TextAndAvailableAudio,
            changes,
            Arc::new(ScriptedPayloads::new(vec![Ok(interrupted)])),
        );

        assert!(first.process_once().await.is_err());
        let generation = {
            let conn = crate::db::open_db_at(&db_path).unwrap();
            let generation = LibraryCacheStore::inactive_generation(&conn, source)
                .unwrap()
                .unwrap();
            assert!(generation.complete);
            generation
        };

        let no_more_changes = Arc::new(ScriptedChanges::new(Vec::new()));
        let retry = CacheReplica::new(
            db_path.clone(),
            hub(source),
            CacheLevel::TextAndAvailableAudio,
            no_more_changes.clone(),
            Arc::new(ScriptedPayloads::new(vec![Ok(payload_response(
                200,
                bytes,
                Some(bytes.len() as u64),
                Some("audio/wav"),
            ))])),
        );
        retry.process_once().await.unwrap();

        let conn = crate::db::open_db_at(&db_path).unwrap();
        assert_eq!(
            LibraryCacheStore::active_generation(&conn, source)
                .unwrap()
                .unwrap()
                .id,
            generation.id
        );
        assert!(no_more_changes.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn all_missing_payload_representatives_discard_the_stale_generation() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("cache.db");
        crate::db::migrate_db_at(&db_path).unwrap();
        let source = HubId::new();
        let bytes = b"missing payload";
        let checksum = format!("{:x}", Sha256::digest(bytes));
        let left = RecordId::new();
        let right = RecordId::new();
        let worker = CacheReplica::new(
            db_path.clone(),
            hub(source),
            CacheLevel::TextAndAvailableAudio,
            Arc::new(ScriptedChanges::new(vec![page(
                0,
                2,
                vec![
                    available_change(1, left, &checksum, bytes.len() as u64, "audio/wav"),
                    available_change(2, right, &checksum, bytes.len() as u64, "audio/wav"),
                ],
            )])),
            Arc::new(ScriptedPayloads::new(vec![
                Ok(payload_response(404, b"", Some(0), None)),
                Ok(payload_response(409, b"", Some(0), None)),
            ])),
        );

        worker.process_once().await.unwrap();

        let conn = crate::db::open_db_at(&db_path).unwrap();
        assert!(LibraryCacheStore::inactive_generation(&conn, source)
            .unwrap()
            .is_none());
        assert!(LibraryCacheStore::active_generation(&conn, source)
            .unwrap()
            .is_none());
        assert_eq!(
            LibraryCacheStore::source_cursor(&conn, source).unwrap(),
            ChangeCursor::ZERO
        );
    }

    #[tokio::test]
    async fn exact_current_text_baseline_does_not_create_a_generation() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("cache.db");
        crate::db::migrate_db_at(&db_path).unwrap();
        let source = HubId::new();
        activate_text_baseline(&db_path, source, RecordId::new());
        let original = {
            let conn = crate::db::open_db_at(&db_path).unwrap();
            LibraryCacheStore::active_generation(&conn, source)
                .unwrap()
                .unwrap()
                .id
        };
        let changes = Arc::new(ScriptedChanges::new(vec![page(1, 1, Vec::new())]));
        let worker = CacheReplica::new(
            db_path.clone(),
            hub(source),
            CacheLevel::TextForOfflineUse,
            changes.clone(),
            Arc::new(UnusedPayloads),
        );

        worker.process_once().await.unwrap();

        let conn = crate::db::open_db_at(&db_path).unwrap();
        assert_eq!(
            LibraryCacheStore::active_generation(&conn, source)
                .unwrap()
                .unwrap()
                .id,
            original
        );
        assert!(LibraryCacheStore::inactive_generation(&conn, source)
            .unwrap()
            .is_none());
        assert_eq!(
            *changes.calls.lock().unwrap(),
            vec![(ChangeCursor::new(1), None, MAX_CHANGE_PAGE)]
        );
    }

    #[tokio::test]
    async fn conflicting_inactive_level_is_discarded_before_a_zero_cursor_rebuild() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("cache.db");
        let mut conn = crate::db::migrate_db_at(&db_path).unwrap();
        let source = HubId::new();
        let stale = LibraryCacheStore::begin_generation(
            &mut conn,
            source,
            CacheLevel::TextAndAvailableAudio,
            ChangeCursor::ZERO,
            ChangeTarget::new(ChangeCursor::new(1)),
        )
        .unwrap();
        drop(conn);
        let changes = Arc::new(ScriptedChanges::new(vec![page(
            0,
            1,
            vec![dictation_change(1, RecordId::new(), "rebuilt")],
        )]));
        let worker = CacheReplica::new(
            db_path.clone(),
            hub(source),
            CacheLevel::TextForOfflineUse,
            changes.clone(),
            Arc::new(UnusedPayloads),
        );

        worker.process_once().await.unwrap();

        let conn = crate::db::open_db_at(&db_path).unwrap();
        assert!(LibraryCacheStore::generation(&conn, source, stale)
            .unwrap()
            .is_none());
        assert_eq!(
            LibraryCacheStore::active_generation(&conn, source)
                .unwrap()
                .unwrap()
                .start_cursor,
            ChangeCursor::ZERO
        );
        assert_eq!(
            *changes.calls.lock().unwrap(),
            vec![(ChangeCursor::ZERO, None, MAX_CHANGE_PAGE)]
        );
    }

    #[tokio::test]
    async fn payload_metadata_mismatch_discards_snapshot_that_advanced() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("cache.db");
        crate::db::migrate_db_at(&db_path).unwrap();
        let source = HubId::new();
        let bytes = b"target payload";
        let checksum = format!("{:x}", Sha256::digest(bytes));
        let worker = CacheReplica::new(
            db_path.clone(),
            hub(source),
            CacheLevel::TextAndAvailableAudio,
            Arc::new(ScriptedChanges::new(vec![page(
                0,
                1,
                vec![available_change(
                    1,
                    RecordId::new(),
                    &checksum,
                    bytes.len() as u64,
                    "audio/wav",
                )],
            )])),
            Arc::new(ScriptedPayloads::new(vec![Ok(payload_response(
                200,
                bytes,
                Some(bytes.len() as u64 + 1),
                Some("audio/wav"),
            ))])),
        );

        worker.process_once().await.unwrap();

        let conn = crate::db::open_db_at(&db_path).unwrap();
        assert!(LibraryCacheStore::inactive_generation(&conn, source)
            .unwrap()
            .is_none());
        assert!(LibraryCacheStore::active_generation(&conn, source)
            .unwrap()
            .is_none());
    }

    #[test]
    fn missing_payload_content_type_marks_snapshot_advanced() {
        let response = payload_response(200, b"a", Some(1), None);

        assert_eq!(
            classify_payload_response(&response, 1, "audio/wav").unwrap(),
            PayloadResponseDisposition::SnapshotAdvanced
        );
    }

    #[tokio::test]
    async fn checksum_mismatch_discards_snapshot_that_advanced_with_equal_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("cache.db");
        crate::db::migrate_db_at(&db_path).unwrap();
        let source = HubId::new();
        let target_bytes = b"old";
        let current_bytes = b"new";
        let checksum = format!("{:x}", Sha256::digest(target_bytes));
        let worker = CacheReplica::new(
            db_path.clone(),
            hub(source),
            CacheLevel::TextAndAvailableAudio,
            Arc::new(ScriptedChanges::new(vec![page(
                0,
                1,
                vec![available_change(
                    1,
                    RecordId::new(),
                    &checksum,
                    target_bytes.len() as u64,
                    "audio/wav",
                )],
            )])),
            Arc::new(ScriptedPayloads::new(vec![Ok(payload_response(
                200,
                current_bytes,
                Some(current_bytes.len() as u64),
                Some("audio/wav"),
            ))])),
        );

        worker.process_once().await.unwrap();

        let connection = crate::db::open_db_at(&db_path).unwrap();
        assert!(LibraryCacheStore::inactive_generation(&connection, source)
            .unwrap()
            .is_none());
        assert!(LibraryCacheStore::active_generation(&connection, source)
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn corrupt_registered_cache_file_is_replaced_from_the_stream() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("cache.db");
        let mut conn = crate::db::migrate_db_at(&db_path).unwrap();
        let source = HubId::new();
        let bytes = b"verified replacement";
        let checksum = format!("{:x}", Sha256::digest(bytes));
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
            &page(
                0,
                1,
                vec![available_change(
                    1,
                    RecordId::new(),
                    &checksum,
                    bytes.len() as u64,
                    "audio/wav",
                )],
            ),
        )
        .unwrap();
        let blob = VerifiedCacheBlob::publish_stream_for_db(
            &db_path,
            source,
            checksum.clone(),
            bytes.len() as u64,
            "audio/wav".into(),
            futures_util::stream::iter([Ok::<_, HubTransferError>(Bytes::from_static(bytes))]),
        )
        .await
        .unwrap();
        LibraryCacheStore::register_verified_blob(&mut conn, &blob).unwrap();
        let path = cache_blob_path_for_db(&db_path, source, &checksum).unwrap();
        std::fs::write(&path, b"corrupt replacement").unwrap();
        drop(conn);
        let worker = CacheReplica::new(
            db_path.clone(),
            hub(source),
            CacheLevel::TextAndAvailableAudio,
            Arc::new(ScriptedChanges::new(Vec::new())),
            Arc::new(ScriptedPayloads::new(vec![Ok(payload_response(
                200,
                bytes,
                Some(bytes.len() as u64),
                Some("audio/wav"),
            ))])),
        );

        worker.process_once().await.unwrap();

        assert_eq!(std::fs::read(path).unwrap(), bytes);
        let conn = crate::db::open_db_at(&db_path).unwrap();
        assert_eq!(
            LibraryCacheStore::active_generation(&conn, source)
                .unwrap()
                .unwrap()
                .id,
            generation
        );
    }

    #[tokio::test]
    async fn cancellation_drops_an_in_flight_page_without_creating_a_generation() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("cache.db");
        crate::db::migrate_db_at(&db_path).unwrap();
        let source = HubId::new();
        let started = Arc::new(Notify::new());
        let worker = CacheReplica::new(
            db_path.clone(),
            hub(source),
            CacheLevel::TextForOfflineUse,
            Arc::new(BlockingChanges {
                started: started.clone(),
            }),
            Arc::new(UnusedPayloads),
        );
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let task =
            tokio::spawn(async move { worker.process_once_cancellable(&task_cancellation).await });
        tokio::time::timeout(Duration::from_secs(1), started.notified())
            .await
            .unwrap();

        cancellation.cancel();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("cancelled refresh should join promptly")
            .unwrap()
            .unwrap();

        let conn = crate::db::open_db_at(&db_path).unwrap();
        assert!(LibraryCacheStore::inactive_generation(&conn, source)
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn cancellation_drops_payload_stream_temp_file_and_keeps_complete_generation() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("cache.db");
        crate::db::migrate_db_at(&db_path).unwrap();
        let source = HubId::new();
        let bytes = b"payload that remains resumable";
        let checksum = format!("{:x}", Sha256::digest(bytes));
        let body_started = Arc::new(Notify::new());
        let worker = CacheReplica::new(
            db_path.clone(),
            hub(source),
            CacheLevel::TextAndAvailableAudio,
            Arc::new(ScriptedChanges::new(vec![page(
                0,
                1,
                vec![available_change(
                    1,
                    RecordId::new(),
                    &checksum,
                    bytes.len() as u64,
                    "audio/wav",
                )],
            )])),
            Arc::new(BlockingBodyPayloads {
                started: body_started.clone(),
                expected_size: bytes.len() as u64,
            }),
        );
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let task =
            tokio::spawn(async move { worker.process_once_cancellable(&task_cancellation).await });
        tokio::time::timeout(Duration::from_secs(1), body_started.notified())
            .await
            .unwrap();

        cancellation.cancel();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("cancelled payload stream should join promptly")
            .unwrap()
            .unwrap();

        let conn = crate::db::open_db_at(&db_path).unwrap();
        assert!(
            LibraryCacheStore::inactive_generation(&conn, source)
                .unwrap()
                .unwrap()
                .complete
        );
        let temporary = temp
            .path()
            .join("sync/library-cache-blobs")
            .join(source.to_string())
            .join(".tmp");
        assert_eq!(std::fs::read_dir(temporary).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn restart_cleanup_removes_abandoned_and_unclaimed_files() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("cache.db");
        crate::db::migrate_db_at(&db_path).unwrap();
        let source = HubId::new();
        let temp_path = temp
            .path()
            .join("sync/library-cache-blobs")
            .join(source.to_string())
            .join(".tmp")
            .join("interrupted");
        std::fs::create_dir_all(temp_path.parent().unwrap()).unwrap();
        std::fs::write(&temp_path, b"partial").unwrap();
        let orphan_bytes = b"published without registration or claim";
        let orphan_checksum = format!("{:x}", Sha256::digest(orphan_bytes));
        let orphan_path = cache_blob_path_for_db(&db_path, source, &orphan_checksum).unwrap();
        std::fs::create_dir_all(orphan_path.parent().unwrap()).unwrap();
        std::fs::write(&orphan_path, orphan_bytes).unwrap();
        let worker = CacheReplica::new(
            db_path,
            hub(source),
            CacheLevel::LiveOnly,
            Arc::new(ScriptedChanges::new(vec![page(0, 0, Vec::new())])),
            Arc::new(UnusedPayloads),
        );

        worker.process_once().await.unwrap();

        assert!(!temp_path.exists());
        assert!(!orphan_path.exists());
    }

    #[tokio::test]
    async fn cleanup_failure_is_reported_after_successful_activation() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("cache.db");
        let conn = crate::db::migrate_db_at(&db_path).unwrap();
        let source = HubId::new();
        let orphan_checksum = "a".repeat(64);
        let orphan_path = cache_blob_path_for_db(&db_path, source, &orphan_checksum).unwrap();
        std::fs::create_dir_all(&orphan_path).unwrap();
        conn.execute(
            "INSERT INTO library_cache_sources(source_hub_id) VALUES(?1)",
            [source.to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO library_cache_blobs
                (source_hub_id,checksum,local_path,byte_size,media_type,verified,cleanup_pending)
             VALUES(?1,?2,?3,1,'audio/wav',0,1)",
            rusqlite::params![
                source.to_string(),
                orphan_checksum,
                orphan_path.to_string_lossy()
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO library_cache_blob_cleanup(source_hub_id,checksum,local_path)
             VALUES(?1,?2,?3)",
            rusqlite::params![
                source.to_string(),
                orphan_checksum,
                orphan_path.to_string_lossy()
            ],
        )
        .unwrap();
        drop(conn);
        let worker = CacheReplica::new(
            db_path.clone(),
            hub(source),
            CacheLevel::TextForOfflineUse,
            Arc::new(ScriptedChanges::new(vec![page(
                0,
                1,
                vec![dictation_change(1, RecordId::new(), "activated")],
            )])),
            Arc::new(UnusedPayloads),
        );

        assert!(worker.process_once().await.is_err());

        let conn = crate::db::open_db_at(&db_path).unwrap();
        assert!(LibraryCacheStore::active_generation(&conn, source)
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn run_observes_one_role_scoped_cycle_and_stops_after_cancellation() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("cache.db");
        crate::db::migrate_db_at(&db_path).unwrap();
        let source = HubId::new();
        let observer = Arc::new(EventLog::default());
        let worker = CacheReplica::new(
            db_path,
            hub(source),
            CacheLevel::LiveOnly,
            Arc::new(ScriptedChanges::new(vec![page(0, 0, Vec::new())])),
            Arc::new(UnusedPayloads),
        )
        .with_role_epoch(42)
        .with_clock(Arc::new(CancelOnSleep))
        .with_observer(observer.clone());
        let cancellation = CancellationToken::new();

        worker.run(cancellation).await;

        assert_eq!(
            *observer.0.lock().unwrap(),
            vec![
                WorkerEvent::CacheReplicaStarted { role_epoch: 42 },
                WorkerEvent::CacheReplicaCycleStarted { role_epoch: 42 },
                WorkerEvent::CacheReplicaCycleSucceeded { role_epoch: 42 },
                WorkerEvent::CacheReplicaStopped { role_epoch: 42 },
            ]
        );
    }
}
