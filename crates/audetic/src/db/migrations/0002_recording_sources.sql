-- One configured recording source per provider instance. `sync_mode` is either
-- `poll` or `webhook`. The INSERT below only moves the previous singleton row
-- onto this table. `source_instance` is the same identity `external_imports` uses.
CREATE TABLE recording_sources (
    provider TEXT NOT NULL CHECK(trim(provider) <> ''),
    source_instance TEXT NOT NULL CHECK(trim(source_instance) <> ''),
    sync_mode TEXT NOT NULL CHECK(sync_mode IN ('poll', 'webhook')),
    enabled INTEGER NOT NULL DEFAULT 0 CHECK(enabled IN (0, 1)),
    interval_minutes INTEGER CHECK(interval_minutes IS NULL OR interval_minutes > 0),
    import_after TIMESTAMP,
    running INTEGER NOT NULL DEFAULT 0 CHECK(running IN (0, 1)),
    last_started_at TIMESTAMP,
    last_completed_at TIMESTAMP,
    last_error TEXT,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (provider, source_instance)
);
INSERT INTO recording_sources (
    provider, source_instance, sync_mode, enabled, interval_minutes, import_after,
    running, last_started_at, last_completed_at, last_error, updated_at
)
SELECT
    'plaud',
    'default',
    'poll',
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
