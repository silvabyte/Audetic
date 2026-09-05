import { makeAutoObservable, runInAction } from "mobx";
import type { RootStore } from "./root-store";
import { daemon } from "@/api/client";
import type { components } from "@/api/schema";

export type MeetingStatus = components["schemas"]["MeetingStatusResponse"];
export type MeetingSummary = components["schemas"]["MeetingSummary"];
export type MeetingDetail = components["schemas"]["MeetingDetailResponse"];
export type TitleMutationStatus = "idle" | "saving" | "generating" | "error";

/**
 * Phases mirror crates/audetic/src/meeting/meeting_machine.rs.
 * Kept as a string union so the daemon can evolve without breaking
 * the UI — unknown phases fall through as "unknown".
 */
export type MeetingPhase =
  | "idle"
  | "recording"
  | "review"
  | "compressing"
  | "transcribing"
  | "running_hook"
  | "completed"
  | "error"
  | "cancelled"
  | "unknown";

export type CaptureState = "both" | "mic_only" | "system_only" | "unknown";

/**
 * Whether a meeting in this phase is settled and therefore safe to delete.
 * In-flight phases (recording, review, compressing, transcribing,
 * running_hook) are still owned by the daemon's meeting machine — the backend
 * rejects deleting them with 409, so we also hide the control for them.
 * Mirrors `MeetingPhase::is_terminal` in
 * crates/audetic/src/meeting/status.rs.
 */
export function isDeletableMeetingStatus(status: string): boolean {
  const s = status.toLowerCase();
  return s === "completed" || s === "error" || s === "cancelled";
}

const ACTIVE_POLL_MS = 1000;

type ListStatus = "idle" | "loading" | "loaded" | "error";

export class MeetingStore {
  // Live-meeting state (from /meetings/status)
  active = false;
  phase: MeetingPhase = "idle";
  meetingId: string | null = null;
  title: string | null = null;
  durationSeconds: number | null = null;
  captureState: CaptureState | null = null;
  meetingStartedAt: string | null = null;
  lastError: string | null = null;

  // List
  list: MeetingSummary[] = [];
  listStatus: ListStatus = "idle";
  listError: string | null = null;

  // Detail cache — keyed by meeting id. One-shot fetch per id.
  detailCache: Record<string, MeetingDetail> = {};
  detailStatus: Record<string, ListStatus> = {};

  // Shared title picker + per-detail mutation feedback.
  recentTitles: string[] = [];
  recentTitlesStatus: ListStatus = "idle";
  recentTitlesError: string | null = null;
  titleMutationStatus: Record<string, TitleMutationStatus> = {};
  titleMutationError: Record<string, string | null> = {};

  /** ID the store wants the UI to auto-navigate to on Completed. */
  pendingNavigationId: string | null = null;

  private pollTimer: ReturnType<typeof setTimeout> | null = null;
  private silentTitlePolls = new Set<string>();
  private titleMutationEpoch = new Map<string, number>();
  private root: RootStore;

  constructor(root: RootStore) {
    this.root = root;
    makeAutoObservable<
      this,
      "root" | "pollTimer" | "silentTitlePolls" | "titleMutationEpoch"
    >(this, {
      root: false,
      pollTimer: false,
      silentTitlePolls: false,
      titleMutationEpoch: false,
    });
  }

  /** Called by RootStore.start(). Fetches once to bootstrap state. */
  start(): void {
    void this.pollStatus();
  }

  stop(): void {
    if (this.pollTimer !== null) {
      clearTimeout(this.pollTimer);
      this.pollTimer = null;
    }
  }

  // ---------------------------------------------------------------
  // Mutations
  // ---------------------------------------------------------------

  async startMeeting(title?: string): Promise<void> {
    try {
      const { data, error } = await daemon.POST("/meetings/start", {
        body: title ? { title } : {},
      });
      if (error) throw new Error(formatError(error));
      // /meetings/status doesn't return capture_state; the start
      // response does. Stash it so the banner can render it.
      if (data) {
        runInAction(() => {
          this.captureState = normalizeCaptureState(data.capture_state);
          this.meetingStartedAt = new Date().toISOString();
          if (title) this.rememberRecentTitle(title);
        });
      }
    } catch (e) {
      runInAction(() => {
        this.lastError = e instanceof Error ? e.message : String(e);
      });
    } finally {
      this.schedulePoll(0);
    }
  }

