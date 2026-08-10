# Audio Capture

Audio Capture follows system-selected devices and turns their native audio into canonical audio for dictations and meetings.

## Language

**Default Input**:
The system-selected microphone that a new or swapped microphone capture follows.
_Avoid_: Preferred mic, cached input device

**Default Output**:
The system-selected playback device whose audio a System Tap follows.
_Avoid_: Preferred output, pinned output device

**Device Switch**:
A change to the system Default Input or Default Output.
_Avoid_: Device failure, reconnect

**Settled Switch**:
A Device Switch emitted after transient device churn has quieted.
_Avoid_: Raw device notification

**Segment**:
A contiguous interval captured from one device at one native sample rate.
_Avoid_: Chunk, buffer

**Hot Swap**:
Closing the current Segment and continuing an active capture with the current default device while preserving prior audio.
_Avoid_: Restart, reconnect

**Capture Backend**:
The boundary that opens a current default device and delivers its native-rate samples without exposing platform audio APIs.
_Avoid_: Device manager, cpal wrapper

**Device Watcher**:
The single daemon-level source of Default Input and Default Output changes.
_Avoid_: Poller, capture watcher

**Stream Generation**:
A monotonically increasing identity for a live stream that distinguishes current failures from stale failure reports.
_Avoid_: Stream ID, retry count

**Degraded Capture**:
A live recording session that is temporarily receiving no audio because its required device cannot be opened.
_Avoid_: Failed recording, stopped capture

**Silence Fill**:
Canonical zero-valued audio representing a meeting capture gap so independently captured tracks remain aligned.
_Avoid_: Padding, dead air

**System Tap**:
Capture of audio being played through the Default Output for the meeting system-audio track.
_Avoid_: Loopback mic, output recording
