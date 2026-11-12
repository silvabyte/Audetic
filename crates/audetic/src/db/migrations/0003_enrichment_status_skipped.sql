-- Allow enrichment_status 'skipped' for notes whose automatic AI processing
-- was not enqueued. SQLite cannot ALTER a CHECK, and this statement runs
-- inside the runner's transaction with foreign keys already enabled, so
-- PRAGMA foreign_keys cannot be turned off. Dropping audio_notes while
-- child rows still reference it records a deferred violation that survives
-- recreating the table and fails the commit. Rebuild the referencing
-- tables onto the replacement first, then replace the parent. Existing
-- rows are copied unchanged. Renaming the new tables keeps sqlite_sequence,
-- so later inserts continue past the current ids.
CREATE TABLE audio_notes_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    title TEXT, title_source TEXT, title_updated_at TIMESTAMP,
    title_version INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'recording',
    audio_path TEXT NOT NULL,
    source_filename TEXT,
    transcript_path TEXT, transcript_text TEXT, transcript_segments TEXT,
    duration_seconds INTEGER,
    started_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    completed_at TIMESTAMP, error TEXT,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    deleted_at TIMESTAMP,
    capture_source TEXT NOT NULL DEFAULT 'microphone',
    source_provider TEXT,
    source_external_id TEXT,
    source_recorded_at TIMESTAMP,
    classification TEXT CHECK(classification IS NULL OR json_valid(classification)),
    classification_kind_override TEXT CHECK(
        classification_kind_override IS NULL OR (
            length(classification_kind_override) BETWEEN 1 AND 64
            AND substr(classification_kind_override, 1, 1) GLOB '[a-z]'
            AND classification_kind_override NOT GLOB '*[^a-z0-9_-]*'
        )
    ),
    enrichment_status TEXT NOT NULL DEFAULT 'pending'
        CHECK(enrichment_status IN ('pending', 'running', 'completed', 'error', 'skipped')),
    enrichment_error TEXT
);
INSERT INTO audio_notes_new SELECT * FROM audio_notes;

CREATE TABLE audio_note_artifacts_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    note_id INTEGER NOT NULL REFERENCES audio_notes_new(id),
    kind TEXT NOT NULL, title TEXT NOT NULL, template_id TEXT,
    agent_profile_id INTEGER REFERENCES agent_profiles(id),
    status TEXT NOT NULL, content_markdown TEXT, content_json TEXT,
    error TEXT, stdout TEXT, stderr TEXT,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    completed_at TIMESTAMP
);
INSERT INTO audio_note_artifacts_new SELECT * FROM audio_note_artifacts;
DROP TABLE audio_note_artifacts;
ALTER TABLE audio_note_artifacts_new RENAME TO audio_note_artifacts;
CREATE INDEX idx_audio_note_artifacts_note_created
    ON audio_note_artifacts(note_id, created_at DESC);
CREATE INDEX idx_audio_note_artifacts_status ON audio_note_artifacts(status);

CREATE TABLE external_imports_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    provider TEXT NOT NULL CHECK(trim(provider) <> ''),
    source_instance TEXT NOT NULL CHECK(trim(source_instance) <> ''),
    external_id TEXT NOT NULL CHECK(trim(external_id) <> ''),
    status TEXT NOT NULL CHECK(status IN ('pending', 'accepted', 'failed')),
    audio_note_id INTEGER REFERENCES audio_notes_new(id),
    recorded_at TIMESTAMP,
    source_filename TEXT,
    error TEXT,
    attempt_count INTEGER NOT NULL DEFAULT 1 CHECK(attempt_count > 0),
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(provider, source_instance, external_id)
);
INSERT INTO external_imports_new SELECT * FROM external_imports;
DROP TABLE external_imports;
ALTER TABLE external_imports_new RENAME TO external_imports;
CREATE INDEX idx_external_imports_recent
    ON external_imports(created_at DESC, id DESC);
CREATE INDEX idx_external_imports_note
    ON external_imports(audio_note_id);

DROP TABLE audio_notes;
ALTER TABLE audio_notes_new RENAME TO audio_notes;
CREATE INDEX idx_audio_notes_stream ON audio_notes(started_at DESC, id DESC)
    WHERE deleted_at IS NULL;
CREATE INDEX idx_audio_notes_status ON audio_notes(status);
CREATE INDEX idx_audio_notes_effective_kind ON audio_notes(
    COALESCE(
        classification_kind_override,
        json_extract(CASE WHEN json_valid(classification) THEN classification END, '$.kind')
    )
);
CREATE INDEX idx_audio_notes_source ON audio_notes(source_provider, source_external_id);
