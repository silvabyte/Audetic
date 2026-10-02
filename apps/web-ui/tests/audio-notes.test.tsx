import { test } from "node:test";
import assert from "node:assert/strict";
import { renderToStaticMarkup } from "react-dom/server";
import { MemoryRouter } from "react-router-dom";
import createClient from "openapi-fetch";
import type { paths } from "../src/api/schema";
import { audioNoteAudioUrl } from "../src/api/client";
import { ArtifactContent } from "../src/components/artifact-content";
import { AudioNotesStore, type AudioNoteDetail, type AudioNoteSummary } from "../src/stores/audio-notes-store";
import { AudioNoteRow } from "../src/routes/audio-notes";
import { NoteEnrichment } from "../src/components/note-enrichment";
import { TranscriptPlayer } from "../src/components/transcript-player";
import { AudioTransport } from "../src/components/audio-transport";
import { ArtifactCard, artifactKindLabel } from "../src/components/note-artifacts-panel";
import type { AudioNoteArtifact } from "../src/stores/note-artifacts-store";
import { RootStore, RootStoreProvider } from "../src/stores/root-store";
import { classificationKind, effectiveClassificationKind, isClassificationSlug, noteNeedsRefresh } from "../src/lib/audio-notes";
import { parseTalkingPoints } from "../src/lib/talking-points";

