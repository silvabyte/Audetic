import { existsSync } from "node:fs";
import { afterAll, beforeAll, expect, test } from "bun:test";

import {
  CODEX_ARGS,
  ENRICHMENT_ERROR_TEXT,
  ERROR_TRANSCRIPT,
  INTERRUPTED_ARTIFACT,
  INTERRUPTED_ENRICHMENT,
  INTERRUPTED_IMPORT,
  INTERRUPTED_RECORDING,
  INTERRUPTED_SYNC,
  INTERRUPTED_TRANSCRIBING,
  MANUAL_TITLE,
  MEETING_KIND,
  NOTE_IDS,
} from "./src/contract.ts";
import { type DaemonSession, startDaemon } from "./src/daemon.ts";
import {
  migrationChecksums,
  readImport,
  readMigrations,
  readNotes,
  readRecordingSource,
} from "./src/fixture.ts";
import { getJson, openApiOperations, request, unclassifiedOperations } from "./src/http.ts";
import {
  AgentProfiles,
  ApiErrorBody,
  Artifacts,
  Artifact,
  AudioNoteDetail,
  AudioNoteSettings,
  AudioNoteStatus,
  AudioNotesList,
  Classifications,
  ExternalImports,
  InstallStatus,
  IntegrationOverview,
  AccessKeys,
  KeybindStatuses,
  LogsResult,
  MissingRoute,
  ModelDescriptor,
  ModelsList,
  NotFoundMessage,
  OpenApiDocument,
  PostProcessingEvents,
  PostProcessingJobs,
  ProviderInfo,
  ProviderRuntime,
  RecentTitles,
  ServiceInfo,
  SetupAssessment,
  SummaryTemplates,
  SystemDeps,
  VersionInfo,
  WaybarStatus,
  WhisperConfig,
  decode,
} from "./src/schema.ts";

let session: DaemonSession;
let openApiPaths: Record<string, Record<string, unknown>> = {};

beforeAll(async () => {
  session = await startDaemon("migration2");
}, 60_000);

afterAll(async () => {
  const gaps =
    Object.keys(openApiPaths).length === 0 ? [] : unclassifiedOperations(openApiPaths);
  await session?.stop();
  if (gaps.length > 0) {
    throw new Error(`OpenAPI operations are not classified:\n${gaps.join("\n")}`);
  }
});

test("migration 2 upgrades to 3 and records the SQL checksums", () => {
  const expected = migrationChecksums();
  const live = readMigrations(session.dbPath);
  expect(live.map((row) => row.version)).toEqual([1, 2, 3]);
  expect(live.map((row) => row.checksum)).toEqual(expected.map((row) => row.checksum));
  for (const checksum of live) {
    expect(checksum.checksum).toMatch(/^[0-9a-f]{64}$/);
  }

  expect(existsSync(session.backupPath)).toBe(true);
  const backup = readMigrations(session.backupPath);
  expect(backup.map((row) => row.version)).toEqual([1, 2]);
  expect(backup.map((row) => row.checksum)).toEqual(
    expected.slice(0, 2).map((row) => row.checksum),
  );

  const saved = readNotes(session.backupPath);
  expect(saved.find((note) => note.id === NOTE_IDS.recording)?.status).toBe("recording");
  expect(saved.find((note) => note.id === NOTE_IDS.enrichmentRunning)?.enrichment_status).toBe(
    "running",
  );
  expect(saved.find((note) => note.id === NOTE_IDS.enrichmentError)?.enrichment_error).toBe(
    ENRICHMENT_ERROR_TEXT,
  );
  expect(readImport(session.backupPath)).toEqual({ status: "pending", error: null });
  expect(readRecordingSource(session.backupPath)).toEqual({ running: 1, last_error: null });
});

test("startup sweep marks the in-flight recording and leaves the deleted row", async () => {
  const detail = decode(
    AudioNoteDetail,
    (await getJson(session.baseUrl, `/api/audio-notes/${NOTE_IDS.recording}`)).body,
    "recording detail",
  );
  expect(detail.status).toBe("error");
  expect(detail.error).toBe(INTERRUPTED_RECORDING);
  expect(detail.enrichment_status).toBe("pending");
  expect(detail.enrichment_error).toBeNull();
  expect(detail.completed_at).toMatch(/^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$/);

  const transcribing = decode(
    AudioNoteDetail,
    (await getJson(session.baseUrl, `/api/audio-notes/${NOTE_IDS.transcribing}`)).body,
    "transcribing detail",
  );
  expect(transcribing.status).toBe("error");
  expect(transcribing.error).toBe(INTERRUPTED_TRANSCRIBING);

  const stored = readNotes(session.dbPath);
  const deleted = stored.find((note) => note.id === NOTE_IDS.deleted);
  expect(deleted?.status).toBe("recording");
  expect(deleted?.deleted_at).not.toBeNull();

  const hidden = await getJson(session.baseUrl, `/api/audio-notes/${NOTE_IDS.deleted}`);
  expect(hidden.status).toBe(404);
  const missing = decode(NotFoundMessage, hidden.body, "deleted note");
  expect(missing.message).toBe(`AudioNote ${NOTE_IDS.deleted} not found`);
});

