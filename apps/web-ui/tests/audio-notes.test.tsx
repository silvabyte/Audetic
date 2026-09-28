import { test } from "node:test";
import assert from "node:assert/strict";
import { renderToStaticMarkup } from "react-dom/server";
import { MemoryRouter } from "react-router-dom";
import createClient from "openapi-fetch";
import type { paths } from "../src/api/schema";
import { AudioNotesStore, type AudioNoteDetail, type AudioNoteSummary } from "../src/stores/audio-notes-store";
import { AudioNoteRow } from "../src/routes/audio-notes";
import { NoteEnrichment } from "../src/components/note-enrichment";
import { TranscriptPlayer } from "../src/components/transcript-player";
import { ArtifactCard } from "../src/components/note-artifacts-panel";
import type { AudioNoteArtifact } from "../src/stores/note-artifacts-store";
import { RootStore, RootStoreProvider } from "../src/stores/root-store";
import { classificationKind, noteNeedsRefresh } from "../src/lib/audio-notes";

const note: AudioNoteSummary = {
  id: 7, title: "Ideas for the garden", title_source: "manual", source_filename: null,
  status: "completed", duration_seconds: 65, started_at: "2026-09-25T10:00:00Z",
  audio_path: "/isolated/7.wav", transcript_path: "/isolated/7.txt", transcript_text: "Buy basil and tomatoes.",
  capture_source: "microphone", classification: { kind: "shopping-list", confidence: 0.91 },
  enrichment_status: "error", enrichment_error: "Local agent is not configured",
};
const detail: AudioNoteDetail = { ...note, completed_at: "2026-09-25T10:02:00Z", created_at: note.started_at, error: null, transcript_segments: null };
function testStore(handler: (request: Request) => Response | Promise<Response>): AudioNotesStore {
  return new AudioNotesStore(createClient<paths>({ baseUrl: "http://audetic.test/api", fetch: async (request) => handler(request) }));
}

test("stream sends search/classification/pagination to the daemon and keeps chronological results", async () => {
  const requests: URL[] = [];
  const store = testStore((request) => { requests.push(new URL(request.url)); return Response.json({ notes: [note, { ...note, id: 6 }] }); });
  await store.setFilters(" garden ", "shopping-list");
  await store.setPage(30);
  assert.deepEqual(store.list.map((entry) => entry.id), [7, 6]);
  assert.equal(requests[0]?.pathname, "/api/audio-notes");
  assert.equal(requests[0]?.searchParams.get("query"), "garden");
  assert.equal(requests[0]?.searchParams.get("kind"), "shopping-list");
  assert.equal(requests[1]?.searchParams.get("offset"), "30");
  assert.equal(requests[0]?.searchParams.get("limit"), "31");
});

test("an old slow search cannot overwrite a newer filter result", async () => {
  let release: ((response: Response) => void) | undefined;
  const store = testStore((request) => new URL(request.url).searchParams.get("query") === "old" ? new Promise<Response>((resolve) => { release = resolve; }) : Response.json({ notes: [{ ...note, id: 8 }] }));
  const old = store.setFilters("old", "");
  await store.setFilters("new", "");
  assert.ok(release);
  release(Response.json({ notes: [note] }));
  await old;
  assert.equal(store.list[0]?.id, 8);
});

test("capture uses microphone and explicitly disables automatic delivery by default", async () => {
  let body: unknown;
  const store = testStore(async (request) => {
    const path = new URL(request.url, "http://localhost").pathname;
    if (path.endsWith("/start")) { body = await request.json(); return Response.json({ success: true, note_id: 7, audio_path: "/isolated/7.wav", capture_state: "mic_only", message: "Started" }); }
    if (path.endsWith("/status")) return Response.json({ active: true, capture_degraded: false, note_id: 7, phase: "recording", duration_seconds: 0, title: null, audio_path: "/isolated/7.wav", last_error: null });
    return Response.json({ notes: [note] });
  });
  assert.equal(await store.startCapture(), true);
  assert.deepEqual(body, { title: null, capture_source: "microphone", review_before_processing: false, auto_paste: false, copy_to_clipboard: false });
  assert.equal(store.noteId, 7);
  assert.equal(store.active, true);
});

test("raw transcript remains in the stream when enrichment fails", () => {
  const html = renderToStaticMarkup(<MemoryRouter><AudioNoteRow note={note} /></MemoryRouter>);
  assert.match(html, /Buy basil and tomatoes/);
  assert.match(html, /shopping list/);
  assert.match(html, /AI processing needs attention/);
  assert.match(html, /href="\/audio-notes\/7"/);
});

test("detail explains retryable AI error without treating the transcript as failed", () => {
  const root = new RootStore();
  const html = renderToStaticMarkup(<RootStoreProvider value={root}><NoteEnrichment note={detail} /></RootStoreProvider>);
  assert.match(html, /Retry AI processing/);
  assert.match(html, /AI processing failed, not transcription/);
  assert.match(html, /Local agent is not configured/);
  assert.match(html, /91% confidence/);
});

test("notes without timestamps still expose playback and the raw transcript", () => {
  const html = renderToStaticMarkup(<TranscriptPlayer noteId={7} text="Original words" segments={null} />);
  assert.match(html, /<audio/);
  assert.match(html, /\/api\/audio-notes\/7\/audio/);
  assert.match(html, /Original words/);
});

