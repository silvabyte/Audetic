import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { Database } from "bun:sqlite";

import {
  ENRICHMENT_ERROR_TEXT,
  ERROR_TRANSCRIPT,
  MANUAL_TITLE,
  MEETING_KIND,
  NOTE_IDS,
} from "./contract.ts";
import { migrationDir } from "./paths.ts";

const SCHEMA_MIGRATIONS_DDL = `CREATE TABLE schema_migrations (
    version INTEGER PRIMARY KEY CHECK(version > 0),
    checksum TEXT NOT NULL CHECK(length(checksum) = 64),
    applied_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);`;

export const MIGRATION_FILES = [
  "0001_baseline.sql",
  "0002_recording_sources.sql",
  "0003_enrichment_status_skipped.sql",
] as const;

export type FixtureKind = "migration2" | "migration3";

export interface MigrationChecksum {
  readonly version: number;
  readonly name: string;
  readonly checksum: string;
}

export function migrationChecksums(): MigrationChecksum[] {
  return MIGRATION_FILES.map((name, index) => {
    const bytes = readFileSync(join(migrationDir(), name));
    return {
      version: index + 1,
      name,
      checksum: createHash("sha256").update(bytes).digest("hex"),
    };
  });
}

export interface PreparedFixture {
  readonly kind: FixtureKind;
  readonly dbPath: string;
  readonly audioPath: string;
  readonly checksums: readonly MigrationChecksum[];
}

/**
 * Write a database the Rust daemon can upgrade. Migration 2 is the MacBook
 * shape (no `skipped` enrichment). Migration 3 is this checkout, including a
 * skipped row the older CHECK constraint would reject.
 */
export function writeFixture(dataDir: string, kind: FixtureKind): PreparedFixture {
  mkdirSync(dataDir, { recursive: true });
  const dbPath = join(dataDir, "audetic.db");
  const audioPath = join(dataDir, "enrichment-error.wav");
  writeFileSync(audioPath, Buffer.from("audetic-characterization-audio"));

  const checksums = migrationChecksums();
  const applied = kind === "migration2" ? 2 : 3;
  const db = new Database(dbPath);
  try {
    db.exec("PRAGMA foreign_keys = ON");
    db.exec(SCHEMA_MIGRATIONS_DDL);
    for (const migration of checksums.slice(0, applied)) {
      const sql = readFileSync(join(migrationDir(), migration.name), "utf8");
      db.exec(sql);
      db.run(
        "INSERT INTO schema_migrations(version, checksum) VALUES (?, ?)",
        [migration.version, migration.checksum],
      );
    }
    seed(db, kind, audioPath);
  } finally {
    db.close();
  }

  return { kind, dbPath, audioPath, checksums };
}