test("enrichment error text round-trips and a running row is swept", async () => {
  const list = decode(
    AudioNotesList,
    (await getJson(session.baseUrl, "/api/audio-notes?limit=50")).body,
    "note list",
  );
  const failed = list.notes.find((note) => note.id === NOTE_IDS.enrichmentError);
  expect(failed?.enrichment_status).toBe("error");
  expect(failed?.enrichment_error).toBe(ENRICHMENT_ERROR_TEXT);
  expect(failed?.title).toBe(MANUAL_TITLE);
  expect(failed?.title_source).toBe("manual");
  expect(failed?.classification_kind).toBe(MEETING_KIND);
  expect(failed?.transcript_text).toBe(ERROR_TRANSCRIPT);

  const detailResponse = await getJson(
    session.baseUrl,
    `/api/audio-notes/${NOTE_IDS.enrichmentError}`,
  );
  expect(detailResponse.status).toBe(200);
  const detail = decode(AudioNoteDetail, detailResponse.body, "enrichment detail");
  expect(detail.enrichment_status).toBe("error");
  expect(detail.enrichment_error).toBe(ENRICHMENT_ERROR_TEXT);
  expect(detail.transcript_text).toBe(ERROR_TRANSCRIPT);
  expect(detail.transcript_segments).toEqual([
    { start: 0, end: 1.25, text: ERROR_TRANSCRIPT },
  ]);
  expect(detail.status).toBe("completed");
  expect(detail.error).toBeNull();
  expect(detail.classification).toEqual({ kind: MEETING_KIND });

  const running = decode(
    AudioNoteDetail,
    (await getJson(session.baseUrl, `/api/audio-notes/${NOTE_IDS.enrichmentRunning}`)).body,
    "running enrichment detail",
  );
  expect(running.status).toBe("completed");
  expect(running.enrichment_status).toBe("error");
  expect(running.enrichment_error).toBe(INTERRUPTED_ENRICHMENT);
  expect(running.title_source).toBe("generated");
});

test("note list order, search, and kind filter follow the stored rows", async () => {
  const list = decode(
    AudioNotesList,
    (await getJson(session.baseUrl, "/api/audio-notes?limit=50")).body,
    "ordered list",
  );
  expect(list.notes.map((note) => note.id)).toEqual([
    NOTE_IDS.completed,
    NOTE_IDS.enrichmentRunning,
    NOTE_IDS.enrichmentError,
    NOTE_IDS.transcribing,
    NOTE_IDS.recording,
  ]);
  expect(list.notes.some((note) => note.id === NOTE_IDS.deleted)).toBe(false);

  const newest = decode(
    AudioNotesList,
    (await getJson(session.baseUrl, "/api/audio-notes?limit=1")).body,
    "limited list",
  );
  expect(newest.notes.map((note) => note.id)).toEqual([NOTE_IDS.completed]);

  const found = decode(
    AudioNotesList,
    (await getJson(session.baseUrl, "/api/audio-notes?query=macbook")).body,
    "query list",
  );
  expect(found.notes.map((note) => note.id)).toEqual([NOTE_IDS.enrichmentError]);

  const byKind = decode(
    AudioNotesList,
    (await getJson(session.baseUrl, `/api/audio-notes?kind=${MEETING_KIND}`)).body,
    "kind list",
  );
  expect(byKind.notes.map((note) => note.id)).toEqual([NOTE_IDS.enrichmentError]);

  const empty = decode(
    AudioNotesList,
    (await getJson(session.baseUrl, "/api/audio-notes?query=no-such-note")).body,
    "empty list",
  );
  expect(empty.notes).toEqual([]);
});