test("unknown classifications stay usable and async enrichment remains refreshable", () => {
  assert.equal(classificationKind({ kind: "new-kind", metadata: { extension: true } }), "new-kind");
  assert.equal(classificationKind(["invalid"]), null);
  assert.equal(classificationKind({ kind: 4 }), null);
  assert.equal(noteNeedsRefresh({ status: "completed", enrichment_status: "running" }), true);
  assert.equal(noteNeedsRefresh({ status: "completed", enrichment_status: "error" }), false);
  assert.equal(noteNeedsRefresh({ status: "cancelled", enrichment_status: "pending" }), false);
  assert.equal(noteNeedsRefresh({ status: "error", enrichment_status: "pending" }), false);
});

test("enrichment retry uses the unified endpoint and reloads the persisted note", async () => {
  const requests: string[] = [];
  const store = testStore((request) => {
    const path = new URL(request.url, "http://localhost").pathname;
    requests.push(`${request.method} ${path}`);
    return Response.json(path.endsWith("/process") ? { success: true, note_id: 7 } : path.endsWith("/7") ? { ...detail, enrichment_status: "running", enrichment_error: null } : { notes: [note] });
  });
  assert.equal(await store.processNote(7), true);
  assert.ok(requests.includes("POST /api/audio-notes/7/process"));
  assert.equal(store.detailCache[7]?.enrichment_status, "running");
});

test("import preserves the real multipart file and returns a note id", async () => {
  let imported: FormData | null = null;
  const store = testStore(async (request) => {
    if (request.method === "POST") {
      assert.match(request.headers.get("content-type") ?? "", /^multipart\/form-data; boundary=/);
      imported = await request.formData();
      return Response.json({ success: true, note_id: 9, message: "Imported" }, { status: 202 });
    }
    return Response.json({ notes: [] });
  });
  assert.equal(await store.importFile(new File(["audio-bytes"], "sample.wav", { type: "audio/wav" }), "  Garden  "), 9);
  assert.ok(imported);
  const form: FormData = imported;
  assert.equal(form.get("title"), "Garden");
  const file = form.get("file");
  assert.ok(file instanceof File);
  assert.equal(file.name, "sample.wav");
  assert.equal(await file.text(), "audio-bytes");
});

test("confirm passes precise trim boundaries and surfaces repeated failures", async () => {
  const bodies: unknown[] = [];
  const store = testStore(async (request) => {
    bodies.push(await request.json());
    return Response.json({ message: "No recording awaiting review" }, { status: 409 });
  });
  assert.equal(await store.confirmCapture(1.125, 12.75), false);
  assert.equal(store.lastError, "No recording awaiting review");
  assert.equal(await store.confirmCapture(1.125, 12.75), false);
  assert.deepEqual(bodies, [{ start_seconds: 1.125, end_seconds: 12.75 }, { start_seconds: 1.125, end_seconds: 12.75 }]);
});

test("a note deleted during a slow detail fetch cannot reappear in the cache", async () => {
  let release: ((response: Response) => void) | undefined;
  const store = testStore((request) => {
    if (request.method === "DELETE") return Response.json({ success: true, note_id: 7 });
    if (new URL(request.url).pathname.endsWith("/7")) return new Promise<Response>((resolve) => { release = resolve; });
    return Response.json({ notes: [] });
  });
  const loading = store.loadDetail(7);
  assert.equal(await store.deleteNote(7), true);
  assert.ok(release);
  release(Response.json(detail));
  await loading;
  assert.equal(store.detailCache[7], undefined);
});

test("structured request artifacts expose data without claiming external execution", () => {
  const artifact: AudioNoteArtifact = {
    id: 1, note_id: 7, kind: "shopping_items", title: "Shopping list", template_id: "request_intent", agent_profile_id: 1,
    status: "completed", content_markdown: null, content_json: { shopping_items: [{ name: "basil", quantity: 2 }] },
    error: null, stdout: null, stderr: null, created_at: note.started_at, updated_at: note.started_at, completed_at: note.started_at,
  };
  const html = renderToStaticMarkup(<ArtifactCard artifact={artifact} onDelete={async () => {}} />);
  assert.match(html, /Structured data/);
  assert.match(html, /basil/);
  assert.match(html, /No external action has been performed/);
  assert.match(html, /Copy JSON/);
});

test("a title mutation refresh wins over an older in-flight detail read", async () => {
  let release: ((response: Response) => void) | undefined;
  let first = true;
  const store = testStore((request) => {
    const path = new URL(request.url).pathname;
    if (request.method === "PATCH") return Response.json({ note_id: 7, title: "Updated title", title_source: "manual" });
    if (path.endsWith("/7") && first) { first = false; return new Promise<Response>((resolve) => { release = resolve; }); }
    if (path.endsWith("/7")) return Response.json({ ...detail, title: "Updated title" });
    return Response.json(path.endsWith("/recent-titles") ? { titles: ["Updated title"] } : { notes: [] });
  });
  const loading = store.loadDetail(7);
  assert.equal(await store.updateTitle(7, "Updated title"), true);
  assert.ok(release);
  release(Response.json(detail));
  await loading;
  assert.equal(store.detailCache[7]?.title, "Updated title");
});
