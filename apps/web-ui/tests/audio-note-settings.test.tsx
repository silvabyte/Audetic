import { test } from "node:test";
import assert from "node:assert/strict";
import { renderToStaticMarkup } from "react-dom/server";
import { runInAction } from "mobx";
import createClient from "openapi-fetch";
import type { paths } from "../src/api/schema";
import { AudioNoteSettingsStore } from "../src/stores/audio-note-settings-store";
import { AudioNotesStore } from "../src/stores/audio-notes-store";
import { RootStore, RootStoreProvider } from "../src/stores/root-store";
import { SettingsCapture } from "../src/routes/settings/capture";
import { DEFAULT_CAPTURE_OPTIONS } from "../src/lib/capture-options";

function clientWith(handler: (request: Request) => Response | Promise<Response>) {
  return createClient<paths>({ baseUrl: "http://audetic.test/api", fetch: async (request) => handler(request) });
}

test("automatic paste is safely off before and during the initial settings load", async () => {
  let release: ((response: Response) => void) | undefined;
  const store = new AudioNoteSettingsStore(clientWith(() => new Promise<Response>((resolve) => { release = resolve; })));
  assert.equal(store.effectiveDefault, false);
  assert.equal(store.captureAutoPaste(null), false);
  const loading = store.load();
  assert.equal(store.state, "loading");
  assert.equal(store.effectiveDefault, false);
  assert.equal(store.captureAutoPaste(null), false);
  assert.ok(release);
  release(Response.json({ auto_paste: true }));
  await loading;
  assert.equal(store.effectiveDefault, true);
  assert.equal(store.captureAutoPaste(null), null);
});

test("saving uses the real settings API and a fresh store reads the persisted preference", async () => {
  let persisted = false;
  const requests: string[] = [];
  const client = clientWith(async (request) => {
    requests.push(`${request.method} ${new URL(request.url).pathname}`);
    if (request.method === "PUT") {
      const body: unknown = await request.json();
      assert.deepEqual(body, { auto_paste: true });
      persisted = true;
    }
    return Response.json({ auto_paste: persisted });
  });
  const first = new AudioNoteSettingsStore(client);
  await first.load();
  assert.equal(first.effectiveDefault, false);
  assert.equal(await first.saveAutoPaste(true), true);
  const reopened = new AudioNoteSettingsStore(client);
  await reopened.load();
  assert.equal(reopened.effectiveDefault, true);
  assert.deepEqual(requests, ["GET /api/audio-notes/settings", "PUT /api/audio-notes/settings", "GET /api/audio-notes/settings"]);
});

test("capture inherits the saved preference with null while explicit false and true override it", async () => {
  const bodies: unknown[] = [];
  const client = clientWith(async (request) => {
    const path = new URL(request.url).pathname;
    if (path.endsWith("/settings")) return Response.json({ auto_paste: true });
    if (path.endsWith("/start")) {
      bodies.push(await request.json());
      return Response.json({ success: true, note_id: 1, audio_path: "/isolated/1.wav", capture_state: "mic_only", message: "Started" });
    }
    if (path.endsWith("/status")) return Response.json({ active: true, capture_degraded: false, note_id: 1, phase: "recording", duration_seconds: 0, title: null, audio_path: "/isolated/1.wav", last_error: null });
    return Response.json({ notes: [] });
  });
  const settings = new AudioNoteSettingsStore(client);
  await settings.load();
  const notes = new AudioNotesStore(client);
  for (const choice of [null, false, true]) {
    assert.equal(await notes.startCapture({ ...DEFAULT_CAPTURE_OPTIONS, auto_paste: settings.captureAutoPaste(choice) }), true);
  }
  assert.deepEqual(bodies, [null, false, true].map((auto_paste) => ({ ...DEFAULT_CAPTURE_OPTIONS, auto_paste })));
});

test("failed settings loads report the error and cannot silently enable delivery", async () => {
  const settings = new AudioNoteSettingsStore(clientWith(() => Response.json({ message: "Config unreadable" }, { status: 500 })));
  await settings.load();
  assert.equal(settings.state, "error");
  assert.equal(settings.loadError, "Config unreadable");
  assert.equal(settings.captureAutoPaste(null), false);
  assert.equal(settings.captureAutoPaste(true), true);
  const root = new RootStore();
  runInAction(() => { root.noteSettings = settings; });
  const html = renderToStaticMarkup(<RootStoreProvider value={root}><SettingsCapture /></RootStoreProvider>);
  assert.match(html, /Config unreadable/);
  assert.match(html, /Retry loading/);
  assert.match(html, /aria-checked="false"/);
});

test("a failed save is not shown as confirmed and reload recovers the actual preference", async () => {
  let release: ((response: Response) => void) | undefined;
  const settings = new AudioNoteSettingsStore(clientWith((request) => request.method === "PUT" ? new Promise<Response>((resolve) => { release = resolve; }) : Response.json({ auto_paste: false })));
  await settings.load();
  const saving = settings.saveAutoPaste(true);
  assert.equal(settings.saving, true);
  assert.equal(settings.effectiveDefault, false);
  assert.equal(await settings.saveAutoPaste(false), false);
  assert.ok(release);
  release(Response.json({ message: "Config is read-only" }, { status: 500 }));
  assert.equal(await saving, false);
  assert.equal(settings.saveError, "Config is read-only");
  assert.equal(settings.captureAutoPaste(null), false);
  await settings.load();
  assert.equal(settings.state, "loaded");
  assert.equal(settings.saveError, null);
});

test("changing the saved preference does not send capture lifecycle commands", async () => {
  const requests: string[] = [];
  const settings = new AudioNoteSettingsStore(clientWith((request) => {
    requests.push(`${request.method} ${new URL(request.url).pathname}`);
    return Response.json({ auto_paste: request.method === "PUT" });
  }));
  await settings.load();
  await settings.saveAutoPaste(true);
  assert.deepEqual(requests, ["GET /api/audio-notes/settings", "PUT /api/audio-notes/settings"]);
});