test("version, service identity, and the OpenAPI document", async () => {
  const versionResponse = await getJson(session.baseUrl, "/api/version");
  expect(versionResponse.status).toBe(200);
  const version = decode(VersionInfo, versionResponse.body, "version");
  expect(version.instance_id).toMatch(
    /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/,
  );

  const status = decode(
    ServiceInfo,
    (await getJson(session.baseUrl, "/api")).body,
    "service",
  );
  expect(status.version).toBe(version.version);

  const specResponse = await getJson(session.baseUrl, "/api/openapi.json");
  expect(specResponse.status).toBe(200);
  const spec = decode(OpenApiDocument, specResponse.body, "openapi");
  expect(spec.openapi.startsWith("3.")).toBe(true);
  expect(spec.info.title).toBe("Audetic daemon API");
  expect(spec.info.version).toBe(version.version);
  expect(spec.servers[0]?.url).toBe("http://127.0.0.1:3737/api");

  const operations = openApiOperations(spec.paths);
  expect(operations).toHaveLength(67);
  const paths = new Set(operations.map((operation) => operation.path));
  expect(paths.has("/version")).toBe(true);
  expect(paths.has("/audio-notes")).toBe(true);
  expect(paths.has("/audio-notes/{id}")).toBe(true);
  expect(paths.has("/openapi.json")).toBe(false);
  expect(paths.has("/history")).toBe(false);
  expect(paths.has("/meetings")).toBe(false);

  const enrichment = spec.components.schemas.EnrichmentStatus as {
    enum?: readonly string[];
  };
  expect(enrichment.enum?.slice().sort()).toEqual([
    "completed",
    "error",
    "pending",
    "running",
    "skipped",
  ]);
  openApiPaths = spec.paths;
});

test("capture status is idle and does not open a device", async () => {
  const status = decode(
    AudioNoteStatus,
    (await getJson(session.baseUrl, "/api/audio-notes/status")).body,
    "status",
  );
  expect(status).toMatchObject({
    active: false,
    capture_degraded: false,
    note_id: null,
    phase: "idle",
    duration_seconds: null,
    title: null,
    audio_path: null,
    last_error: null,
  });

  const waybar = decode(
    WaybarStatus,
    (await getJson(session.baseUrl, "/api/audio-notes/status?style=waybar")).body,
    "waybar status",
  );
  expect(waybar).toEqual({
    text: "",
    class: "audetic-note-idle",
    tooltip: "No active capture",
  });
});

test("settings, classifications, recent titles, artifacts, and audio bytes", async () => {
  const settings = decode(
    AudioNoteSettings,
    (await getJson(session.baseUrl, "/api/audio-notes/settings")).body,
    "settings",
  );
  expect(settings).toEqual({
    auto_paste: { microphone: false, microphone_and_system: false },
    ai_processing: { enabled: true },
  });

  const kinds = decode(
    Classifications,
    (await getJson(session.baseUrl, "/api/audio-notes/classifications")).body,
    "classifications",
  );
  expect(kinds.kinds).toEqual([MEETING_KIND]);

  const titles = decode(
    RecentTitles,
    (await getJson(session.baseUrl, "/api/audio-notes/recent-titles")).body,
    "recent titles",
  );
  expect(titles.titles).toEqual([MANUAL_TITLE]);

  const artifacts = decode(
    Artifacts,
    (await getJson(session.baseUrl, `/api/audio-notes/${NOTE_IDS.completed}/artifacts`)).body,
    "artifacts",
  );
  expect(artifacts.artifacts).toHaveLength(1);
  const artifact = artifacts.artifacts[0];
  expect(artifact?.status).toBe("error");
  expect(artifact?.error).toBe(INTERRUPTED_ARTIFACT);
  expect(artifact?.note_id).toBe(NOTE_IDS.completed);
  expect(artifact?.kind).toBe("summary");
  expect(artifact?.title).toBe("Kept summary");

  const one = decode(
    Artifact,
    (
      await getJson(
        session.baseUrl,
        `/api/audio-notes/${NOTE_IDS.completed}/artifacts/${artifact?.id}`,
      )
    ).body,
    "artifact detail",
  );
  expect(one.id).toBe(artifact?.id);
  expect(one.error).toBe(INTERRUPTED_ARTIFACT);

  const audio = await getJson(
    session.baseUrl,
    `/api/audio-notes/${NOTE_IDS.enrichmentError}/audio`,
  );
  expect(audio.status).toBe(200);
  expect(Buffer.isBuffer(audio.body)).toBe(true);
  expect(Buffer.from(audio.body as Buffer).toString("utf8")).toBe(
    "audetic-characterization-audio",
  );
  expect(audio.headers.get("content-type") ?? "").toContain("audio/");

  const missingAudio = await getJson(
    session.baseUrl,
    `/api/audio-notes/${NOTE_IDS.recording}/audio`,
  );
  expect(missingAudio.status).toBe(404);
  const missingBody = decode(NotFoundMessage, missingAudio.body, "missing audio");
  expect(missingBody.message).toBe(`Audio for note ${NOTE_IDS.recording} not found`);
});

