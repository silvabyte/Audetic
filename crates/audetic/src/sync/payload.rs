use anyhow::{bail, Context, Result};
use bytes::Bytes;
use fs2::FileExt;
use futures_util::{Stream, StreamExt};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::protocol::{is_canonical_sha256, RecordingPayloadDescriptor, MAX_BLOB_BYTES};

#[derive(Clone, Debug)]
pub struct StagedPayload {
    pub descriptor: RecordingPayloadDescriptor,
    pub path: PathBuf,
    // Keep the staging namespace locked until the caller has either committed
    // the outbox reference or discarded this payload.
    _staging_lock: Arc<File>,
}

#[derive(Clone, Debug)]
pub struct StoredBlob {
    pub checksum: String,
    pub path: PathBuf,
    pub byte_size: u64,
    pub media_type: String,
    pub created: bool,
}

struct TemporaryFileGuard(PathBuf);

impl Drop for TemporaryFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub fn resolve_operational_audio(path: &Path) -> std::io::Result<Option<PathBuf>> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => return Ok(Some(path.to_path_buf())),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mp3 = path.with_extension("mp3");
    match std::fs::metadata(&mp3) {
        Ok(metadata) if metadata.is_file() => Ok(Some(mp3)),
        Ok(_) => Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn staging_root_for_db(db_path: &Path) -> PathBuf {
    db_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("sync")
        .join("staging")
}

pub fn stage_recording(db_path: &Path, source: &Path) -> Result<Option<StagedPayload>> {
    stage_recording_cancellable(db_path, source, &CancellationToken::new())
}

pub fn stage_recording_cancellable(
    db_path: &Path,
    source: &Path,
    cancellation: &CancellationToken,
) -> Result<Option<StagedPayload>> {
    let Some(source) = resolve_operational_audio(source)
        .with_context(|| format!("probing Recording Payload source {}", source.display()))?
    else {
        return Ok(None);
    };
    let root = staging_root_for_db(db_path);
    std::fs::create_dir_all(&root).context("creating Recording Payload staging directory")?;
    let staging_lock = Arc::new(
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join(".lock"))
            .context("opening Recording Payload staging lock")?,
    );
    loop {
        match staging_lock.try_lock_exclusive() {
            Ok(()) => break,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if cancellation.is_cancelled() {
                    bail!("Recording Payload staging cancelled");
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(error) => return Err(error).context("locking Recording Payload staging directory"),
        }
    }
    if cancellation.is_cancelled() {
        bail!("Recording Payload staging cancelled");
    }
    let temp = root.join(format!(".stage-{}", uuid::Uuid::new_v4()));
    let mut input = File::open(&source)
        .with_context(|| format!("opening Recording Payload source {}", source.display()))?;
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)
        .with_context(|| format!("creating Recording Payload staging file {}", temp.display()))?;
    let result = (|| -> Result<(String, u64)> {
        let mut hasher = Sha256::new();
        let mut size = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            if cancellation.is_cancelled() {
                bail!("Recording Payload staging cancelled");
            }
            let read = input
                .read(&mut buffer)
                .context("reading Recording Payload")?;
            if read == 0 {
                break;
            }
            size = size
                .checked_add(read as u64)
                .context("Recording Payload size overflow")?;
            if size > MAX_BLOB_BYTES {
                bail!("Recording Payload exceeds the {MAX_BLOB_BYTES}-byte upload limit");
            }
            hasher.update(&buffer[..read]);
            output
                .write_all(&buffer[..read])
                .context("writing staged Recording Payload")?;
        }
        if size == 0 {
            bail!("Recording Payload is empty");
        }
        output
            .flush()
            .context("flushing staged Recording Payload")?;
        output
            .sync_all()
            .context("syncing staged Recording Payload")?;
        Ok((format!("{:x}", hasher.finalize()), size))
    })();
    let (checksum, byte_size) = match result {
        Ok(value) => value,
        Err(error) => {
            let _ = std::fs::remove_file(&temp);
            return Err(error);
        }
    };
    let final_path = root.join(&checksum);
    if final_path.exists() {
        std::fs::remove_file(&temp).context("discarding duplicate staged Recording Payload")?;
        verify_file(&final_path, &checksum, byte_size)
            .context("verifying existing staged Recording Payload")?;
    } else {
        std::fs::rename(&temp, &final_path)
            .context("atomically finalizing staged Recording Payload")?;
        sync_directory(&root)?;
    }
    let media_type = media_type_for(&source);
    Ok(Some(StagedPayload {
        descriptor: RecordingPayloadDescriptor::pending(checksum, byte_size, media_type),
        path: final_path,
        _staging_lock: staging_lock,
    }))
}