function seed(db: Database, kind: FixtureKind, audioPath: string): void {
  const insert = db.prepare(`INSERT INTO audio_notes (
      id, title, title_source, status, audio_path, source_filename,
      transcript_path, transcript_text, transcript_segments, duration_seconds,
      started_at, completed_at, error, created_at, deleted_at, capture_source,
      classification, classification_kind_override, enrichment_status, enrichment_error
    ) VALUES (
      $id, $title, $title_source, $status, $audio_path, NULL,
      $transcript_path, $transcript_text, $transcript_segments, $duration_seconds,
      $started_at, $completed_at, $error, $created_at, $deleted_at, 'microphone',
      $classification, NULL, $enrichment_status, $enrichment_error
    )`);

  const meeting = JSON.stringify({ kind: MEETING_KIND });
  const segments = JSON.stringify([
    { start: 0, end: 1.25, text: ERROR_TRANSCRIPT },
  ]);

  insert.run({
    $id: NOTE_IDS.recording,
    $title: "Still recording",
    $title_source: null,
    $status: "recording",
    $audio_path: "missing-recording.wav",
    $transcript_path: null,
    $transcript_text: null,
    $transcript_segments: null,
    $duration_seconds: null,
    $started_at: "2026-11-02 10:00:00",
    $completed_at: null,
    $error: null,
    $created_at: "2026-11-02 10:00:00",
    $deleted_at: null,
    $classification: null,
    $enrichment_status: "pending",
    $enrichment_error: null,
  });

  insert.run({
    $id: NOTE_IDS.transcribing,
    $title: "Still transcribing",
    $title_source: null,
    $status: "transcribing",
    $audio_path: "missing-transcribing.wav",
    $transcript_path: null,
    $transcript_text: null,
    $transcript_segments: null,
    $duration_seconds: 3,
    $started_at: "2026-11-03 10:00:00",
    $completed_at: null,
    $error: null,
    $created_at: "2026-11-03 10:00:00",
    $deleted_at: null,
    $classification: null,
    $enrichment_status: "pending",
    $enrichment_error: null,
  });

  insert.run({
    $id: NOTE_IDS.enrichmentError,
    $title: MANUAL_TITLE,
    $title_source: "manual",
    $status: "completed",
    $audio_path: audioPath,
    $transcript_path: "enrichment-error.txt",
    $transcript_text: ERROR_TRANSCRIPT,
    $transcript_segments: segments,
    $duration_seconds: 12,
    $started_at: "2026-11-04 10:00:00",
    $completed_at: "2026-11-04 10:00:12",
    $error: null,
    $created_at: "2026-11-04 10:00:00",
    $deleted_at: null,
    $classification: meeting,
    $enrichment_status: "error",
    $enrichment_error: ENRICHMENT_ERROR_TEXT,
  });

  insert.run({
    $id: NOTE_IDS.enrichmentRunning,
    $title: "Enrichment was running",
    $title_source: "generated",
    $status: "completed",
    $audio_path: "missing-running.wav",
    $transcript_path: "running.txt",
    $transcript_text: "running enrichment transcript",
    $transcript_segments: null,
    $duration_seconds: 8,
    $started_at: "2026-11-05 10:00:00",
    $completed_at: "2026-11-05 10:00:08",
    $error: null,
    $created_at: "2026-11-05 10:00:00",
    $deleted_at: null,
    $classification: null,
    $enrichment_status: "running",
    $enrichment_error: null,
  });

  insert.run({
    $id: NOTE_IDS.completed,
    $title: "Ready note",
    $title_source: null,
    $status: "completed",
    $audio_path: "missing-ready.wav",
    $transcript_path: "ready.txt",
    $transcript_text: "ready transcript",
    $transcript_segments: null,
    $duration_seconds: 4,
    $started_at: "2026-11-06 10:00:00",
    $completed_at: "2026-11-06 10:00:04",
    $error: null,
    $created_at: "2026-11-06 10:00:00",
    $deleted_at: null,
    $classification: null,
    $enrichment_status: "pending",
    $enrichment_error: null,
  });

  insert.run({
    $id: NOTE_IDS.deleted,
    $title: "Deleted in flight",
    $title_source: null,
    $status: "recording",
    $audio_path: "missing-deleted.wav",
    $transcript_path: null,
    $transcript_text: null,
    $transcript_segments: null,
    $duration_seconds: null,
    $started_at: "2026-11-01 10:00:00",
    $completed_at: null,
    $error: null,
    $created_at: "2026-11-01 10:00:00",
    $deleted_at: "2026-11-01 10:05:00",
    $classification: null,
    $enrichment_status: "pending",
    $enrichment_error: null,
  });

  if (kind === "migration3") {
    insert.run({
      $id: NOTE_IDS.skipped,
      $title: "Processing skipped",
      $title_source: null,
      $status: "completed",
      $audio_path: "missing-skipped.wav",
      $transcript_path: "skipped.txt",
      $transcript_text: "skipped transcript",
      $transcript_segments: null,
      $duration_seconds: 2,
      $started_at: "2026-11-07 10:00:00",
      $completed_at: "2026-11-07 10:00:02",
      $error: null,
      $created_at: "2026-11-07 10:00:00",
      $deleted_at: null,
      $classification: null,
      $enrichment_status: "skipped",
      $enrichment_error: null,
    });
  }

  db.run(
    `INSERT INTO audio_note_artifacts(note_id, kind, title, status)
     VALUES (?, 'summary', 'Kept summary', 'pending')`,
    [NOTE_IDS.completed],
  );
  db.run(
    `INSERT INTO external_imports(
       provider, source_instance, external_id, status, audio_note_id
     ) VALUES ('plaud', 'default', 'ext-pending', 'pending', ?)`,
    [NOTE_IDS.completed],
  );
  db.run(
    `UPDATE recording_sources SET running = 1
     WHERE provider = 'plaud' AND source_instance = 'default'`,
  );
}

export interface MigrationRow {
  readonly version: number;
  readonly checksum: string;
}

export interface NoteRow {
  readonly id: number;
  readonly status: string;
  readonly error: string | null;
  readonly enrichment_status: string;
  readonly enrichment_error: string | null;
  readonly deleted_at: string | null;
}

export function readMigrations(dbPath: string): MigrationRow[] {
  return readOnly(dbPath, (db) =>
    db
      .query("SELECT version, checksum FROM schema_migrations ORDER BY version")
      .all() as MigrationRow[],
  );
}

export function readNotes(dbPath: string): NoteRow[] {
  return readOnly(dbPath, (db) =>
    db
      .query(
        `SELECT id, status, error, enrichment_status, enrichment_error, deleted_at
         FROM audio_notes ORDER BY id`,
      )
      .all() as NoteRow[],
  );
}

export function readRecordingSource(dbPath: string): {
  running: number;
  last_error: string | null;
} {
  return readOnly(dbPath, (db) => {
    const row = db
      .query(
        `SELECT running, last_error FROM recording_sources
         WHERE provider = 'plaud' AND source_instance = 'default'`,
      )
      .get() as { running: number; last_error: string | null } | null;
    if (!row) {
      throw new Error("recording source plaud/default is missing");
    }
    return row;
  });
}

export function readImport(dbPath: string): { status: string; error: string | null } {
  return readOnly(dbPath, (db) => {
    const row = db
      .query(
        `SELECT status, error FROM external_imports
         WHERE provider = 'plaud' AND external_id = 'ext-pending'`,
      )
      .get() as { status: string; error: string | null } | null;
    if (!row) {
      throw new Error("pending import fixture row is missing");
    }
    return row;
  });
}

function readOnly<T>(dbPath: string, read: (db: Database) => T): T {
  const db = new Database(dbPath, { readonly: true });
  try {
    return read(db);
  } finally {
    db.close();
  }
}

export function preUpgradeBackupPath(dbPath: string): string {
  return `${dbPath}.pre-v3.sqlite3`;
}
