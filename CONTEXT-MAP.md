# Context Map

## Contexts

- [Audio Capture](./crates/audetic/src/audio/CONTEXT.md) - acquires and normalizes audio for dictations and meetings
- [Library Sync](./crates/audetic/src/sync/CONTEXT.md) - gathers records from a person's Audetic devices into one shared library
- [Meetings](./crates/audetic/src/meeting/CONTEXT.md) - records, transcribes, and organizes conversations

## Relationships

- **Audio Capture -> Recording**: supplies canonical microphone audio for dictation jobs
- **Audio Capture -> Meetings**: supplies aligned microphone and system-audio tracks for meeting transcription
- **Meetings -> Library Sync**: contributes completed meeting records and optional recordings to the Shared Library
