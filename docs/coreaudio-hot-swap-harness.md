# CoreAudio hot-swap hardware harness

This is the durable manual real-hardware test for Audetic's macOS Default Input
and Default Output hot-swap behavior. It is intentionally not a hardware CI
test. The Rust driver controls the existing daemon HTTP API, while a Swift
holder process owns one public aggregate device with a fresh UUID identity for
the run.

The harness writes `report.json`, `report.txt`, the relevant daemon log slice,
marker WAVs, and copied output WAVs under
`target/coreaudio-hot-swap/<run-id>/`.

## Prerequisites

- macOS 14.6 or newer, full Xcode selected with `xcode-select`, and `xcrun swift` available.
- The daemon built from the current checkout and running on `127.0.0.1:3737`.
- Microphone and Screen & System Audio Recording permission granted to that daemon build.
- `lsof` and `afplay`, both included with macOS.
- Headphones for the Mac's Default Output and a separate phone or tone generator for the microphone marker. Headphones prevent the System Tap marker from leaking acoustically into the microphone.
- A removable USB microphone or audio interface for `--mode degraded`.
- Set `behavior.delete_audio_files = false` in the active Audetic config and restart the daemon before testing. The driver must copy the dictation WAV before any processing cleanup.
- Start each mode with no active dictation or meeting. Leave meetings in Review until their run directory has been inspected; cancel them afterward if they are not useful.

List the physical CoreAudio UIDs before choosing subdevices:

```bash
cargo run -p audetic --example coreaudio_hot_swap --release -- --list-devices
```

The output is one JSON object. Choose an entry with `has_input: true` for
`--input-uid` and one with `has_output: true` for `--output-uid`. If omitted,
the holder wraps the defaults present when it starts.

Run the daemon with normal info logging. The harness records the structured
`audio_device_switch_settled`, `audio_segment_started`,
`audio_segment_closed`, `audio_capture_recovery`, `audio_silence_fill`, and
`meeting_audio_output` events. These include source, Stream Generation, native
and canonical frame counts, rates, and Silence Fill lengths.

## Holder lifecycle

`scripts/coreaudio_aggregate_holder.swift` creates a public aggregate named
`Audetic Hot Swap <run-id>` with a random UUID UID. The process prints a JSON
`ready` record, accepts JSON commands on stdin, and owns the aggregate for its
lifetime. It restores the original input, output, and system-output defaults
before destroying the aggregate on a normal command, stdin EOF, SIGINT,
SIGTERM, or SIGHUP. Readiness is emitted only after the UUID is visible in the
CoreAudio device list with input and output streams. Default-change commands
poll CoreAudio readback before replying, and command-driven teardown reports
`destroyed` only after the UUID disappears from the device list.

If the Rust driver crashes, its pipe closes and the holder tears down on EOF.
SIGKILL cannot run cleanup in either process, but every invocation uses a new
UUID and never refers to an earlier aggregate ID. A stale CoreAudio object can
therefore never be mistaken for the current run; restart `coreaudiod` or log
out if macOS itself retains a stale object.

## Idle flow

This changes Default Input while Audetic is idle, waits for the Settled Switch,
then starts dictation. The daemon must open the run's aggregate without being
restarted.

```bash
cargo run -p audetic --example coreaudio_hot_swap --release -- \
  --mode idle \
  --input-uid '<physical-input-uid>' \
  --idle-mic-marker-hz 697
```

When prompted, play the generated 697 Hz marker into the microphone from a
separate device, stop playback, and press Return. The report checks all of the
following:

- a Settled Switch was observed before dictation;
- the dictation device log names this run's aggregate;
- the copied dictation WAV contains the microphone marker;
- the daemon PID is unchanged;
- original defaults were restored.

## Live flow

This starts dictation and a meeting, records one microphone/System Tap marker
pair through the original defaults, changes both defaults, waits until all
three replacement capture legs name the new aggregate, then records a second
marker pair. It exercises active dictation input, active meeting microphone
input, and active meeting System Tap output on both sides of one replacement.

