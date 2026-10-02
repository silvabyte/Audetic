import { makeAutoObservable, runInAction } from "mobx";
import { daemon } from "@/api/client";
import type { components } from "@/api/schema";
import { errorMessage, isClassificationSlug, noteNeedsRefresh } from "@/lib/audio-notes";
import { DEFAULT_CAPTURE_OPTIONS } from "@/lib/capture-options";

export type AudioNoteSummary = components["schemas"]["AudioNoteSummary"];
export type AudioNoteDetail = components["schemas"]["AudioNoteDetailResponse"];
export type CaptureOptions = components["schemas"]["AudioNoteStartRequest"];
type ClassificationResponse = components["schemas"]["AudioNoteClassificationResponse"];
type LoadState = "idle" | "loading" | "loaded" | "error";
type ClassificationMutationState = "idle" | "saving" | "error";

/** The sole capture/status owner. Meaning is inferred after the raw transcript is saved. */
export class AudioNotesStore {
  active = false;
  phase = "idle";
  noteId: number | null = null;
  title: string | null = null;
  durationSeconds = 0;
  captureDegraded = false;
  reachable = true;
  firstPollDone = false;
  lastError: string | null = null;
  commandPending = false;
  list: AudioNoteSummary[] = [];
  listStatus: LoadState = "idle";
  listError: string | null = null;
  query = "";
  kind = "";
  offset = 0;
  readonly pageSize = 30;
  hasMore = false;
  detailCache: Record<number, AudioNoteDetail> = {};
  detailStatus: Record<number, LoadState> = {};
  detailError: Record<number, string | null> = {};
  recentTitles: string[] = [];
  recentTitlesStatus: LoadState = "idle";
  recentTitlesError: string | null = null;
  titleMutationStatus: Record<number, "idle" | "saving" | "generating" | "error"> = {};
  titleMutationError: Record<number, string | null> = {};
  processing: Record<number, boolean> = {};
  classificationKinds: string[] = [];
  classificationKindsStatus: LoadState = "idle";
  classificationKindsError: string | null = null;
  classificationMutationStatus: Record<number, ClassificationMutationState> = {};
  classificationMutationError: Record<number, string | null> = {};
  private timer: ReturnType<typeof setTimeout> | null = null;
  private running = false;
  private polling = false;
  private listEpoch = 0;
  private listPending = 0;
  private detailEpoch = new Map<number, number>();
  private classificationKindsEpoch = 0;
  private deletedIds = new Set<number>();

  constructor(private client = daemon) {
    makeAutoObservable<this, "client" | "timer" | "running" | "polling" | "listEpoch" | "listPending" | "detailEpoch" | "classificationKindsEpoch" | "deletedIds">(
      this, { client: false, timer: false, running: false, polling: false, listEpoch: false, listPending: false, detailEpoch: false, classificationKindsEpoch: false, deletedIds: false },
    );
  }

  start(): void { if (!this.running) { this.running = true; void this.pollStatus(); } }
  stop(): void { this.running = false; if (this.timer) clearTimeout(this.timer); this.timer = null; }

  async startCapture(options: CaptureOptions = DEFAULT_CAPTURE_OPTIONS): Promise<boolean> {
    return this.captureCommand(async () => this.client.POST("/audio-notes/start", { body: options }));
  }
  async stopCapture(): Promise<boolean> {
    return this.captureCommand(async () => this.client.POST("/audio-notes/stop"));
  }
  async cancelCapture(): Promise<boolean> {
    return this.captureCommand(async () => this.client.POST("/audio-notes/cancel"));
  }
  async confirmCapture(start_seconds?: number, end_seconds?: number): Promise<boolean> {
    return this.captureCommand(async () => this.client.POST("/audio-notes/confirm", { body: { start_seconds, end_seconds } }));
  }
  private async captureCommand(command: () => Promise<{ error?: unknown }>): Promise<boolean> {
    if (this.commandPending) return false;
    this.commandPending = true;
    this.lastError = null;
    try {
      const { error } = await command();
      if (error) throw new Error(errorMessage(error));
      await this.pollStatus();
      void this.loadList(true);
      return true;
    } catch (error) {
      runInAction(() => { this.lastError = errorMessage(error); });
      return false;
    } finally { runInAction(() => { this.commandPending = false; }); }
  }

  async importFile(file: File, title?: string): Promise<number | null> {
    const form = new FormData();
    form.append("file", file);
    if (title?.trim()) form.append("title", title.trim());
    this.lastError = null;
    try {
      const { data, error } = await this.client.POST("/audio-notes/import", {
        // The typed multipart fields describe the form; the serializer supplies browser-owned boundaries.
        body: { file: "", title },
        bodySerializer: () => form,
        headers: { "Content-Type": null },
      });
      if (error || !data) throw new Error(errorMessage(error ?? "Empty response"));
      void this.loadList(true);
      return data.note_id;
    } catch (error) { runInAction(() => { this.lastError = errorMessage(error); }); return null; }
  }

