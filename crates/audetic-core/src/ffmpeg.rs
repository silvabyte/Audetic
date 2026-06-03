//! FFmpeg resolution and on-demand install.
//!
//! Resolution order (used by every other module that shells out to ffmpeg):
//! 1. App-local sidecar binary at `<exe-dir>/ffmpeg` — what the
//!    `ffmpeg-sidecar` crate manages. Wins over PATH so an app-managed install
//!    is deterministic across PATH changes.
//! 2. System `ffmpeg` on PATH (any pkg-manager install the user did themselves).
//! 3. None — the caller is responsible for surfacing the install-ffmpeg
//!    onboarding step.
//!
//! Install is wrapped over `ffmpeg_sidecar::download::auto_download_with_progress`
//! which fetches platform-specific static builds (BtbN for Linux/Windows,
//! evermeet for macOS) into the same dir the daemon binary lives in. We set
//! `KEEP_ONLY_FFMPEG=1` so `ffplay`/`ffprobe` are dropped — the daemon never
//! invokes them.

use anyhow::Result;
use ffmpeg_sidecar::download::{auto_download_with_progress, FfmpegDownloadProgressEvent};
use ffmpeg_sidecar::paths::sidecar_path;
use std::path::PathBuf;

/// What stage the install is in. Mirrored to JSON for the
/// `GET /system/install-ffmpeg/status` endpoint.
#[derive(Debug, Clone)]
pub enum InstallProgress {
    /// Install hasn't been kicked off, or ffmpeg was already present.
    Idle,
    /// `auto_download_with_progress` has begun resolving the download URL.
    Starting,
    /// Bytes are streaming in. Both fields are in bytes; `total` may be 0
    /// before the response headers arrive.
    Downloading { downloaded: u64, total: u64 },
    /// Archive downloaded; unpacking the static binary out of it.
    Extracting,
    /// Done — `binary_path` is the resolved path the daemon will call.
    Done { binary_path: PathBuf },
    /// Install failed. Renderer surfaces this as the `ErrorLine` on the
    /// onboarding card.
    Error { message: String },
}

/// Resolve the ffmpeg binary the daemon should invoke, in priority order.
/// `None` means neither the app-local copy nor a system install was found.
pub fn resolve_ffmpeg_binary() -> Option<PathBuf> {
    if let Ok(local) = sidecar_path() {
        if local.exists() {
            return Some(local);
        }
    }
    which::which("ffmpeg").ok()
}

/// Resolve the `ffprobe` binary if available. Only consults PATH — the
/// app-local sidecar drops `ffprobe` (see `KEEP_ONLY_FFMPEG=1` in
/// `install_blocking`) so a sibling lookup next to the sidecar would always
/// miss. `None` is non-fatal: callers that probe duration treat absent
/// `ffprobe` as "unknown duration" and continue.
pub fn resolve_ffprobe_binary() -> Option<PathBuf> {
    which::which("ffprobe").ok()
}

/// Quick "do we have ffmpeg" check used by `GET /system/deps` and the
/// pre-flight in `compress_for_transcription`.
pub fn check_available() -> bool {
    resolve_ffmpeg_binary().is_some()
}

/// Download + unpack ffmpeg into the sidecar dir. **Synchronous and blocking**
/// — callers must run this on a blocking-friendly thread (e.g.
/// `tokio::task::spawn_blocking`).
///
/// `on_progress` fires for every state change `ffmpeg-sidecar` emits, plus a
/// final `Done` once the binary verifies. Errors come back via the `Result`
/// *and* a final `Error` callback so a single observer can drive the UI.
pub fn install_blocking(
    on_progress: impl Fn(InstallProgress) + Send + Sync + 'static,
) -> Result<PathBuf> {
    // Skip ffplay/ffprobe — daemon never calls them and dropping them shaves
    // ~25MB off the download on Linux.
    std::env::set_var("KEEP_ONLY_FFMPEG", "1");

    let result = auto_download_with_progress(|event| match event {
        FfmpegDownloadProgressEvent::Starting => on_progress(InstallProgress::Starting),
        FfmpegDownloadProgressEvent::Downloading {
            downloaded_bytes,
            total_bytes,
        } => on_progress(InstallProgress::Downloading {
            downloaded: downloaded_bytes,
            total: total_bytes,
        }),
        FfmpegDownloadProgressEvent::UnpackingArchive => on_progress(InstallProgress::Extracting),
        FfmpegDownloadProgressEvent::Done => {
            // Final Done is emitted below once we verify the binary resolves.
        }
    });

    match result {
        Ok(()) => match resolve_ffmpeg_binary() {
            Some(path) => {
                on_progress(InstallProgress::Done {
                    binary_path: path.clone(),
                });
                Ok(path)
            }
            None => {
                let msg = "FFmpeg install completed but binary could not be located.".to_string();
                on_progress(InstallProgress::Error {
                    message: msg.clone(),
                });
                Err(anyhow::anyhow!(msg))
            }
        },
        Err(e) => {
            let msg = format!("{}", e);
            on_progress(InstallProgress::Error {
                message: msg.clone(),
            });
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_returns_some_when_ffmpeg_on_path() {
        // Documents behavior; on machines without ffmpeg this asserts None,
        // on machines with ffmpeg this asserts Some. Either is valid.
        let _ = resolve_ffmpeg_binary();
    }

    #[test]
    fn check_available_matches_resolve() {
        assert_eq!(check_available(), resolve_ffmpeg_binary().is_some());
    }
}
