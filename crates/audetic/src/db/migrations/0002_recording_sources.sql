-- One configured recording source per provider instance. The INSERT below
-- moves the previous singleton row onto this table. `source_instance` is the
-- same identity `external_imports` uses.
CREATE TABLE recording_sources (
    provider TEXT NOT NULL CHECK(trim(provider) <> ''),
    source_instance TEXT NOT NULL CHECK(trim(source_instance) <> ''),
    enabled INTEGER NOT NULL DEFAULT 0 CHECK(enabled IN (0, 1)),
    interval_minutes INTEGER NOT NULL CHECK(interval_minutes BETWEEN 5 AND 1440),
    import_after TIMESTAMP,
    running INTEGER NOT NULL DEFAULT 0 CHECK(running IN (0, 1)),
    last_started_at TIMESTAMP,
    last_completed_at TIMESTAMP,
    last_error TEXT,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (provider, source_instance)
);
INSERT INTO recording_sources (
    provider, source_instance, enabled, interval_minutes, import_after,
    running, last_started_at, last_completed_at, last_error, updated_at
)
SELECT
    'plaud',
    'default',
    enabled,
    interval_minutes,
    import_after,
    running,
    last_started_at,
    last_completed_at,
    last_error,
    updated_at
FROM plaud_sync_state
WHERE singleton = 1;
DROP TABLE plaud_sync_state;