test("built-in agents keep the Codex argv and summary templates stay data", async () => {
  const agents = decode(
    AgentProfiles,
    (await getJson(session.baseUrl, "/api/agent-profiles")).body,
    "agents",
  );
  const codex = agents.profiles.find((profile) => profile.kind === "codex");
  expect(codex?.args).toEqual([...CODEX_ARGS]);
  expect(codex?.args.includes("--model")).toBe(false);
  expect(codex?.prompt_mode).toBe("stdin");
  expect(codex?.executable).toBe("codex");
  expect(agents.profiles.map((profile) => profile.kind).sort()).toEqual([
    "claude",
    "codex",
    "cursor_agent",
    "cursor_agent",
    "opencode",
  ]);
  expect(agents.profiles.find((profile) => profile.default_profile)?.kind).toBe("claude");

  const templates = decode(
    SummaryTemplates,
    (await getJson(session.baseUrl, "/api/summary/templates")).body,
    "templates",
  );
  expect(templates.templates.map((template) => template.id).sort()).toEqual(
    [
      "action_items",
      "cleaned_dictation",
      "concise_summary",
      "conversation",
      "daily_standup",
      "general_note",
      "mind_map",
      "project_sync",
      "request_intent",
      "retrospective",
      "shopping_items",
      "standard_meeting",
      "talking_points",
    ].sort(),
  );
});

test("integrations, provider, and models are readable without a network call", async () => {
  const overview = decode(
    IntegrationOverview,
    (await getJson(session.baseUrl, "/api/integrations")).body,
    "integrations",
  );
  expect(overview.public_base_url).toBe("https://ingest.audetic.link");
  expect(overview.index_webhook_url).toBe("https://ingest.audetic.link/v1/index");
  expect(overview.generic_audio_url).toBe("https://ingest.audetic.link/v1/audio");
  expect(overview.ingress_origin).toBe(`http://127.0.0.1:${session.ingressPort}`);
  expect(overview.upload_limit_bytes).toBe(50 * 1024 * 1024);

  const keys = decode(
    AccessKeys,
    (await getJson(session.baseUrl, "/api/integrations/keys")).body,
    "keys",
  );
  expect(keys.keys).toEqual([]);

  const imports = decode(
    ExternalImports,
    (await getJson(session.baseUrl, "/api/integrations/imports")).body,
    "imports",
  );
  expect(imports.imports).toEqual([
    expect.objectContaining({
      provider: "plaud",
      external_id: "ext-pending",
      status: "failed",
      audio_note_id: NOTE_IDS.completed,
      error: INTERRUPTED_IMPORT,
      attempt_count: 1,
    }),
  ]);
  expect(readRecordingSource(session.dbPath)).toEqual({
    running: 0,
    last_error: INTERRUPTED_SYNC,
  });

  const provider = decode(
    ProviderInfo,
    (await getJson(session.baseUrl, "/api/provider")).body,
    "provider",
  );
  expect(provider).toMatchObject({
    provider: "audetic-api",
    model: "base",
    language: "en",
    api_endpoint: null,
    has_api_key: false,
    command_path: null,
    model_path: null,
  });

  const providerStatus = await getJson(session.baseUrl, "/api/provider/status");
  expect(providerStatus.status).toBe(200);
  expect(providerStatus.body).toEqual({
    status: "ready",
    provider: "audetic-api",
    model: "base",
    language: "en",
  });

  const runtime = decode(
    ProviderRuntime,
    (await getJson(session.baseUrl, "/api/provider/runtime")).body,
    "provider runtime",
  );
  expect(runtime.restart_required).toBe(false);
  expect(runtime.active.provider).toBe("audetic-api");
  expect(runtime.persisted.provider).toBe("audetic-api");

  const raw = decode(
    WhisperConfig,
    (await getJson(session.baseUrl, "/api/provider/config")).body,
    "provider config",
  );
  expect(raw.provider).toBe("audetic-api");
  expect(raw.api_key).toBeNull();

  const models = decode(
    ModelsList,
    (await getJson(session.baseUrl, "/api/models")).body,
    "models",
  );
  expect(models.models.map((model) => model.id).sort()).toEqual([
    "parakeet-tdt-0.6b-v2",
    "parakeet-tdt-0.6b-v3",
    "whisper-base.en",
    "whisper-large-v3",
  ]);
  expect(models.models.every((model) => model.installed === false)).toBe(true);
  expect(models.models.find((model) => model.recommended)?.id).toBe("parakeet-tdt-0.6b-v3");

  const one = decode(
    ModelDescriptor,
    (await getJson(session.baseUrl, "/api/models/parakeet-tdt-0.6b-v3")).body,
    "one model",
  );
  expect(one.installed).toBe(false);
  expect(one.engine).toBe("parakeet");
  expect(one.download).toBeNull();

  const unknown = await getJson(session.baseUrl, "/api/models/no-such-model");
  expect(unknown.status).toBe(404);
  const unknownBody = decode(ApiErrorBody, unknown.body, "unknown model");
  expect(unknownBody.message).toContain("no-such-model");
});