pub(crate) fn lock_staging_for_db(db_path: &Path) -> Result<File> {
    let root = staging_root_for_db(db_path);
    std::fs::create_dir_all(&root).context("creating Recording Payload staging directory")?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join(".lock"))
        .context("opening Recording Payload staging lock")?;
    lock.lock_exclusive()
        .context("locking Recording Payload staging directory")?;
    Ok(lock)
}

pub fn media_type_for(path: &Path) -> String {
    mime_guess::from_path(path)
        .first_raw()
        .unwrap_or("application/octet-stream")
        .to_owned()
}

#[derive(Clone, Debug)]
pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    pub fn for_db(db_path: &Path) -> Self {
        Self::new(
            db_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("sync")
                .join("blobs"),
        )
    }

    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn canonical_path(&self, checksum: &str) -> Result<PathBuf> {
        if !is_canonical_sha256(checksum) {
            bail!("blob checksum must be 64 lowercase hexadecimal characters");
        }
        Ok(self.root.join(&checksum[..2]).join(checksum))
    }

    pub fn contains(&self, checksum: &str) -> Result<bool> {
        Ok(self.canonical_path(checksum)?.is_file())
    }

    pub async fn put_stream<S, E>(
        &self,
        checksum: &str,
        expected_size: u64,
        media_type: &str,
        mut stream: S,
    ) -> Result<StoredBlob>
    where
        S: Stream<Item = std::result::Result<Bytes, E>> + Unpin,
        E: std::fmt::Display,
    {
        validate_blob_metadata(checksum, expected_size, media_type)?;
        let final_path = self.canonical_path(checksum)?;
        let existed = final_path.is_file();

        let temp_dir = self.root.join(".tmp");
        tokio::fs::create_dir_all(&temp_dir)
            .await
            .context("creating blob temporary directory")?;
        let temp = temp_dir.join(uuid::Uuid::new_v4().to_string());
        let _temp_guard = TemporaryFileGuard(temp.clone());
        let mut output = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)
            .await
            .context("creating blob temporary file")?;
        let received = async {
            let mut hasher = Sha256::new();
            let mut received = 0u64;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|error| anyhow::anyhow!(error.to_string()))?;
                received = received
                    .checked_add(chunk.len() as u64)
                    .context("blob size overflow")?;
                if received > expected_size || received > MAX_BLOB_BYTES {
                    bail!("blob body exceeds its declared size or server limit");
                }
                hasher.update(&chunk);
                output
                    .write_all(&chunk)
                    .await
                    .context("writing blob body")?;
            }
            output.flush().await.context("flushing blob body")?;
            output.sync_all().await.context("syncing blob body")?;
            Ok::<_, anyhow::Error>((received, format!("{:x}", hasher.finalize())))
        }
        .await;
        drop(output);
        let (received, actual_checksum) = match received {
            Ok(value) => value,
            Err(error) => {
                let _ = tokio::fs::remove_file(&temp).await;
                return Err(error);
            }
        };
        if received != expected_size || actual_checksum != checksum {
            let _ = tokio::fs::remove_file(&temp).await;
            bail!(
                "blob verification failed: expected {checksum}/{expected_size}, received {actual_checksum}/{received}"
            );
        }
        if existed {
            tokio::fs::remove_file(&temp)
                .await
                .context("discarding duplicate blob upload")?;
            verify_file(&final_path, checksum, expected_size)?;
            return Ok(StoredBlob {
                checksum: checksum.to_owned(),
                path: final_path,
                byte_size: expected_size,
                media_type: media_type.to_owned(),
                created: false,
            });
        }
        if let Some(parent) = final_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .context("creating canonical blob directory")?;
        }
        match tokio::fs::rename(&temp, &final_path).await {
            Ok(()) => {}
            Err(_error) if final_path.is_file() => {
                let _ = tokio::fs::remove_file(&temp).await;
                verify_file(&final_path, checksum, expected_size)?;
                return Ok(StoredBlob {
                    checksum: checksum.to_owned(),
                    path: final_path,
                    byte_size: expected_size,
                    media_type: media_type.to_owned(),
                    created: false,
                });
            }
            Err(error) => {
                let _ = tokio::fs::remove_file(&temp).await;
                return Err(error).context("atomically finalizing canonical blob");
            }
        }
        if let Some(parent) = final_path.parent() {
            sync_directory(parent)?;
        }
        Ok(StoredBlob {
            checksum: checksum.to_owned(),
            path: final_path,
            byte_size: expected_size,
            media_type: media_type.to_owned(),
            created: true,
        })
    }

    pub async fn put_file(
        &self,
        source: &Path,
        checksum: &str,
        expected_size: u64,
        media_type: &str,
    ) -> Result<StoredBlob> {
        let file = tokio::fs::File::open(source)
            .await
            .with_context(|| format!("opening staged blob {}", source.display()))?;
        let stream = tokio_util::io::ReaderStream::new(file);
        self.put_stream(checksum, expected_size, media_type, stream)
            .await
    }
}

