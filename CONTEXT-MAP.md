# Context Map

## Contexts

- [Audio Capture](./crates/audetic/src/audio/CONTEXT.md) - acquires and normalizes microphone and system audio
- [Audio Notes](./crates/audetic/src/audio_notes/CONTEXT.md) - one durable captured input with its transcript, classification, and artifacts

## Relationships

- **Audio Capture → Audio Notes**: supplies canonical, aligned source tracks to one capture lifecycle.
- **Audio Notes → Note Intelligence**: persisted transcripts feed classification and a processor registry; failures preserve the original note.
- **Note Intelligence → Post-processing**: successful enrichment emits `audio_note.completed` for configured downstream jobs.
