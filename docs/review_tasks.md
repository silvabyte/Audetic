# Audetic Quality Review – Task Breakdown

## Scope Notes

- **Hyprland-first**: keep short-term focus on Hyprland, but avoid hard-coding that would block future compositors.
- **Wayland crates**: `wayland-client`, `wayland-protocols`, and `smithay-client-toolkit` currently unused—remove until needed.
- **API usage**: service is local-only; no requirement for multi-user concurrency beyond keeping `/status` responsive.

## High Priority

- [x] **Fix config save path bug**  
  ✅ CLI overrides were removed, so `Config::load`/`save` always target the standard `~/.config/audetic/config.toml`, eliminating the mismatch.

- [x] **Implement full recording state machine**  
  ✅ Recording management now uses the `RecordingPhase` enum and is wired through `/status`, so the legacy boolean flow is gone.

- [x] **Normalize OpenAI API output correctly**  
  ✅ Providers now vend their own `TranscriptionNormalizer` implementations, so both OpenAI paths share the same normalizer without brittle string checks.

- [ ] **Harden the binary installer + auto-updater**  
  - Add CI coverage that runs `release/cli/latest.sh --dry-run` for every supported target so broken URLs/manifests are caught before publishing.  
  - Exercise the Rust `UpdateManager` against a mocked install endpoint to verify checksum enforcement, staging behavior, and restart flow.  
  - Document the release artifact manifest schema so future providers can add new targets without reverse-engineering the current JSON.

- [ ] **Offload blocking processes from async tasks**  
  Switch whisper CLI/whisper.cpp invocations and clipboard/text-injection helpers to `tokio::process::Command` or `spawn_blocking` to avoid stalling the runtime during long recordings.

## Medium Priority

- [ ] **Redact sensitive text from logs**  
  Remove or gate the `debug!("Text to copy: …")` and `debug!("Raw transcription: …")` statements so user dictation never lands in logs unless an explicit debug flag is set.

- [x] **Trim unused UI configuration & dependencies**  
  ✅ Removed unused UI fields (`indicator_*`, `layer_shell_*`, `processing_*`) plus audio config, and dropped unused Wayland/notification crates from `Cargo.toml`.

- [ ] **Strengthen automated tests**  
  Add targeted tests for config load/save (including custom paths), provider auto-detection, clipboard fallbacks, the Axum API, and a mocked transcription pipeline to catch regressions automatically.

## Low Priority

- [ ] **Document Hyprland assumptions & future portability hooks**  
  Record in docs/README which pieces are Hyprland-specific today (hyprctl notifications, bindings) and identify seams (notification abstraction, text injection adapters) so future compositor support is straightforward.

## Architecture Review Tasks

- [ ] **Extract a recording orchestrator service**  
  Move the toggle-processing logic out of `src/main.rs` into a dedicated state machine that sequences recorder, indicator, transcription, clipboard, and injection so new commands (pause, retry, status) can reuse the same pipeline.

- [ ] **Decouple HTTP API presentation from transport**  
  Teach `ApiServer` to depend on an abstract status provider rather than embedding Waybar formatting logic in handlers; introduce view adapters so future panels/widgets do not require touching the HTTP layer.

- [ ] **Unify recording lifecycle representation**  
  Replace the ad-hoc `RecordingState` struct in `main.rs` with the enum in `audio::RecordingState`, expose richer states (Idle/Recording/Processing/Error), and publish them through both the orchestrator and `/status`.

- [ ] **Stream audio capture instead of buffering Vec<f32>**  
  Update `AudioStreamManager` to write directly to a temporary file or ring buffer using async-aware locks, avoiding blocking `std::sync::Mutex` on the Tokio runtime and preventing OOM for long recordings.

- [ ] **Push blocking commands onto dedicated executors**  
  Wrap every shell-based helper (clipboard, indicators, providers) with `tokio::process::Command` or `spawn_blocking` so `main.rs` can respond to new API events while transcription or notifications run.

- [x] **Introduce provider/normalizer capabilities**  
  ✅ `TranscriptionProvider` includes a `normalizer()` hook, letting each provider co-locate its normalization logic and removing enum-based switching in the service layer.

- [ ] **Deduplicate provider configuration building**  
  Add a `ProviderConfig::from_whisper_config(&Config)` helper so both explicit and auto-detected provider paths share one construction and new config fields can be threaded uniformly.

- [ ] **Consolidate clipboard + text injection fallbacks**  
  Create a single clipboard service that handles preservation, verification, and wl-copy/xclip/ydotool fallbacks so `ClipboardManager` and `TextInjector` stop duplicating backend selection logic.

- [ ] **Cache text-injection environment detection**  
  Persist the chosen paste strategy/backend after the first detection instead of probing `ydotool`, `wtype`, `xdotool`, etc. on every injection to cut latency and redundant command execution.

- [ ] **Make transcription pipeline interruptible**  
  Ensure “stop recording” immediately releases locks, hands work to a background task, and surfaces progress/errors via channels so `/toggle` or `/status` stay responsive even during long transcriptions.
