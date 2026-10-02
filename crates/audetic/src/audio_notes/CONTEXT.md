# Audio Notes

An Audio Note is any captured audio and its durable transcript. Capture source
(microphone, microphone plus system audio, or import) does not determine meaning.
Classification infers meeting, dictation, conversation, request, or other kinds
after persistence. Processors create independent artifacts without overwriting
the raw transcript. See `docs/audio-notes.md` for the extension points.

## Language

**Note Title**:
The canonical human-readable label for an Audio Note.
_Avoid_: Artifact title, summary heading

**Manual Title**:
A Note Title authored or edited by a person. It takes precedence over a Generated Title.
_Avoid_: Custom title, original title

**Generated Title**:
A Note Title derived from the transcript when no Manual Title exists. It remains editable.
_Avoid_: Suggested title, AI title

**Recent Title**:
A distinct Manual Title from the Audio Note stream offered for quick reuse. Choosing one copies its text; it does not establish a recurring series.
_Avoid_: Recurring meeting, series

**Inferred Classification**:
The versioned AI-produced classification document, including kind, confidence, topics, participants, and metadata. It drives automatic processor selection and remains unchanged by later organization.
_Avoid_: Effective Classification, folder

**Manual Classification**:
A lowercase kind slug explicitly assigned by a person. It overrides the displayed and searchable kind without replacing the Inferred Classification.
_Avoid_: AI classification, collection

**Effective Classification**:
The Manual Classification when present; otherwise the Inferred Classification kind. Lists and filters use this value.
_Avoid_: Processor kind, artifact kind

**Artifact Kind**:
The output shape owned by a server template, such as Meeting Minutes, Talking Points, or Mind Map. Clients select templates and never supply this value independently.
_Avoid_: Classification, note type