  async stopMeeting(): Promise<void> {
    try {
      const { error } = await daemon.POST("/meetings/stop", {});
      if (error) throw new Error(formatError(error));
    } catch (e) {
      runInAction(() => {
        this.lastError = e instanceof Error ? e.message : String(e);
      });
    } finally {
      this.schedulePoll(0);
    }
  }

  /**
   * Confirm the recording awaiting review and send it for transcription,
   * optionally trimming to `[startSeconds, endSeconds)`. Either bound omitted
   * keeps that edge. Seconds are sent as floats so the daemon can trim the
   * lossless WAV sample-accurately.
   */
  async confirmMeeting(
    startSeconds?: number,
    endSeconds?: number,
  ): Promise<void> {
    try {
      const body: { start_seconds?: number; end_seconds?: number } = {};
      if (typeof startSeconds === "number") body.start_seconds = startSeconds;
      if (typeof endSeconds === "number") body.end_seconds = endSeconds;
      const { error } = await daemon.POST("/meetings/confirm", { body });
      if (error) throw new Error(formatError(error));
    } catch (e) {
      runInAction(() => {
        this.lastError = e instanceof Error ? e.message : String(e);
      });
    } finally {
      this.schedulePoll(0);
    }
  }

  async cancelMeeting(): Promise<void> {
    try {
      const { error } = await daemon.POST("/meetings/cancel", {});
      if (error) throw new Error(formatError(error));
    } catch (e) {
      runInAction(() => {
        this.lastError = e instanceof Error ? e.message : String(e);
      });
    } finally {
      this.schedulePoll(0);
    }
  }

  /** Consumer calls this after handling auto-nav so we don't loop. */
  clearPendingNavigation(): void {
    this.pendingNavigationId = null;
  }

  /**
   * Upload an existing media file as a new meeting. Returns the new
   * meeting id on success. POSTs through the same typed `daemon` client
   * the rest of this store uses; the daemon streams the file to disk
   * and kicks off the processing pipeline, so the response comes back
   * as soon as the upload finishes — not when the transcription does.
   * Refreshes the list so the new row shows up in "compressing" state
   * and the detail page's auto-refresh takes over.
   *
   * Reports failures via `lastError` (same surface other mutations
   * use) so route actions can diff and toast.
   */
  async importFile(
    file: File,
    title?: string,
  ): Promise<{ meetingId: string } | null> {
    const form = new FormData();
    form.append("file", file);
    const trimmed = title?.trim();
    if (trimmed) {
      form.append("title", trimmed);
    }

    try {
      // openapi-fetch defaults `bodySerializer` to JSON.stringify, which
      // would corrupt the FormData boundary. Pass-through serializer +
      // empty `Content-Type` lets the browser set the multipart header
      // (with its random boundary token) for us. The schema types the
      // body as `unknown`, hence the cast.
      const { data, error } = await daemon.POST("/meetings/import", {
        body: form as never,
        bodySerializer: (body) => body as BodyInit,
        headers: { "Content-Type": null },
      });
      if (error || !data) {
        throw new Error(formatError(error ?? "empty response"));
      }
      // Best-effort list refresh so the row appears immediately.
      void this.loadList();
      this.watchForGeneratedTitle(data.meeting_id);
      return { meetingId: data.meeting_id };
    } catch (e) {
      runInAction(() => {
        this.lastError = e instanceof Error ? e.message : String(e);
      });
      return null;
    }
  }

  /**
   * Re-run transcription against the durable mp3 of a previously failed
   * meeting. Optimistically flips the cached detail to `transcribing` so the
   * UI updates immediately; meeting-detail polls itself while in that state.
   */
  async retryTranscription(id: string): Promise<void> {
    try {
      const { error } = await daemon.POST("/meetings/{id}/retry", {
        params: { path: { id } },
      });
      if (error) throw new Error(formatError(error));
      this.bumpTitleMutationEpoch(id);
      runInAction(() => {
        const cached = this.detailCache[id];
        if (cached) {
          this.detailCache[id] = {
            ...cached,
            status: "transcribing",
            error: null,
          };
        }
      });
    } catch (e) {
      runInAction(() => {
        // Surface on the detail row so meeting-detail.tsx renders it inline.
        const cached = this.detailCache[id];
        if (cached) {
          this.detailCache[id] = {
            ...cached,
            error: e instanceof Error ? e.message : String(e),
          };
        }
      });
    }
  }

