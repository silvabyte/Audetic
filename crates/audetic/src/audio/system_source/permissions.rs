//! macOS Screen Recording permission helpers.
//!
//! `AudioHardwareCreateProcessTap` (the CoreAudio API behind cpal's loopback
//! input on macOS) is gated by the Screen Recording TCC service. Unlike the
//! mic, the audio-tap API does *not* reliably auto-trigger a TCC prompt on
//! first use — even from a Developer-ID-signed bundle running under launchd.
//! Apple's CGRequestScreenCaptureAccess is the explicit way to ask.
//!
//! Behaviour:
//! - `is_granted()` returns the current TCC state without prompting.
//! - `request()` fires the system prompt if access is not already granted.
//!   It returns the *current* (pre-prompt) state, so the calling process
//!   does **not** immediately see the result of the user's decision; the
//!   grant only takes effect on a fresh process.
//!
//! That's why `spawn_grant_watcher_then_exit` exists: pair the request
//! call with a background task that polls preflight every couple of
//! seconds and exits the process when the state flips. Combined with the
//! LaunchAgent's `KeepAlive=true`, launchd then restarts the daemon with
//! the new TCC state and audio-tap creation works on the next attempt —
//! without the user needing to manually relaunch anything.

#![cfg(target_os = "macos")]

use std::sync::Once;
use std::time::Duration;
use tracing::{info, warn};

// CoreGraphics ships `CGRequestScreenCaptureAccess` / `CGPreflightScreenCaptureAccess`
// on macOS 11+. They're declared `CG_EXTERN bool` in CGDisplayCapture.h, so the
// Rust `bool` ABI matches.
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

pub fn is_granted() -> bool {
    unsafe { CGPreflightScreenCaptureAccess() }
}

/// Fire the TCC prompt for Screen Recording if not already granted. The
/// prompt is shown asynchronously in the user's GUI session; this function
/// returns immediately with the current grant state. Idempotent: subsequent
/// calls within the same process do not re-prompt (Apple's tccd dedupes).
pub fn request() -> bool {
    unsafe { CGRequestScreenCaptureAccess() }
}

/// Once-per-process guard so background polling doesn't get spawned more
/// than once, e.g. if multiple subsystems each try to request access.
static WATCHER_STARTED: Once = Once::new();

/// On first call: if access isn't granted yet, fires the system prompt and
/// spawns a background task that polls preflight every `poll_interval`. The
/// moment the user grants permission, the task calls `std::process::exit(0)`.
/// Pair with a supervisor that restarts the daemon (`KeepAlive=true` in the
/// LaunchAgent plist) so the new process picks up the fresh TCC state.
///
/// No-op if access is already granted or if called more than once.
pub fn spawn_grant_watcher_then_exit(poll_interval: Duration) {
    if is_granted() {
        return;
    }
    WATCHER_STARTED.call_once(|| {
        warn!(
            "Screen Recording permission not granted. Requesting via TCC — \
             accept the system prompt to enable system-audio capture in \
             meetings. The daemon will auto-restart once you grant access."
        );
        let _ = request();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(poll_interval).await;
                if is_granted() {
                    info!(
                        "Screen Recording permission granted. Exiting so \
                         launchd can restart the daemon with the new TCC state."
                    );
                    // Exit cleanly. KeepAlive=true in the plist relaunches us.
                    std::process::exit(0);
                }
            }
        });
    });
}