const note: AudioNoteSummary = {
  id: 7, title: "Ideas for the garden", title_source: "manual", source_filename: null,
  status: "completed", duration_seconds: 65, started_at: "2026-09-25T10:00:00Z",
  audio_path: "/isolated/7.wav", transcript_path: "/isolated/7.txt", transcript_text: "Buy basil and tomatoes.",
  capture_source: "microphone", classification: { kind: "shopping-list", confidence: 0.91 }, classification_kind: "shopping-list", classification_kind_override: null,
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

test("audio transport exposes talking point markers without crowding the scrubber", () => {
  const ref = { current: null };
  const html = renderToStaticMarkup(<AudioTransport noteId={7} audioRef={ref} currentTime={0} durationHint={120} markers={[{ label: "Decision", seconds: 30, timestamp: "00:30" }]} onTimeChange={() => {}} />);
  assert.match(html, /Playback position/);
  assert.match(html, /left:25%/);
  assert.match(html, /basis-full/);
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

test("effective classification prefers a manual override and validates custom slugs", () => {
  assert.equal(effectiveClassificationKind({ classification: { kind: "meeting" }, classification_kind: "meeting", classification_kind_override: "creative-art" }), "creative-art");
  assert.equal(effectiveClassificationKind({ classification: { kind: "meeting" }, classification_kind: "conversation", classification_kind_override: null }), "conversation");
  assert.equal(effectiveClassificationKind({ classification: { kind: "dictation" } }), "dictation");
  assert.equal(isClassificationSlug("creative-art"), true);
  assert.equal(isClassificationSlug("Creative art"), false);
  assert.equal(isClassificationSlug(`a${"b".repeat(64)}`), false);
});

test("manual classification mutations use typed endpoints and stale detail/list reads cannot win", async () => {
  let releaseDetail: ((response: Response) => void) | undefined;
  let releaseList: ((response: Response) => void) | undefined;
  let firstDetail = true;
  let firstList = true;
  let putBody: unknown;
  const requests: string[] = [];
  const store = testStore(async (request) => {
    const url = new URL(request.url);
    requests.push(`${request.method} ${url.pathname}`);
    if (request.method === "PUT") {
      putBody = await request.json();
      return Response.json({ note_id: 7, classification_kind: "creative-art", classification_kind_override: "creative-art" });
    }
    if (request.method === "DELETE") return Response.json({ note_id: 7, classification_kind: "shopping-list", classification_kind_override: null });
    if (url.pathname.endsWith("/classifications")) return Response.json({ kinds: ["creative-art", "shopping-list"] });
    if (url.pathname.endsWith("/7") && firstDetail) {
      firstDetail = false;
      return new Promise<Response>((resolve) => { releaseDetail = resolve; });
    }
    if (url.pathname.endsWith("/7")) {
      const cleared = requests.some((entry) => entry === "DELETE /api/audio-notes/7/classification");
      return Response.json({ ...detail, classification_kind: cleared ? "shopping-list" : "creative-art", classification_kind_override: cleared ? null : "creative-art" });
    }
    if (url.pathname.endsWith("/audio-notes") && firstList) {
      firstList = false;
      return new Promise<Response>((resolve) => { releaseList = resolve; });
    }
    return Response.json({ notes: [{ ...note, classification_kind: "creative-art", classification_kind_override: "creative-art" }] });
  });

  const staleDetail = store.loadDetail(7);
  const staleList = store.loadList();
  assert.equal(await store.setClassification(7, " creative-art "), true);
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.deepEqual(putBody, { kind: "creative-art" });
  assert.equal(store.detailCache[7]?.classification_kind_override, "creative-art");
  assert.equal(store.list[0]?.classification_kind_override, "creative-art");
  assert.deepEqual(store.classificationKinds, ["creative-art", "shopping-list"]);
  assert.ok(releaseDetail);
  assert.ok(releaseList);
  releaseDetail(Response.json(detail));
  releaseList(Response.json({ notes: [note] }));
  await Promise.all([staleDetail, staleList]);
  assert.equal(store.detailCache[7]?.classification_kind_override, "creative-art");
  assert.equal(store.list[0]?.classification_kind_override, "creative-art");

  assert.equal(await store.clearClassification(7), true);
  assert.equal(store.detailCache[7]?.classification_kind_override, null);
  assert.ok(requests.includes("PUT /api/audio-notes/7/classification"));
  assert.ok(requests.includes("DELETE /api/audio-notes/7/classification"));
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

test("artifact markdown renders GFM safely without interpreting raw HTML", () => {
  const html = renderToStaticMarkup(<ArtifactContent markdown={'# Report\n\n| Item | Owner |\n| --- | --- |\n| Ship | Ana |\n\n<script>alert("unsafe")</script>\n\n[Reference](https://example.com)'} />);
  assert.match(html, /<table/);
  assert.match(html, /Ship/);
  assert.match(html, /target="_blank"/);
  assert.match(html, /rel="noreferrer noopener"/);
  assert.doesNotMatch(html, /<script/);
  assert.doesNotMatch(html, /alert\(&quot;unsafe&quot;\)/);
});

test("mermaid fences use a standalone render surface without invalid pre nesting", () => {
  const html = renderToStaticMarkup(<ArtifactContent markdown={'```mermaid\ngraph TD\n  A --> B\n```'} />);
  assert.match(html, /Rendering diagram/);
  assert.doesNotMatch(html, /<pre[^>]*><div/);
});

test("talking points use the newest completed artifact and produce sorted bounded chapters", () => {
  const artifacts: AudioNoteArtifact[] = [
    { id: 1, note_id: 7, kind: "talking_points", title: "Old", template_id: "talking_points", agent_profile_id: 1, status: "completed", content_markdown: "- [00:01] Old - ignored", content_json: null, error: null, stdout: null, stderr: null, created_at: "2026-09-25T09:00:00Z", updated_at: "2026-09-25T09:00:00Z", completed_at: "2026-09-25T09:00:00Z" },
    { id: 2, note_id: 7, kind: "talking_points", title: "New", template_id: "talking_points", agent_profile_id: 1, status: "completed", content_markdown: "- [01:20] Garden — Planting plan\n- [00:10] Opening - Context\n- [00:10] Duplicate – ignored\n- [00:70] Invalid - ignored\n- [1:02:03] Long — outside recording", content_json: null, error: null, stdout: null, stderr: null, created_at: "2026-09-25T10:00:00Z", updated_at: "2026-09-25T10:00:00Z", completed_at: "2026-09-25T10:00:00Z" },
  ];
  assert.deepEqual(parseTalkingPoints(artifacts, 300), [
    { label: "Opening - Context", seconds: 10, timestamp: "00:10" },
    { label: "Garden — Planting plan", seconds: 80, timestamp: "01:20" },
  ]);
});

test("audio URLs and unknown artifact kinds have stable fallbacks", () => {
  assert.equal(audioNoteAudioUrl(42), "/api/audio-notes/42/audio");
  assert.equal(artifactKindLabel("future_format"), "future format");
  const artifact: AudioNoteArtifact = { id: 3, note_id: 7, kind: "future_format", title: "Future", template_id: null, agent_profile_id: null, status: "completed", content_markdown: "Hello", content_json: null, error: null, stdout: null, stderr: null, created_at: note.started_at, updated_at: note.started_at, completed_at: note.started_at };
  assert.match(renderToStaticMarkup(<ArtifactCard artifact={artifact} onDelete={async () => {}} />), /future format/);
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