test("setup, system, jobs, keybinds, and logs stay local", async () => {
  const setup = decode(
    SetupAssessment,
    (await getJson(session.baseUrl, "/api/setup")).body,
    "setup",
  );
  const expectedOs = process.platform === "darwin" ? "macos" : process.platform;
  const expectedArch =
    process.arch === "x64" ? "x86_64" : process.arch === "arm64" ? "aarch64" : process.arch;
  expect(setup.platform.os).toBe(expectedOs);
  expect(setup.platform.architecture).toBe(expectedArch);
  expect(setup.restart_required).toBe(false);
  expect(setup.capabilities.map((capability) => capability.id).sort()).toEqual([
    "clipboard_fallback",
    "ffmpeg",
    "hyprland_config",
    "hyprland_session",
    "note_keybind",
    "omarchy",
    "system_audio",
    "system_note_keybind",
    "text_delivery",
    "transcription_provider",
  ]);

  const deps = decode(
    SystemDeps,
    (await getJson(session.baseUrl, "/api/system/deps")).body,
    "deps",
  );
  expect(deps.ffmpeg).toBe(true);

  const install = decode(
    InstallStatus,
    (await getJson(session.baseUrl, "/api/system/install-ffmpeg/status")).body,
    "ffmpeg install",
  );
  expect(install.phase).toBe("idle");
  expect(install.binaryPath).toBeNull();

  const events = decode(
    PostProcessingEvents,
    (await getJson(session.baseUrl, "/api/post-processing/events")).body,
    "events",
  );
  expect(events.events.map((event) => event.name)).toEqual(["audio_note.completed"]);

  const jobs = decode(
    PostProcessingJobs,
    (await getJson(session.baseUrl, "/api/post-processing/jobs")).body,
    "jobs",
  );
  expect(jobs.jobs).toEqual([]);

  const missingJob = await getJson(session.baseUrl, "/api/post-processing/jobs/404");
  expect(missingJob.status).toBe(404);
  expect(decode(ApiErrorBody, missingJob.body, "missing job").message).toBe("Job 404 not found");

  const keybinds = decode(
    KeybindStatuses,
    (await getJson(session.baseUrl, "/api/keybind/status")).body,
    "keybinds",
  );
  expect(keybinds.note).toEqual({ status: "no_config", target: "note" });
  expect(keybinds.system_note).toEqual({ status: "no_config", target: "system-note" });

  const logs = decode(LogsResult, (await getJson(session.baseUrl, "/api/logs")).body, "logs");
  expect(Array.isArray(logs.app_logs)).toBe(true);
  expect(logs.transcriptions.map((entry) => entry.text)).toContain(ERROR_TRANSCRIPT);
});

test("browser origins are allow-listed and unknown routes 404", async () => {
  const forbidden = await request(session.baseUrl, "GET", "/api/version", {
    headers: { Origin: "https://example.com" },
  });
  expect(forbidden.status).toBe(403);

  const allowed = await request(session.baseUrl, "GET", "/api/version", {
    headers: { Origin: session.baseUrl },
  });
  expect(allowed.status).toBe(200);
  decode(VersionInfo, allowed.body, "allowed origin version");

  const missing = await getJson(session.baseUrl, "/api/does-not-exist");
  expect(missing.status).toBe(404);
  decode(MissingRoute, missing.body, "missing route");
});
