//! Media file inspection (duration probing) abstraction.
//!
//! The import path needs a duration before kicking off the pipeline so the
//! meeting row carries the same `duration_seconds` field a live recording
//! would. The default implementation shells out to `ffprobe` (which ships
//! alongside the `ffmpeg` sidecar already required for compression); tests
//! inject a fake.

use async_trait::async_trait;
use std::path::Path;
use std::process::Command;
use tracing::warn;

use crate::system::ffmpeg::resolve_ffprobe_binary;

/// Inspects media files for metadata.
#[async_trait]
pub trait MediaInspector: Send + Sync {
    /// Returns the duration of `path` in whole seconds, or `None` if the
    /// duration can't be determined. Never returns an error — duration is
    /// non-essential metadata; the pipeline runs fine without it.
    async fn probe_duration_seconds(&self, path: &Path) -> Option<u64>;
}

/// Production inspector: invokes `ffprobe` to read container metadata.
pub struct FfprobeMediaInspector;

#[async_trait]
impl MediaInspector for FfprobeMediaInspector {
    async fn probe_duration_seconds(&self, path: &Path) -> Option<u64> {
        let ffprobe = resolve_ffprobe_binary()?;
        let path = path.to_path_buf();

        tokio::task::spawn_blocking(move || {
            let output = Command::new(&ffprobe)
                .args([
                    "-v",
                    "quiet",
                    "-show_entries",
                    "format=duration",
                    "-of",
                    "csv=p=0",
                ])
                .arg(&path)
                .output()
                .ok()?;

            if !output.status.success() {
                warn!(
                    "ffprobe failed for {:?}: {}",
                    path,
                    String::from_utf8_lossy(&output.stderr)
                );
                return None;
            }

            let stdout = String::from_utf8_lossy(&output.stdout);
            let seconds = stdout.trim().parse::<f64>().ok()?;
            Some(seconds.max(0.0) as u64)
        })
        .await
        .ok()
        .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    /// Test inspector that returns a fixed duration without touching disk or ffprobe.
    pub struct StubInspector {
        pub duration: Option<u64>,
        pub calls: Arc<Mutex<Vec<std::path::PathBuf>>>,
    }

    impl StubInspector {
        pub fn new(duration: Option<u64>) -> Self {
            Self {
                duration,
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl MediaInspector for StubInspector {
        async fn probe_duration_seconds(&self, path: &Path) -> Option<u64> {
            self.calls.lock().await.push(path.to_path_buf());
            self.duration
        }
    }

    #[tokio::test]
    async fn stub_inspector_returns_configured_value() {
        let inspector = StubInspector::new(Some(123));
        let dur = inspector
            .probe_duration_seconds(Path::new("/tmp/whatever.mp3"))
            .await;
        assert_eq!(dur, Some(123));
        assert_eq!(inspector.calls.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn stub_inspector_returns_none() {
        let inspector = StubInspector::new(None);
        let dur = inspector
            .probe_duration_seconds(Path::new("/tmp/whatever.mp3"))
            .await;
        assert_eq!(dur, None);
    }
}
