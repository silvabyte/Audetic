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

const OFF = { microphone: false, microphone_and_system: false };
const MIC_ONLY = { microphone: true, microphone_and_system: false };
const PROCESSING_ON = { enabled: true };

function clientWith(handler: (request: Request) => Response | Promise<Response>) {
  return createClient<paths>({ baseUrl: "http://audetic.test/api", fetch: async (request) => handler(request) });
}

test("automatic paste is safely off for every source before and during the initial settings load", async () => {
  let release: ((response: Response) => void) | undefined;
  const store = new AudioNoteSettingsStore(clientWith(() => new Promise<Response>((resolve) => { release = resolve; })));
  assert.equal(store.savedAutoPaste("microphone"), false);
  assert.equal(store.savedAutoPaste("microphone_and_system"), false);
  assert.equal(store.captureAutoPaste(null), false);
  const loading = store.load();
  assert.equal(store.state, "loading");
  assert.equal(store.savedAutoPaste("microphone"), false);
  assert.equal(store.savedAutoPaste("microphone_and_system"), false);
  assert.equal(store.captureAutoPaste(null), false);
  assert.ok(release);
  release(Response.json({ auto_paste: MIC_ONLY }));
  await loading;
  assert.equal(store.savedAutoPaste("microphone"), true);
  assert.equal(store.savedAutoPaste("microphone_and_system"), false);
  assert.equal(store.captureAutoPaste(null), null);
});

test("saving uses the real settings API and a fresh store reads each source's preference", async () => {
  let persisted = { ...OFF };
  const requests: string[] = [];
  const client = clientWith(async (request) => {
    requests.push(`${request.method} ${new URL(request.url).pathname}`);
    if (request.method === "PUT") {
      const body: unknown = await request.json();
      assert.deepEqual(body, { auto_paste: MIC_ONLY, ai_processing: PROCESSING_ON });
      persisted = { ...MIC_ONLY };
    }
    return Response.json({ auto_paste: persisted });
  });
  const first = new AudioNoteSettingsStore(client);
  await first.load();
  assert.equal(first.savedAutoPaste("microphone"), false);
  assert.equal(first.savedAutoPaste("microphone_and_system"), false);
  assert.equal(await first.saveAutoPaste("microphone", true), true);
  const reopened = new AudioNoteSettingsStore(client);
  await reopened.load();
  assert.equal(reopened.savedAutoPaste("microphone"), true);
  assert.equal(reopened.savedAutoPaste("microphone_and_system"), false);
  assert.deepEqual(requests, ["GET /api/audio-notes/settings", "PUT /api/audio-notes/settings", "GET /api/audio-notes/settings"]);
});

test("capture inherits the saved preference with null while explicit false and true override it", async () => {
  const bodies: unknown[] = [];
  const client = clientWith(async (request) => {
    const path = new URL(request.url).pathname;
    if (path.endsWith("/settings")) return Response.json({ auto_paste: MIC_ONLY });
    if (path.endsWith("/start")) {
      bodies.push(await request.json());
      return Response.json({ success: true, note_id: 1, audio_path: "/isolated/1.wav", capture_state: "mic_only", message: "Started" });
    }
    if (path.endsWith("/status")) return Response.json({ active: true, capture_degraded: false, note_id: 1, phase: "recording", duration_seconds: 0, title: null, audio_path: "/isolated/1.wav", last_error: null });
    return Response.json({ notes: [] });
  });
  const settings = new AudioNoteSettingsStore(client);
  await settings.load();
  assert.equal(settings.savedAutoPaste("microphone"), true);
  assert.equal(settings.savedAutoPaste("microphone_and_system"), false);
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
  assert.equal(settings.savedAutoPaste("microphone"), false);
  assert.equal(settings.savedAutoPaste("microphone_and_system"), false);
  const root = new RootStore();
  runInAction(() => { root.noteSettings = settings; });
  const html = renderToStaticMarkup(<RootStoreProvider value={root}><SettingsCapture /></RootStoreProvider>);
  assert.match(html, /Config unreadable/);
  assert.match(html, /Retry loading/);
  assert.match(html, /Microphone notes \(dictation\)/);
  assert.match(html, /Microphone \+ system audio notes/);
  assert.equal(html.match(/aria-checked="false"/g)?.length, 3);
  assert.equal(html.match(/aria-checked="true"/g)?.length, undefined);
  assert.match(html, /Automatic AI processing/);
  assert.doesNotMatch(html, /id="default-note-agent"/);
  assert.doesNotMatch(html, /The saved agent is unchanged/);
});

