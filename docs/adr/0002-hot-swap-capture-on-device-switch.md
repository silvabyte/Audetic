# Hot-swap capture on default-device changes

Status: accepted

Implementation is staged across the hot-swap Fizzy cards: resolving defaults for fresh dictations and exact Segment normalization land first; command-loop swaps, recovery, meeting adoption, and platform watchers follow in later slices.

Audetic follows the system Default Input and Default Output rather than pinning device identities. Capture objects must resolve the current default for every new session, and active sessions will replace streams after settled default-device changes or stream deaths so device churn never requires a daemon restart. This decision is ADR 0002 because ADR 0001 was assigned to source-only distribution before the original hot-swap design artifact was restored from Fizzy card #99.

## Decision

A recording is a sequence of native-rate Segments. A Hot Swap closes the current Segment, normalizes it to the 16 kHz pipeline rate, preserves it in the canonical buffer, and opens the next Segment from the current default device. Normalization trims resampler delay and partial-chunk padding so each Segment contributes exactly its source duration. Dictations concatenate Segments without filling capture gaps; meetings use Silence Fill so microphone and System Tap tracks remain positionally aligned.

A Capture Backend hides device resolution and stream construction while delivering samples to capture callbacks. The cpal implementation is a thin adapter and remains lazy because reading the default microphone configuration can gate on macOS microphone permission. Constructors, daemon boot, and Device Watcher startup must not open an audio device.

One daemon-level Device Watcher owns platform notification APIs and emits trailing-debounced Settled Switches. macOS uses CoreAudio default-device property listeners; Linux initially uses a no-op watcher until a PipeWire implementation exists. Device Switch events enter the existing daemon Command Loop so swaps serialize with start and stop operations.

Stream error callbacks report the source and Stream Generation to the same Command Loop. Reports for stale generations are ignored. A swap retries stream construction briefly; after retry exhaustion the recording remains alive in Degraded Capture and attempts recovery on the next Device Switch or stream-death event. Device trouble does not discard already captured audio or terminate a recording session.

## Consequences

- Dictation, meeting microphone, and System Tap capture share follow-default semantics and the Capture Backend boundary.
- Recording and meeting status contracts expose whether capture is degraded; user-interface rendering is separate work.
- Mid-session swapping, failure recovery, watcher backends, and meeting alignment can land incrementally without reintroducing cached device handles.
- Hardware-free fakes are the primary regression seam; real cpal and CoreAudio behavior is verified with a manual device-switch harness.