  /**
   * Delete a meeting. The label is "Delete" but the daemon soft-deletes it:
   * the row is hidden everywhere and the audio stays on disk. On success we
   * drop it from the in-memory list and detail caches so the UI updates
   * without a refetch. Returns whether it succeeded so callers can navigate
   * and toast. Failures land on `lastError`.
   */
  async deleteMeeting(id: string): Promise<boolean> {
    try {
      const { error } = await daemon.DELETE("/meetings/{id}", {
        params: { path: { id } },
      });
      if (error) throw new Error(formatError(error));
      this.bumpTitleMutationEpoch(id);
      runInAction(() => {
        this.list = this.list.filter((m) => m.id !== id);
        delete this.detailCache[id];
        delete this.detailStatus[id];
        delete this.titleMutationStatus[id];
        delete this.titleMutationError[id];
      });
      return true;
    } catch (e) {
      runInAction(() => {
        this.lastError = e instanceof Error ? e.message : String(e);
      });
      return false;
    }
  }

  // ---------------------------------------------------------------
  // List + detail fetches
  // ---------------------------------------------------------------

  async loadRecentTitles(limit = 10): Promise<void> {
    if (this.recentTitlesStatus === "loading") return;
    runInAction(() => {
      this.recentTitlesStatus = "loading";
      this.recentTitlesError = null;
    });
    try {
      const { data, error } = await daemon.GET("/meetings/recent-titles", {
        params: { query: { limit } },
      });
      if (error || !data) throw new Error(formatError(error ?? "empty response"));
      runInAction(() => {
        this.recentTitles = data.titles;
        this.recentTitlesStatus = "loaded";
      });
    } catch (e) {
      runInAction(() => {
        this.recentTitlesError = e instanceof Error ? e.message : String(e);
        this.recentTitlesStatus = "error";
      });
    }
  }

  async updateTitle(id: string, title: string): Promise<boolean> {
    const trimmed = title.trim();
    if (!trimmed) {
      runInAction(() => {
        this.titleMutationStatus[id] = "error";
        this.titleMutationError[id] = "Title cannot be blank.";
      });
      return false;
    }

    this.bumpTitleMutationEpoch(id);
    runInAction(() => {
      this.titleMutationStatus[id] = "saving";
      this.titleMutationError[id] = null;
    });
    try {
      const { data, error } = await daemon.PATCH("/meetings/{id}/title", {
        params: { path: { id } },
        body: { title: trimmed },
      });
      if (error || !data) throw new Error(formatError(error ?? "empty response"));
      runInAction(() => {
        this.applyTitle(id, data.title ?? trimmed, data.title_source ?? "manual");
        this.rememberRecentTitle(data.title ?? trimmed);
        this.titleMutationStatus[id] = "idle";
      });
      return true;
    } catch (e) {
      runInAction(() => {
        this.titleMutationStatus[id] = "error";
        this.titleMutationError[id] = e instanceof Error ? e.message : String(e);
      });
      return false;
    }
  }

  async regenerateTitle(id: string): Promise<boolean> {
    const mutationEpoch = this.bumpTitleMutationEpoch(id);
    runInAction(() => {
      this.titleMutationStatus[id] = "generating";
      this.titleMutationError[id] = null;
    });
    try {
      const { error } = await daemon.POST("/meetings/{id}/regenerate-title", {
        params: { path: { id } },
      });
      if (error) throw new Error(formatError(error));
      runInAction(() => {
        this.applyTitle(id, null, null);
      });
      void this.pollForGeneratedTitle(id, true, mutationEpoch);
      return true;
    } catch (e) {
      runInAction(() => {
        this.titleMutationStatus[id] = "error";
        this.titleMutationError[id] = e instanceof Error ? e.message : String(e);
      });
      return false;
    }
  }

  clearTitleMutationFeedback(id: string): void {
    if (this.titleMutationStatus[id] === "error") {
      this.titleMutationStatus[id] = "idle";
      this.titleMutationError[id] = null;
    }
  }

