# macOS system audio via cpal loopback

Plan to replace `pw-cat` system-audio capture with cpal-native loopback on macOS. Resolves the §1b Blocker in `macos-port.md`.

## Reality check

- **PR**: [RustAudio/cpal#1003](https://github.com/RustAudio/cpal/pull/1003), merged Sep 2025.
- **Released in**: cpal `0.17.0` (Dec 2025). Latest is `0.17.3` (Feb 2026).
- **Audetic today**: pinned to `cpal = "0.15"` at `crates/audetic/Cargo.toml:22`.
- **Minimum macOS**: 14.6 per the 0.17.0 changelog (PR description originally claimed 14.2; trust the changelog).
- **Permission**: System Audio Recording (a sub-category of Screen Recording in macOS 15+; on 14.x the user grants Screen Recording outright). cpal triggers it internally when the audio tap is created — no extra plumbing needed beyond the Info.plist key and a code-sign identity.
- **Known cpal bug** (per PR thread): when the output device has >2 channels, the captured volume is halved per channel-count multiplier. We mono-mix downstream anyway so this is cosmetic for us but worth a note.

## API shape

In 0.17 the loopback device *is* the default output device. There is no separate enumerate path:

```rust
let host = cpal::default_host();
let device = host.default_output_device().context("no default output")?;
let config = device.default_output_config()?;       // 48 kHz f32 stereo typically
let stream = device.build_input_stream(
    &config.into(),
    move |data: &[f32], _| { /* push */ },
    move |err|             { /* report */ },
    None,
)?;
stream.play()?;
```

The same call on a 14.5 machine returns `BuildStreamError::DeviceNotAvailable` (or similar from the tap-creation OSStatus). The same call without permission silently delivers a stream of zeros until the user grants it — there is no "denied" error path. We need to either ping for permission up-front or detect silence.

## Plan

### 1. Bump cpal to 0.17.3

- `crates/audetic/Cargo.toml:22` → `cpal = "0.17"`. Verify the workspace builds on Linux first (this is a major bump).
- 0.16 → 0.17 breaking changes that touch our code:
  - `default_input_device()` and `default_output_device()` are now lazy — they no longer fail at enumeration if no device is present. Affects `mic_source.rs:31`, `audio_stream_manager.rs:39`. The `.context("...")` chain still works.
  - `DeviceTrait::id()` is new; `device.name()` semantics unchanged for our uses.
  - `StreamError::StreamInvalidated` and `StreamError::BufferUnderrun` are new variants. If we exhaustively match `StreamError` anywhere (grep needed), add arms. Best-effort log-and-continue is fine.
- Expect linker churn on Linux from cpal's ALSA bump. CI gates this.

### 2. Split `SystemAudioSource` into platform impls

Current `crates/audetic/src/audio/system_source.rs` is 100% Linux. Two reasonable shapes:

- **(a)** Keep `SystemAudioSource` as a thin enum that dispatches at runtime to a `LinuxPwCat` or `MacosCpalLoopback` inner. Lets the trait `AudioSource` stay untouched.
- **(b)** Two `#[cfg(target_os = "...")]` modules exporting the same struct name. More idiomatic for cpal-style code, less ceremony, but means the Linux and macOS impls share zero code (which they don't anyway).

Go with **(b)**. The two impls share no state; an enum just hides that.

```
crates/audetic/src/audio/system_source/
├── mod.rs           // pub use platform::SystemAudioSource;
├── linux.rs         // current pw-cat code, moved verbatim
└── macos.rs         // new cpal-based impl
```

The `AudioSource` trait contract is unchanged: `start()`, `stop() -> Vec<f32>`, `is_active()`, `sample_rate() -> u32`. Macos impl returns samples at `target_sample_rate` (after resampling — see §3).

### 3. Resampling — non-optional

cpal hands us samples at the *output device's* rate (48 kHz on every Mac I've ever owned). The VTT pipeline wants 16 kHz mono f32. Options:

1. **`rubato` crate** — well-maintained, supports streaming SincFixedIn resampler. Pulls in a few extra deps. Right answer.
2. **Linear/decimation by hand** — 48k → 16k is a clean 3:1 decimation; with a basic FIR lowpass it's a 30-line function. Tempting, but voice quality matters and we'd be reinventing rubato badly.
3. **Hope the device runs at 16k** — won't. CoreAudio output devices are 44.1/48/96.

Use rubato. Wrap it inside the macOS impl; the API surface (`stop() -> Vec<f32>` at target rate) stays the same.

Also need to handle channel mixing: cpal config will be stereo. Average L/R into mono inside the stream callback before pushing to the resampler. Same trick as the existing mic path.

### 4. Permission UX

There is no "permission denied" callback. The flow we want:

1. On `start()`, call `build_input_stream` + `play()`. macOS shows the prompt the first time only.
2. After ~500 ms, check the running sample buffer. If it's all zeros (or has been silent for the entire window) **and** we know the user is actively playing audio… we can't actually know that.

Workable compromise: on first install (or on first system-recording attempt that produces only silence for >2 s), surface a one-time "Open System Settings → Privacy & Security → Screen Recording" hint via the existing notifier. Don't block recording — the mic track is still being captured in parallel.

Optional: at install time, fire a 100 ms dummy capture to trigger the prompt before the user ever starts a real meeting. Trades surprise-during-recording for a startup permission dance. Worth doing.

### 5. Info.plist + entitlements

The daemon binary needs a real `Info.plist` baked in (today there is none — `crates/audetic/build.rs` only builds the SPA). Required keys for macOS:

- `NSMicrophoneUsageDescription` — already required by §5 of the audit for the mic path.
- `NSScreenCaptureUsageDescription` — required for the loopback path. macOS will surface this as the prompt body.

Two ways to attach: (a) link `Info.plist` as a section via `cargo:rustc-link-arg=-Wl,-sectcreate,__TEXT,__info_plist,...` in `build.rs`, or (b) bundle the daemon inside a real `.app` for distribution. (a) is simpler and matches how the daemon ships today (raw binary, not an app bundle). Do (a).

Code signing: ship signed + notarized binaries from day one (Team ID **Z25737G79K**). The flow:

```
codesign --sign "Developer ID Application: … (Z25737G79K)" \
  --options runtime \
  --entitlements crates/audetic/macos/audetic.entitlements \
  --timestamp \
  target/release/audetic

xcrun notarytool submit audetic.zip \
  --keychain-profile audetic-notary --wait

xcrun stapler staple target/release/audetic
```

Entitlements file needs at minimum `com.apple.security.device.audio-input` and `com.apple.security.device.camera` is NOT needed; for the audio tap there is no dedicated entitlement — the runtime grant via `NSScreenCaptureUsageDescription` is what authorizes it. Wire the notarytool credentials into the release workflow (`scripts/release/deploy.ts`) as a keychain profile populated from CI secrets (`AC_APPLE_ID`, `AC_TEAM_ID=Z25737G79K`, `AC_PASSWORD` app-specific password).

### 6. Fallback when unsupported

Build-time `cfg` gate is `target_os = "macos"`. Runtime gates:

- macOS < 14.6 → `build_input_stream` returns `Err`. Log a warning, set `active = true` with no stream (mirrors current pw-cat-missing behavior at `system_source.rs:70-77`), return empty samples on `stop()`.
- No audio tap permission → silence stream (handled in §4).
- No default output device → `default_output_device()` returns `None`. Same degrade path.

The contract "if system audio is unavailable, the meeting still records mic" is preserved.

### 7. Test strategy

- **CI** can't grant Screen Recording. Mark integration tests with `#[ignore]` and a `// run locally on macOS` comment, like the cpal upstream does.
- **Local smoke test**: play a known sine-tone YouTube clip, run `audetic`, verify the resulting meeting WAV contains both mic and system audio (rough FFT or eyeballing the waveform is fine).
- **Volume bug regression**: capture from a Mac with a 6-channel output (e.g. an external interface in surround mode). If the level looks 4× too quiet, document and ship anyway — upstream issue.
- **Edge case**: user changes default output device mid-recording. cpal will keep streaming from the *original* device; the stream does not migrate. Acceptable for v1.

### 8. Sequence

1. Bump cpal to 0.17, get the workspace compiling on Linux. (~2 h, mostly chasing enum exhaustiveness.)
2. Move `system_source.rs` → `system_source/linux.rs`, add empty `system_source/macos.rs` stub returning silence. Verify Linux behavior unchanged. (~1 h.)
3. Implement macOS impl: cpal stream, stereo→mono, rubato resample, stop-and-drain. (~half day.)
4. Wire `Info.plist` section into `build.rs`. (~1 h.)
5. Add the dummy-capture permission warmup at install time, in `crates/audetic/src/install/mod.rs`. (~1 h.)
6. Hands-on test on a real Mac (15.x, then 14.6 VM if available). (~half day, hardware-bound.)
7. Wire Developer ID signing + notarization (Team Z25737G79K) into `scripts/release/deploy.ts` for darwin targets. Write `crates/audetic/macos/audetic.entitlements`. Import cert into a CI keychain step. (~half day — most of the time is the first notarytool round-trip getting rejected for something dumb like a missing hardened-runtime flag.)

Total: ~2.5 working days assuming a Mac is on the desk.

## Open questions

- ~~**Codesign source of truth**~~: resolved — Developer ID Application signing + notarization with Team ID **Z25737G79K**, done at release time via `scripts/release/deploy.ts`. Requires CI secrets: `AC_APPLE_ID`, `AC_PASSWORD` (app-specific), `AC_TEAM_ID=Z25737G79K`, plus the Developer ID cert imported into a temporary keychain on the macOS runner.
- **macOS 14.0-14.5 support**: drop entirely with a clean error message, or attempt some other path (ScreenCaptureKit direct)? The user base for "Mac on Sonoma point-release < 14.6" is small; drop it.
- **Rubato variant**: `SincFixedIn` (better quality, more CPU) vs `FastFixedIn` (cheaper). The VTT model down-samples internally anyway; `FastFixedIn` is almost certainly fine. Pick `FastFixedIn`, revisit if transcription quality drops.