  async setFilters(query: string, kind: string): Promise<void> {
    this.query = query.trim(); this.kind = kind.trim(); this.offset = 0;
    await this.loadList();
  }
  async setPage(offset: number): Promise<void> {
    this.offset = Math.max(0, offset); await this.loadList();
  }
  async loadList(silent = false): Promise<void> {
    if (silent && this.listPending > 0) return;
    this.listPending++;
    const epoch = ++this.listEpoch;
    if (!silent) this.listStatus = "loading";
    this.listError = null;
    try {
      const { data, error } = await this.client.GET("/audio-notes", {
        params: { query: { query: this.query || undefined, kind: this.kind || undefined, limit: this.pageSize + 1, offset: this.offset } },
      });
      if (epoch !== this.listEpoch) return;
      if (error || !data) throw new Error(errorMessage(error ?? "Empty response"));
      runInAction(() => {
        this.hasMore = data.notes.length > this.pageSize;
        this.list = data.notes.slice(0, this.pageSize).filter((note) => !this.deletedIds.has(note.id));
        this.listStatus = "loaded";
      });
    } catch (error) {
      if (epoch !== this.listEpoch) return;
      runInAction(() => { this.listError = errorMessage(error); this.listStatus = "error"; });
    } finally { this.listPending--; }
  }
  async loadDetail(id: number, force = false): Promise<void> {
    if ((!force && this.detailStatus[id] === "loading") || this.deletedIds.has(id)) return;
    const epoch = (this.detailEpoch.get(id) ?? 0) + 1;
    this.detailEpoch.set(id, epoch);
    this.detailStatus[id] = "loading";
    this.detailError[id] = null;
    try {
      const { data, error } = await this.client.GET("/audio-notes/{id}", { params: { path: { id } } });
      if (this.deletedIds.has(id) || epoch !== this.detailEpoch.get(id)) return;
      if (error || !data) throw new Error(errorMessage(error ?? "Empty response"));
      runInAction(() => { this.detailCache[id] = data; this.detailStatus[id] = "loaded"; });
    } catch (error) {
      if (this.deletedIds.has(id) || epoch !== this.detailEpoch.get(id)) return;
      runInAction(() => { this.detailStatus[id] = "error"; this.detailError[id] = errorMessage(error); });
    }
  }
  async loadClassificationKinds(force = false): Promise<void> {
    if (!force && this.classificationKindsStatus === "loading") return;
    const epoch = ++this.classificationKindsEpoch;
    this.classificationKindsStatus = "loading";
    this.classificationKindsError = null;
    try {
      const { data, error } = await this.client.GET("/audio-notes/classifications");
      if (epoch !== this.classificationKindsEpoch) return;
      if (error || !data) throw new Error(errorMessage(error ?? "Empty response"));
      runInAction(() => {
        this.classificationKinds = data.kinds;
        this.classificationKindsStatus = "loaded";
      });
    } catch (error) {
      if (epoch !== this.classificationKindsEpoch) return;
      runInAction(() => {
        this.classificationKindsError = errorMessage(error);
        this.classificationKindsStatus = "error";
      });
    }
  }
  async setClassification(id: number, kind: string): Promise<boolean> {
    const normalized = kind.trim();
    if (this.classificationMutationStatus[id] === "saving") return false;
    this.classificationMutationError[id] = null;
    if (!isClassificationSlug(normalized)) {
      this.classificationMutationStatus[id] = "error";
      this.classificationMutationError[id] = "Use 1-64 lowercase letters, numbers, hyphens, or underscores; begin with a letter.";
      return false;
    }
    return this.mutateClassification(id, async () => this.client.PUT("/audio-notes/{id}/classification", {
      params: { path: { id } },
      body: { kind: normalized },
    }));
  }
  async clearClassification(id: number): Promise<boolean> {
    if (this.classificationMutationStatus[id] === "saving") return false;
    this.classificationMutationError[id] = null;
    return this.mutateClassification(id, async () => this.client.DELETE("/audio-notes/{id}/classification", { params: { path: { id } } }));
  }
  private async mutateClassification(id: number, command: () => Promise<{ data?: ClassificationResponse; error?: unknown }>): Promise<boolean> {
    this.classificationMutationStatus[id] = "saving";
    try {
      const { data, error } = await command();
      if (error || !data) throw new Error(errorMessage(error ?? "Empty response"));
      runInAction(() => {
        const detail = this.detailCache[id];
        if (detail) {
          detail.classification_kind = data.classification_kind;
          detail.classification_kind_override = data.classification_kind_override;
        }
        const summary = this.list.find((note) => note.id === id);
        if (summary) {
          summary.classification_kind = data.classification_kind;
          summary.classification_kind_override = data.classification_kind_override;
        }
        this.classificationMutationStatus[id] = "idle";
      });
      // Reconciliation advances epochs so older reads cannot restore stale classification state.
      void this.loadDetail(id, true);
      void this.loadClassificationKinds(true);
      void this.loadList();
      return true;
    } catch (error) {
      runInAction(() => {
        this.classificationMutationStatus[id] = "error";
        this.classificationMutationError[id] = errorMessage(error);
      });
      return false;
    }
  }
  async retryTranscription(id: number): Promise<boolean> {
    return this.noteCommand(id, async () => this.client.POST("/audio-notes/{id}/retry", { params: { path: { id } } }));
  }
  async processNote(id: number): Promise<boolean> {
    return this.noteCommand(id, async () => this.client.POST("/audio-notes/{id}/process", { params: { path: { id } } }));
  }
  private async noteCommand(id: number, command: () => Promise<{ error?: unknown }>): Promise<boolean> {
    if (this.processing[id]) return false;
    this.processing[id] = true; this.detailError[id] = null;
    try {
      const { error } = await command();
      if (error) throw new Error(errorMessage(error));
      await this.loadDetail(id, true); void this.loadList(true); return true;
    } catch (error) { runInAction(() => { this.detailError[id] = errorMessage(error); }); return false; }
    finally { runInAction(() => { this.processing[id] = false; }); }
  }
  async deleteNote(id: number): Promise<boolean> {
    this.lastError = null;
    try {
      const { error } = await this.client.DELETE("/audio-notes/{id}", { params: { path: { id } } });
      if (error) throw new Error(errorMessage(error));
      this.deletedIds.add(id);
      runInAction(() => { this.list = this.list.filter((note) => note.id !== id); delete this.detailCache[id]; delete this.detailStatus[id]; });
      await this.loadList(true); return true;
    } catch (error) { runInAction(() => { this.lastError = errorMessage(error); }); return false; }
  }
  async loadRecentTitles(): Promise<void> {
    if (this.recentTitlesStatus === "loading") return;
    this.recentTitlesStatus = "loading"; this.recentTitlesError = null;
    try {
      const { data, error } = await this.client.GET("/audio-notes/recent-titles", { params: { query: { limit: 10 } } });
      if (error || !data) throw new Error(errorMessage(error ?? "Empty response"));
      runInAction(() => { this.recentTitles = data.titles; this.recentTitlesStatus = "loaded"; });
    } catch (error) { runInAction(() => { this.recentTitlesStatus = "error"; this.recentTitlesError = errorMessage(error); }); }
  }
  async updateTitle(id: number, title: string): Promise<boolean> {
    this.titleMutationError[id] = null;
    if (!title.trim()) { this.titleMutationError[id] = "Title cannot be blank."; return false; }
    this.titleMutationStatus[id] = "saving";
    try {
      const { error } = await this.client.PATCH("/audio-notes/{id}/title", { params: { path: { id } }, body: { title: title.trim() } });
      if (error) throw new Error(errorMessage(error));
      await this.loadDetail(id, true); void this.loadList(true); void this.loadRecentTitles();
      runInAction(() => { this.titleMutationStatus[id] = "idle"; }); return true;
    } catch (error) { runInAction(() => { this.titleMutationStatus[id] = "error"; this.titleMutationError[id] = errorMessage(error); }); return false; }
  }
  async regenerateTitle(id: number): Promise<boolean> {
    this.titleMutationStatus[id] = "generating"; this.titleMutationError[id] = null;
    try {
      const { error } = await this.client.POST("/audio-notes/{id}/regenerate-title", { params: { path: { id } } });
      if (error) throw new Error(errorMessage(error));
      // Refresh during the bounded async generation window; manual edits end this wait.
      for (let attempt = 0; attempt < 75 && this.titleMutationStatus[id] === "generating"; attempt++) {
        await new Promise((resolve) => setTimeout(resolve, 2000));
        if (this.deletedIds.has(id)) return false;
        await this.loadDetail(id);
        if (this.detailCache[id]?.title?.trim()) {
          runInAction(() => { this.titleMutationStatus[id] = "idle"; });
          void this.loadList(true); return true;
        }
      }
      if (this.titleMutationStatus[id] !== "generating") return true;
      throw new Error("Title generation is still pending. Refresh to check again.");
    } catch (error) { runInAction(() => { this.titleMutationStatus[id] = "error"; this.titleMutationError[id] = errorMessage(error); }); return false; }
  }

  private async pollStatus(): Promise<void> {
    if (this.polling) return;
    this.polling = true;
    if (this.timer) clearTimeout(this.timer);
    try {
      const { data, error } = await this.client.GET("/audio-notes/status");
      if (error || !data) throw new Error(errorMessage(error ?? "Empty response"));
      const changed = data.phase !== this.phase || data.note_id !== this.noteId;
      runInAction(() => {
        this.active = data.active; this.phase = data.phase; this.noteId = data.note_id ?? null;
        this.title = data.title ?? null; this.durationSeconds = data.duration_seconds ?? 0;
        this.captureDegraded = data.capture_degraded; this.reachable = true; this.firstPollDone = true;
        if (data.last_error) this.lastError = data.last_error;
      });
      if (changed || this.list.some(noteNeedsRefresh)) void this.loadList(true);
    } catch { runInAction(() => { this.reachable = false; this.firstPollDone = true; }); }
    finally {
      this.polling = false;
      if (this.running) this.timer = setTimeout(() => void this.pollStatus(), this.active || this.phase === "review" ? 1000 : 4000);
    }
  }
}