fn validate_blob_metadata(checksum: &str, byte_size: u64, media_type: &str) -> Result<()> {
    if !is_canonical_sha256(checksum) {
        bail!("blob checksum must be 64 lowercase hexadecimal characters");
    }
    if byte_size == 0 || byte_size > MAX_BLOB_BYTES {
        bail!("blob size must be between 1 and {MAX_BLOB_BYTES} bytes");
    }
    if media_type.is_empty() || media_type.len() > 255 || media_type.contains(['\r', '\n']) {
        bail!("blob media type is invalid");
    }
    Ok(())
}

fn verify_file(path: &Path, expected_checksum: &str, expected_size: u64) -> Result<()> {
    let mut file = File::open(path).context("opening existing canonical blob")?;
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).context("reading canonical blob")?;
        if read == 0 {
            break;
        }
        size += read as u64;
        hasher.update(&buffer[..read]);
    }
    let checksum = format!("{:x}", hasher.finalize());
    if size != expected_size || checksum != expected_checksum {
        bail!("existing canonical blob failed verification");
    }
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("syncing directory {}", path.display()))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn interrupted_stream_removes_temporary_blob() {
        let temp = tempfile::tempdir().unwrap();
        let store = BlobStore::new(temp.path().join("blobs"));
        let checksum = format!("{:x}", Sha256::digest(b"complete payload"));
        let stream = futures_util::stream::iter([
            Ok(Bytes::from_static(b"partial")),
            Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "interrupted",
            )),
        ]);
        assert!(store
            .put_stream(&checksum, 16, "audio/wav", stream)
            .await
            .is_err());
        let temporary = temp.path().join("blobs").join(".tmp");
        assert_eq!(std::fs::read_dir(temporary).unwrap().count(), 0);
        assert!(!store.canonical_path(&checksum).unwrap().exists());
    }

    #[tokio::test]
    async fn dropping_blob_ingest_reclaims_its_temporary_file() {
        let temp = tempfile::tempdir().unwrap();
        let store = BlobStore::new(temp.path().join("blobs"));
        let checksum = format!("{:x}", Sha256::digest(b"complete payload"));
        let stream = futures_util::stream::once(async {
            Ok::<_, std::io::Error>(Bytes::from_static(b"partial"))
        })
        .chain(futures_util::stream::pending());
        let task = tokio::spawn(async move {
            store
                .put_stream(&checksum, 16, "audio/wav", Box::pin(stream))
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        task.abort();
        let _ = task.await;

        let temporary = temp.path().join("blobs").join(".tmp");
        assert_eq!(std::fs::read_dir(temporary).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn source_probe_preserves_non_not_found_io_errors() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let looped = temp.path().join("loop.wav");
        symlink(&looped, &looped).unwrap();

        let error = resolve_operational_audio(&looped).unwrap_err();
        assert_ne!(error.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn cancelled_staging_stops_before_copying_or_finalizing() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("audetic.db");
        let source = temp.path().join("source.wav");
        std::fs::write(&source, vec![7; 128 * 1024]).unwrap();
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        assert!(stage_recording_cancellable(&db_path, &source, &cancellation).is_err());
        let root = staging_root_for_db(&db_path);
        assert!(std::fs::read_dir(root)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .all(|entry| entry.file_name() == ".lock"));
    }
}
