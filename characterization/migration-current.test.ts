import { existsSync } from "node:fs";
import { afterAll, beforeAll, expect, test } from "bun:test";

import { ENRICHMENT_ERROR_TEXT, INTERRUPTED_RECORDING, NOTE_IDS } from "./src/contract.ts";
import { type DaemonSession, startDaemon } from "./src/daemon.ts";
import { migrationChecksums, readMigrations, readNotes } from "./src/fixture.ts";
import { getJson } from "./src/http.ts";
import { AudioNoteDetail, AudioNotesList, VersionInfo, decode } from "./src/schema.ts";

let session: DaemonSession;

beforeAll(async () => {
  session = await startDaemon("migration3");
}, 60_000);

afterAll(async () => {
  await session?.stop();
});

test("an already-current database is not rewritten or backed up again", () => {
  expect(existsSync(session.backupPath)).toBe(false);
  const expected = migrationChecksums();
  const live = readMigrations(session.dbPath);
  expect(live.map((row) => [row.version, row.checksum])).toEqual(
    expected.map((row) => [row.version, row.checksum]),
  );
});

test("skipped enrichment survives startup and the recording row is still swept", async () => {
  const list = decode(
    AudioNotesList,
    (await getJson(session.baseUrl, "/api/audio-notes?limit=50")).body,
    "current list",
  );
  expect(list.notes.map((note) => note.id)).toEqual([
    NOTE_IDS.skipped,
    NOTE_IDS.completed,
    NOTE_IDS.enrichmentRunning,
    NOTE_IDS.enrichmentError,
    NOTE_IDS.transcribing,
    NOTE_IDS.recording,
  ]);

  const skipped = decode(
    AudioNoteDetail,
    (await getJson(session.baseUrl, `/api/audio-notes/${NOTE_IDS.skipped}`)).body,
    "skipped detail",
  );
  expect(skipped.status).toBe("completed");
  expect(skipped.enrichment_status).toBe("skipped");
  expect(skipped.enrichment_error).toBeNull();
  expect(skipped.transcript_text).toBe("skipped transcript");

  const failed = list.notes.find((note) => note.id === NOTE_IDS.enrichmentError);
  expect(failed?.enrichment_error).toBe(ENRICHMENT_ERROR_TEXT);

  const recording = decode(
    AudioNoteDetail,
    (await getJson(session.baseUrl, `/api/audio-notes/${NOTE_IDS.recording}`)).body,
    "current recording detail",
  );
  expect(recording.status).toBe("error");
  expect(recording.error).toBe(INTERRUPTED_RECORDING);

  const stored = readNotes(session.dbPath);
  expect(stored.find((note) => note.id === NOTE_IDS.skipped)?.enrichment_status).toBe("skipped");
  expect(stored.find((note) => note.id === NOTE_IDS.deleted)?.status).toBe("recording");
});

test("version is still served from the current database", async () => {
  const version = decode(
    VersionInfo,
    (await getJson(session.baseUrl, "/api/version")).body,
    "current version",
  );
  expect(version.name).toBe("audetic");
  expect(version.version.length).toBeGreaterThan(0);
});