  watchForGeneratedTitle(id: string): void {
    if (this.silentTitlePolls.has(id)) return;
    this.silentTitlePolls.add(id);
    const mutationEpoch = this.currentTitleMutationEpoch(id);
    void this.pollForGeneratedTitle(id, false, mutationEpoch).finally(() => {
      this.silentTitlePolls.delete(id);
    });
  }

  async loadList(limit = 50): Promise<void> {
    if (this.listStatus === "loading") return;
    runInAction(() => {
      this.listStatus = "loading";
      this.listError = null;
    });
    try {
      const { data, error } = await daemon.GET("/meetings", {
        params: { query: { limit } },
      });
      if (error || !data) throw new Error(formatError(error ?? "empty response"));
      runInAction(() => {
        this.list = data.meetings ?? [];
        this.listStatus = "loaded";
      });
    } catch (e) {
      runInAction(() => {
        this.listError = e instanceof Error ? e.message : String(e);
        this.listStatus = "error";
      });
    }
  }

  async loadDetail(id: string): Promise<void> {
    if (this.detailStatus[id] === "loading") return;
    runInAction(() => {
      this.detailStatus[id] = "loading";
    });
    try {
      const { data, error } = await daemon.GET("/meetings/{id}", {
        params: { path: { id } },
      });
      if (error || !data) throw new Error(formatError(error ?? "empty response"));
      runInAction(() => {
        this.detailCache[id] = data;
        this.detailStatus[id] = "loaded";
      });
    } catch {
      runInAction(() => {
        this.detailStatus[id] = "error";
      });
    }
  }

  // ---------------------------------------------------------------
  // Polling
  // ---------------------------------------------------------------

  private async pollStatus(): Promise<void> {
    try {
      const { data, error } = await daemon.GET("/meetings/status");
      if (error || !data) throw new Error(formatError(error ?? "empty response"));
      const s = data as MeetingStatus;

      const prevPhase = this.phase;
      const nextPhase = normalizePhase(s.phase);
      const previousMeetingId = this.meetingId;

      runInAction(() => {
        this.active = s.active;
        this.phase = nextPhase;
        this.meetingId = s.meeting_id ?? null;
        this.title = s.title ?? null;
        this.durationSeconds = s.duration_seconds ?? null;
        this.lastError = s.last_error ?? null;
        if (
          s.meeting_id !== null &&
          s.meeting_id !== undefined &&
          s.meeting_id !== previousMeetingId
        ) {
          this.meetingStartedAt = new Date().toISOString();
        }
      });

      // Transition into completed → tell the UI to jump to detail and
      // refresh the list so the new entry is visible.
      if (
        prevPhase !== "completed" &&
        nextPhase === "completed" &&
        this.meetingId !== null
      ) {
        runInAction(() => {
          this.pendingNavigationId = this.meetingId;
        });
        void this.loadList();
      }

      // Pipeline state fell through to idle or a terminal state — the
      // list may have changed (title saved, meeting recorded, etc.).
      if (
        (prevPhase === "recording" ||
          prevPhase === "review" ||
          prevPhase === "compressing" ||
          prevPhase === "transcribing" ||
          prevPhase === "running_hook") &&
        (nextPhase === "idle" ||
          nextPhase === "cancelled" ||
          nextPhase === "error")
      ) {
        void this.loadList();
      }
    } catch {
      // Leave last-known state in place; renderer picks up daemon-down
      // through StatusStore's reachability signal.
    } finally {
      this.schedulePoll(nextPollDelay(this.phase, this.active));
    }
  }

  private schedulePoll(delayMs: number): void {
    if (this.pollTimer !== null) clearTimeout(this.pollTimer);
    this.pollTimer = setTimeout(() => {
      void this.pollStatus();
    }, Math.max(0, delayMs));
  }

