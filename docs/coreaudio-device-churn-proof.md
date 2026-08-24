# CoreAudio device churn proof

This manual harness proves Audetic's Default Input and Default Output recovery
against real CoreAudio behavior. It is intentionally not a CI test.

The Rust driver controls an installed, signed daemon through its existing HTTP
API. The Swift `audiodev` holder creates public, uniquely identified CoreAudio
aggregate devices and changes system defaults. A successful run exercises this
production path:

```text
CoreAudio default change
  -> CoreAudio property listener
  -> Device Watcher trailing debounce
  -> daemon Command Loop
  -> cpal input or macOS System Tap replacement
  -> Segment normalization and meeting Silence Fill
  -> status handle and production WAV writer
```

The harness does not inject Settled Switches, call replacement methods, use a
fake Capture Backend, or read daemon state directly. Input scenarios use real
physical microphones through holder-owned aggregate identities and Audetic's
normal cpal Default Input adapter.

## Prerequisites

- macOS 14.6 or newer.
- Xcode Command Line Tools with Swift.
- The built-in or another physical Default Output must support playback.
- One physical input with at least two discrete supported native rates. A second
  physical input with a different native rate can be selected explicitly.
  Position the selected input to receive the output's markers, or use a
  deterministic hardware loopback.
- Microphone and Screen Recording/System Audio Recording permission for the
  installed `Audetic.app`.
- No active dictation or meeting.
- Local playback is audible. Non-preflight modes temporarily set output to 40%
  and unmute it so acoustic markers are deterministic.

Use the normal install path so TCC attributes capture to the app bundle:

```bash
make install
make macos-device-switch-repro
```

An ad-hoc rebuild changes Audetic's cdhash and may make macOS request both
permissions again. Grant them in System Settings, let launchd restart the
daemon, and run `preflight` before changing any devices. The Rust driver and
Swift holder do not capture audio themselves, so they do not need Audetic's
capture entitlements.

## Run

Run the non-mutating checks first:

```bash
cargo run -p audetic --example device_switch_repro --release -- preflight
```

Run every proof scenario:

```bash
cargo run -p audetic --example device_switch_repro --release -- all
```

Individual modes isolate failures:

```bash
cargo run -p audetic --example device_switch_repro --release -- idle
cargo run -p audetic --example device_switch_repro --release -- live-dictation
cargo run -p audetic --example device_switch_repro --release -- live-meeting-mic
cargo run -p audetic --example device_switch_repro --release -- live-meeting-system
cargo run -p audetic --example device_switch_repro --release -- degraded
cargo run -p audetic --example device_switch_repro --release -- churn
```

Use `--physical-output-uid` only when the original Default Output is not the
physical device that should back the disposable output aggregates. Override
input auto-selection with `--physical-input-a-uid` and
`--physical-input-b-uid`. `preflight` records device names and redacted UIDs so
a selection can be made without placing a machine identifier in committed
evidence. The default 90-second timeout accommodates CoreAudio device opens
that block before returning an unavailable-device error; override it with
`--settle-timeout-seconds` only when diagnosing a known faster topology.

Each invocation prints its absolute artifact directory twice: immediately
after creation and after cleanup. The default root is
`target/device-switch-runs`; override it with `--artifacts-root`.

## Marker topology

Dictation and meeting-microphone scenarios normally create two unique aggregate
identities around one physical input at two discrete supported native rates,
then play acoustic or hardware-looped markers. Audetic captures 697 Hz through
input A before the switch and 1009 Hz through input B afterward. Passing
`--physical-input-b-uid` instead uses a second physical input. Preflight rejects
topologies whose selected native rates are equal.

The meeting-microphone scenario also plays a continuous 311 Hz output reference
that the production System Tap captures. The final mixed WAV must contain the
continuous reference, ordered microphone markers, and logged microphone Silence
Fill.

The meeting-System-Tap scenario creates one uninterrupted physical microphone
reference Segment while the marker player reaches the physical output. Two
disposable output aggregates wrap that output device, and the harness changes
Default Output between them. The reference proof uses the microphone Segment's
generation and canonical sample count rather than relying on a player that may
itself stop when Default Output changes.

At least one `all` run must observe two native rates in production Segment-open
events. Input aggregates retain their physical devices' distinct native rates;
output aggregates request 44.1 kHz and 48 kHz. The harness reports an actionable
failure when the selected topology cannot provide a mixed-rate transition.

For the degraded proof, Audetic first moves from aggregate A to B. The holder
then takes CoreAudio hog mode on A's inactive physical subdevice and selects A
as Default Input. Audetic's real stream build blocks and returns the platform's
unavailable-device error until the bounded retry ladder enters Degraded Capture.
The harness requires `capture_degraded: true`, releases hog mode, selects B, and
then requires `capture_degraded: false` without ending the dictation. It never
substitutes an injected daemon event.

## Evidence

Every run directory contains:

- `manifest.json` with mode, run UID, timestamps, redacted identities, native
  rates, status transitions, capture events, assertions, and final outcome.
- `daemon.log`, containing only the suffix produced during this run.
- `helper.stderr.log`.
- Generated marker/reference WAV files.
- Copied canonical 16 kHz capture WAV files.
- Per-scenario JSON marker analysis with offsets, energy, pitch, sample count,
  duration, and gap measurements.

Passing assertions require ordered markers, explicit RMS/peak thresholds,
frequency within 2%, canonical sample counts consistent with Segment logs,
meeting Silence Fill, one settled replacement for churn, observed degraded and
recovered statuses, and an unchanged launchd daemon PID.

Do not commit run directories. They can contain captured local audio and device
names. The committed guide and PR summary should contain only commands and a
redacted GREEN assertion summary.

## Cleanup and failure behavior

The helper snapshots original defaults before mutation. On success, assertion
failure, Ctrl-C, stdin EOF, or a catchable termination signal it:

1. Releases any physical subdevice hog mode owned by the holder.
2. Restores original Default Input and Default Output and waits for readback.
3. Restores changed nominal rates.
4. Destroys only aggregates and taps created by this holder, in reverse order.
5. Waits for each aggregate/tap to disappear.

Every aggregate name and UID includes a fresh run UUID. The helper refuses to
destroy resources whose current UID does not match its ownership record. The
Rust driver bounds helper calls, status polls, watcher settlement, marker
playback, daemon log waits, and child shutdown, then kills and reaps children if
normal shutdown does not finish.

The Rust driver separately snapshots macOS output volume before mutation. It
restores that exact level and mute state only if the current setting still
matches the harness's temporary 40% unmuted value; a user change made during the
run wins and is not overwritten. `preflight` reads but never changes volume.

`SIGKILL`, power loss, and CoreAudio service failure cannot run process cleanup.
Public aggregates can then remain until CoreAudio removes them. A later run
never reuses their UIDs; remove any visibly stale `Audetic Repro` aggregate in
Audio MIDI Setup after confirming no harness holder is running. If the holder
dies while its aggregate is still the default, CoreAudio normally chooses an
available fallback, but that fallback is an OS behavior rather than a cleanup
guarantee.
