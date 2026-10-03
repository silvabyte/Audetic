import { test } from "node:test";
import assert from "node:assert/strict";
import { groupNotesByDate, transcriptExcerpt } from "../src/lib/audio-note-list";

test("date groups follow local calendar days across a year boundary and keep note order", () => {
  const now = new Date(2026, 0, 1, 10);
  const notes = [
    { id: 4, started_at: new Date(2026, 0, 1, 9).toISOString() },
    { id: 3, started_at: new Date(2026, 0, 1, 8).toISOString() },
    { id: 2, started_at: new Date(2025, 11, 31, 23).toISOString() },
    { id: 1, started_at: new Date(2025, 11, 20, 12).toISOString() },
  ];
  const groups = groupNotesByDate(notes, now);
  assert.deepEqual(groups.map(group => group.notes.map(note => note.id)), [[4, 3], [2], [1]]);
  assert.equal(groups[0]?.label, "Today");
  assert.equal(groups[1]?.label, "Yesterday");
  assert.match(groups[2]?.label ?? "", /2025/);
  assert.deepEqual(groupNotesByDate([], now), []);
});

test("search preview reveals a distant match and preserves nearby context", () => {
  const transcript = `${"Opening remarks. ".repeat(50)}The launch decision is to ship on Friday.`;
  const preview = transcriptExcerpt(transcript, "LAUNCH DECISION");
  assert.ok(preview.startsWith("…"));
  assert.ok(preview.indexOf("launch decision") < 65);
  assert.ok(preview.endsWith("ship on Friday."));
  assert.equal(transcriptExcerpt("  One\n\tthought  ", ""), "One thought");
  assert.equal(transcriptExcerpt("One thought", "missing"), "One thought");
});