test("the settings page shows the saved automatic-paste preference for each capture source", async () => {
  const settings = new AudioNoteSettingsStore(clientWith(() => Response.json({ auto_paste: MIC_ONLY })));
  await settings.load();
  const root = new RootStore();
  runInAction(() => { root.noteSettings = settings; });
  const html = renderToStaticMarkup(<RootStoreProvider value={root}><SettingsCapture /></RootStoreProvider>);
  assert.match(html, /Saved preference: Microphone notes \(dictation\) on, Microphone \+ system audio notes off\./);
  assert.match(html, /id="saved-auto-paste-microphone"[^>]*aria-checked="true"|aria-checked="true"[^>]*id="saved-auto-paste-microphone"/);
  assert.match(html, /id="saved-auto-paste-microphone-and-system"[^>]*aria-checked="false"|aria-checked="false"[^>]*id="saved-auto-paste-microphone-and-system"/);
});

test("a failed save is not shown as confirmed and reload recovers the actual preference", async () => {
  let release: ((response: Response) => void) | undefined;
  const settings = new AudioNoteSettingsStore(clientWith((request) => request.method === "PUT" ? new Promise<Response>((resolve) => { release = resolve; }) : Response.json({ auto_paste: OFF })));
  await settings.load();
  const saving = settings.saveAutoPaste("microphone", true);
  assert.equal(settings.saving, true);
  assert.equal(settings.savedAutoPaste("microphone"), false);
  assert.equal(await settings.saveAutoPaste("microphone_and_system", false), false);
  assert.ok(release);
  release(Response.json({ message: "Config is read-only" }, { status: 500 }));
  assert.equal(await saving, false);
  assert.equal(settings.saveError, "Config is read-only");
  assert.equal(settings.captureAutoPaste(null), false);
  await settings.load();
  assert.equal(settings.state, "loaded");
  assert.equal(settings.saveError, null);
  assert.equal(settings.savedAutoPaste("microphone"), false);
  assert.equal(settings.savedAutoPaste("microphone_and_system"), false);
});

test("turning automatic processing off persists the switch and hides the agent select", async () => {
  let persisted = { auto_paste: OFF, ai_processing: PROCESSING_ON };
  const settings = new AudioNoteSettingsStore(clientWith(async (request) => {
    if (request.method === "PUT") persisted = await request.json() as typeof persisted;
    return Response.json(persisted);
  }));
  await settings.load();
  assert.equal(settings.automaticProcessingEnabled(), true);
  assert.equal(await settings.saveAiProcessing(false), true);
  assert.equal(settings.automaticProcessingEnabled(), false);
  assert.deepEqual(persisted.ai_processing, { enabled: false });
  assert.deepEqual(persisted.auto_paste, OFF);
  const root = new RootStore();
  runInAction(() => { root.noteSettings = settings; });
  const html = renderToStaticMarkup(<RootStoreProvider value={root}><SettingsCapture /></RootStoreProvider>);
  assert.match(html, /id="ai-processing-enabled"[^>]*aria-checked="false"|aria-checked="false"[^>]*id="ai-processing-enabled"/);
  assert.doesNotMatch(html, /id="default-note-agent"/);
  assert.match(html, /The saved agent is unchanged/);
  assert.match(html, /Process note/);
});

test("changing the saved preference does not send capture lifecycle commands", async () => {
  const requests: string[] = [];
  const settings = new AudioNoteSettingsStore(clientWith((request) => {
    requests.push(`${request.method} ${new URL(request.url).pathname}`);
    const enabled = request.method === "PUT";
    return Response.json({ auto_paste: { microphone: enabled, microphone_and_system: false } });
  }));
  await settings.load();
  await settings.saveAutoPaste("microphone", true);
  assert.deepEqual(requests, ["GET /api/audio-notes/settings", "PUT /api/audio-notes/settings"]);
  assert.equal(settings.savedAutoPaste("microphone"), true);
  assert.equal(settings.savedAutoPaste("microphone_and_system"), false);
});
