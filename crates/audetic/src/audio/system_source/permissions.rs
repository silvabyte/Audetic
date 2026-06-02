//! macOS TCC permission helpers for Screen Recording and the Microphone.
//!
//! Two TCC services gate audio capture on macOS:
//!
//! - **Screen Recording** guards `AudioHardwareCreateProcessTap` (the CoreAudio
//!   API behind cpal's loopback input). Unlike the mic, the audio-tap API does
//!   *not* reliably auto-trigger a TCC prompt on first use — even from a
//!   Developer-ID-signed bundle under launchd. `CGRequestScreenCaptureAccess`
//!   is the explicit way to ask.
//! - **Microphone** guards `default_input_config()` / opening an input audio
//!   unit (dictation + meeting mic). `AVCaptureDevice` is the only way to
//!   preflight/request it without actually opening a stream.
//!
//! In both cases `*_request()` fires the system prompt but the grant only takes
//! effect on a fresh process. That's why `spawn_grant_watcher_then_exit` exists:
//! it requests whatever's missing, then polls until *both* grants are present
//! and `std::process::exit(0)`s. Combined with the LaunchAgent's
//! `KeepAlive=true`, launchd restarts the daemon with the fresh TCC state and
//! capture works on the next attempt — no manual relaunch.

#![cfg(target_os = "macos")]

use std::sync::Once;
use std::time::Duration;
use tracing::{info, warn};

use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};

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

/// The `AVMediaTypeAudio` constant. It's weak-linked (`Option`), so resolve it
/// once; `None` would mean AVFoundation didn't export the symbol, in which case
/// we can't reason about mic access and treat it as ungranted.
fn audio_media_type() -> Option<&'static objc2_foundation::NSString> {
    unsafe { AVMediaTypeAudio }
}

/// Current Microphone TCC state, without prompting.
pub fn mic_is_granted() -> bool {
    // Class method, no prompt — the mic analog of `CGPreflightScreenCaptureAccess`.
    let Some(media_type) = audio_media_type() else {
        return false;
    };
    let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media_type) };
    status == AVAuthorizationStatus::Authorized
}

/// Fire the TCC prompt for the Microphone if access is undetermined. Like the
/// Screen Recording request, the result isn't visible to this process — the
/// grant takes effect on a fresh process (hence the watcher + exit-to-restart).
///
/// Requires `NSMicrophoneUsageDescription` in the bundle's Info.plist (embedded
/// via `build.rs`); the prompt is shown in the user's GUI session.
pub fn mic_request() {
    let Some(media_type) = audio_media_type() else {
        return;
    };
    // `requestAccessForMediaType:completionHandler:` delivers the user's
    // decision via the block on an arbitrary queue. We don't need the result
    // (the watcher polls `mic_is_granted`), so the handler is a no-op. The
    // system copies the block (Block_copy), so the `RcBlock` can drop here.
    let handler = RcBlock::new(|_granted: Bool| {});
    unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(media_type, &handler) };
}

/// Once-per-process guard so background polling doesn't get spawned more
/// than once, e.g. if multiple subsystems each try to request access.
static WATCHER_STARTED: Once = Once::new();

/// On first call: if either Screen Recording or Microphone access isn't granted
/// yet, fires the system prompt(s) for whatever's missing and spawns a
/// background task that polls every `poll_interval`. The moment *both* grants
/// are present, the task calls `std::process::exit(0)`. Pair with a supervisor
/// that restarts the daemon (`KeepAlive=true` in the LaunchAgent plist) so the
/// new process picks up the fresh TCC state.
///
/// No-op if both are already granted or if called more than once.
pub fn spawn_grant_watcher_then_exit(poll_interval: Duration) {
    let screen_ok = is_granted();
    let mic_ok = mic_is_granted();
    if screen_ok && mic_ok {
        return;
    }
    WATCHER_STARTED.call_once(|| {
        if !screen_ok {
            warn!(
                "Screen Recording permission not granted. Requesting via TCC — \
                 accept the system prompt to enable system-audio capture in \
                 meetings. The daemon will auto-restart once you grant access."
            );
            let _ = request();
        }
        if !mic_ok {
            warn!(
                "Microphone permission not granted. Requesting via TCC — accept \
                 the system prompt to enable dictation and meeting mic capture. \
                 The daemon will auto-restart once you grant access."
            );
            mic_request();
        }
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(poll_interval).await;
                if is_granted() && mic_is_granted() {
                    info!(
                        "Screen Recording + Microphone permissions granted. \
                         Exiting so launchd can restart the daemon with the new \
                         TCC state."
                    );
                    // Exit cleanly. KeepAlive=true in the plist relaunches us.
                    std::process::exit(0);
                }
            }
        });
    });
}