  private async pollForGeneratedTitle(
    id: string,
    reportTimeout: boolean,
    mutationEpoch: number,
  ): Promise<void> {
    // Explicit regeneration starts from a completed meeting and gets a short,
    // visible timeout. Silent import polling waits through transcription, then
    // allows the same generation window once the meeting reaches completion.
    const maxAttempts = reportTimeout ? 75 : 900;
    let completedAttempts = 0;
    for (let attempt = 0; attempt < maxAttempts; attempt += 1) {
      await delay(2000);
      if (mutationEpoch !== this.currentTitleMutationEpoch(id)) return;
      if (reportTimeout && this.titleMutationStatus[id] !== "generating") {
        return;
      }

      try {
        const { data, error } = await daemon.GET("/meetings/{id}", {
          params: { path: { id } },
        });
        if (error || !data) continue;
        if (mutationEpoch !== this.currentTitleMutationEpoch(id)) return;
        if (data.title?.trim()) {
          runInAction(() => {
            this.detailCache[id] = data;
            this.detailStatus[id] = "loaded";
            this.applyTitle(id, data.title ?? null, data.title_source ?? null);
            if (this.titleMutationStatus[id] === "generating") {
              this.titleMutationStatus[id] = "idle";
            }
          });
          return;
        }
        if (data.status === "error" || data.status === "cancelled") return;
        if (data.status === "completed") {
          completedAttempts += 1;
          if (completedAttempts >= 75) break;
        }
        runInAction(() => {
          this.detailCache[id] = data;
          this.detailStatus[id] = "loaded";
        });
      } catch {
        // A transient detail failure should not stop the generation poll.
      }
    }

    if (mutationEpoch !== this.currentTitleMutationEpoch(id)) return;
    if (reportTimeout) {
      runInAction(() => {
        this.titleMutationStatus[id] = "error";
        this.titleMutationError[id] =
          "Title generation is still pending. Refresh to check again.";
      });
    }
  }

  private applyTitle(
    id: string,
    title: string | null,
    titleSource: components["schemas"]["MeetingTitleSource"] | null,
  ): void {
    const detail = this.detailCache[id];
    if (detail) {
      this.detailCache[id] = {
        ...detail,
        title,
        title_source: titleSource,
      };
    }
    this.list = this.list.map((meeting) =>
      meeting.id === id
        ? { ...meeting, title, title_source: titleSource }
        : meeting,
    );
    if (this.meetingId === id) this.title = title;
  }

  private rememberRecentTitle(title: string): void {
    const trimmed = title.trim();
    if (!trimmed) return;
    this.recentTitles = [
      trimmed,
      ...this.recentTitles.filter((recent) => recent !== trimmed),
    ].slice(0, 10);
    this.recentTitlesStatus = "loaded";
  }

  private bumpTitleMutationEpoch(id: string): number {
    const next = this.currentTitleMutationEpoch(id) + 1;
    this.titleMutationEpoch.set(id, next);
    return next;
  }

  private currentTitleMutationEpoch(id: string): number {
    return this.titleMutationEpoch.get(id) ?? 0;
  }
}

function delay(milliseconds: number): Promise<void> {
  return new Promise((resolve) => window.setTimeout(resolve, milliseconds));
}

function nextPollDelay(phase: MeetingPhase, active: boolean): number {
  // Active meeting, or post-stop pipeline in progress → poll fast.
  if (
    active ||
    phase === "compressing" ||
    phase === "transcribing" ||
    phase === "running_hook"
  ) {
    return ACTIVE_POLL_MS;
  }
  // Completed / idle / error / cancelled → slower heartbeat so we pick
  // up externally-triggered meetings (CLI, hotkey) without flooding.
  return 5000;
}

function normalizeCaptureState(raw: string): CaptureState {
  const v = raw.toLowerCase();
  if (v === "both" || v === "mic_only" || v === "system_only") return v;
  return "unknown";
}

function normalizePhase(raw: string): MeetingPhase {
  const v = raw.toLowerCase();
  const known: MeetingPhase[] = [
    "idle",
    "recording",
    "review",
    "compressing",
    "transcribing",
    "running_hook",
    "completed",
    "error",
    "cancelled",
  ];
  return (known as string[]).includes(v) ? (v as MeetingPhase) : "unknown";
}

function formatError(err: unknown): string {
  if (typeof err === "string") return err;
  if (
    err &&
    typeof err === "object" &&
    "message" in err &&
    typeof (err as { message: unknown }).message === "string"
  ) {
    return (err as { message: string }).message;
  }
  try {
    return JSON.stringify(err);
  } catch {
    return String(err);
  }
}