```bash
cargo run -p audetic --example coreaudio_hot_swap --release -- \
  --mode live \
  --input-uid '<physical-input-uid>' \
  --output-uid '<physical-output-uid>' \
  --live-pre-mic-marker-hz 697 \
  --live-post-mic-marker-hz 770 \
  --live-pre-system-marker-hz 941 \
  --live-post-system-marker-hz 1209
```

For each phase, start the prompted microphone marker on a separate device aimed
at the microphone. The driver plays that phase's system marker through
`afplay`; keep the Mac output on headphones. Stop the external marker when
prompted. The default frequencies are 697/941 Hz before the switch and
770/1209 Hz after it.

All four live frequencies must be distinct and below 8 kHz; the driver rejects
an ambiguous marker configuration before creating the aggregate. Idle mode
creates only its `idle-mic-marker-*` file and uses only
`--idle-mic-marker-hz`.

Post-switch prompts are not shown until new, source-specific daemon log lines
prove that dictation, meeting microphone, and System Tap each opened this
run's aggregate. The report requires `mic_pre` and `mic_post` in the final
dictation, and `mic_pre`, `mic_post`, `system_pre`, and `system_post` in the
final meeting output. Marker files use corresponding `live-pre-*` and
`live-post-*` names.

Marker analysis proves that the expected frequencies reached the final files.
It cannot prove electrical source isolation. Speaker playback can leak into the
microphone and produce a false source attribution, which is why headphones and
an external microphone marker are required.

## Mixed native rates and playback speed

Use hardware whose old and aggregate defaults expose different native rates,
commonly 44.1 kHz and 48 kHz. Pass every rate that must appear in Segment
telemetry and measure the interval from meeting start to stop with a stopwatch:

```bash
cargo run -p audetic --example coreaudio_hot_swap --release -- \
  --mode live \
  --input-uid '<44.1-khz-input-uid>' \
  --output-uid '<48-khz-output-uid>' \
  --expect-native-rates 44100,48000 \
  --expected-duration-secs 12 \
  --duration-tolerance-secs 0.75
```

The rate assertions prove that the requested native rates occurred. Final WAV
duration checks timing, while all four phase-specific markers remaining at
their configured frequencies check playback pitch/speed before and after
per-Segment normalization. Listen to the copied WAV as a final operator check;
the report does not claim perceptual playback quality from duration alone.

## Degraded Capture and recovery

Use a removable input subdevice. The aggregate must be active before capture
starts. Follow the prompts exactly: unplug the selected device while dictation
and meeting capture are live, wait for both status endpoints to report
`capture_degraded: true`, reconnect it, and let the driver restore Default
Input to trigger recovery.

```bash
cargo run -p audetic --example coreaudio_hot_swap --release -- \
  --mode degraded \
  --input-uid '<removable-input-uid>'
```

Speak briefly before unplugging and after reconnecting so both copied outputs
contain real samples. A device or driver that remains open while physically
disconnected does not demonstrate an absent/unopenable device; the run should
fail its Degraded Capture assertion rather than claiming success. Use hardware
whose CoreAudio stream actually dies, or power the interface off completely.

The report requires Degraded Capture and subsequent recovery for both
dictation and meeting status, plus an unchanged daemon PID. Meeting Silence
Fill and output gap metrics are recorded for inspection.

## Reading results

`report.txt` is the quick pass/fail summary. `report.json` is the complete
machine-readable record of status transitions, aggregate identity and physical
subdevices, assertions, WAV lengths, RMS/peak values, silence gaps, and marker
amplitudes. `daemon.log` contains the exact run slice used to report Settled
Switches, Stream Generations, Segments, rates, frame counts, recovery states,
and Silence Fill.

A failed assertion makes the example exit nonzero after writing all reports and
restoring defaults. Preserve the entire run directory when attaching results
to a regression report.
