CREATE TABLE audio_notes (
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
        CHECK(enrichment_status IN ('pending', 'running', 'completed', 'error')),
    enrichment_error TEXT
);
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
CREATE TABLE ingress_access_keys (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL CHECK(trim(name) <> ''),
    scope TEXT NOT NULL CHECK(scope IN ('index', 'generic')),
    secret_hash BLOB NOT NULL UNIQUE
        CHECK(typeof(secret_hash) = 'blob' AND length(secret_hash) = 32),
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    last_used_at TIMESTAMP,
    revoked_at TIMESTAMP
);
CREATE INDEX idx_ingress_access_keys_active
    ON ingress_access_keys(scope, revoked_at);
CREATE TABLE external_imports (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    provider TEXT NOT NULL CHECK(trim(provider) <> ''),
    source_instance TEXT NOT NULL CHECK(trim(source_instance) <> ''),
    external_id TEXT NOT NULL CHECK(trim(external_id) <> ''),
    status TEXT NOT NULL CHECK(status IN ('pending', 'accepted', 'failed')),
    audio_note_id INTEGER REFERENCES audio_notes(id),
    recorded_at TIMESTAMP,
    source_filename TEXT,
    error TEXT,
    attempt_count INTEGER NOT NULL DEFAULT 1 CHECK(attempt_count > 0),
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(provider, source_instance, external_id)
);
CREATE INDEX idx_external_imports_recent
    ON external_imports(created_at DESC, id DESC);
CREATE INDEX idx_external_imports_note
    ON external_imports(audio_note_id);
CREATE TABLE plaud_sync_state (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    enabled INTEGER NOT NULL DEFAULT 0 CHECK(enabled IN (0, 1)),
    interval_minutes INTEGER NOT NULL DEFAULT 15 CHECK(interval_minutes BETWEEN 5 AND 1440),
    import_after TIMESTAMP,
    running INTEGER NOT NULL DEFAULT 0 CHECK(running IN (0, 1)),
    last_started_at TIMESTAMP,
    last_completed_at TIMESTAMP,
    last_error TEXT,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);
INSERT INTO plaud_sync_state(singleton) VALUES(1);
CREATE TABLE post_processing_jobs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL, event TEXT NOT NULL, action_type TEXT NOT NULL,
    action_config TEXT NOT NULL, enabled INTEGER NOT NULL DEFAULT 1,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX idx_pp_jobs_event_enabled ON post_processing_jobs(event) WHERE enabled = 1;
CREATE TABLE agent_profiles (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL, kind TEXT NOT NULL, executable TEXT NOT NULL, args_json TEXT NOT NULL,
    prompt_mode TEXT NOT NULL DEFAULT 'stdin', default_profile INTEGER NOT NULL DEFAULT 0,
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(kind, executable)
);
CREATE INDEX idx_agent_profiles_enabled ON agent_profiles(enabled);
CREATE TABLE audio_note_artifacts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    note_id INTEGER NOT NULL REFERENCES audio_notes(id),
    kind TEXT NOT NULL, title TEXT NOT NULL, template_id TEXT,
    agent_profile_id INTEGER REFERENCES agent_profiles(id),
    status TEXT NOT NULL, content_markdown TEXT, content_json TEXT,
    error TEXT, stdout TEXT, stderr TEXT,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    completed_at TIMESTAMP
);
CREATE INDEX idx_audio_note_artifacts_note_created ON audio_note_artifacts(note_id, created_at DESC);
CREATE INDEX idx_audio_note_artifacts_status ON audio_note_artifacts(status);
