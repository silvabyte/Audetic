# Context Map

## Contexts

- [Audio Capture](./crates/audetic/src/audio/CONTEXT.md) - acquires and normalizes audio for dictations and meetings

## Relationships

- **Audio Capture -> Recording**: supplies canonical microphone audio for dictation jobs
- **Audio Capture -> Meetings**: supplies aligned microphone and system-audio tracks for meeting transcription
