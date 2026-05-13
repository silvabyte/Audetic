import { makeAutoObservable, runInAction } from "mobx";
import type { RootStore } from "./root-store";
import { daemon } from "@/api/client";
import type { components } from "@/api/schema";

export type MeetingStatus = components["schemas"]["MeetingStatusResponse"];
export type MeetingSummary = components["schemas"]["MeetingSummary"];
export type MeetingDetail = components["schemas"]["MeetingDetailResponse"];

/**
 * Phases mirror crates/audetic/src/meeting/meeting_machine.rs.
 * Kept as a string union so the daemon can evolve without breaking
 * the UI — unknown phases fall through as "unknown".
 */
export type MeetingPhase =
  | "idle"
  | "recording"
  | "compressing"
  | "transcribing"
  | "running_hook"
  | "completed"
  | "error"
  | "cancelled"
  | "unknown";

export type CaptureState = "both" | "mic_only" | "system_only" | "unknown";

const ACTIVE_POLL_MS = 1000;

type ListStatus = "idle" | "loading" | "loaded" | "error";

export class MeetingStore {
  // Live-meeting state (from /meetings/status)
  active = false;
  phase: MeetingPhase = "idle";
  meetingId: number | null = null;
  title: string | null = null;
  durationSeconds: number | null = null;
  captureState: CaptureState | null = null;
  audioPath: string | null = null;
  lastError: string | null = null;

  // List
  list: MeetingSummary[] = [];
  listStatus: ListStatus = "idle";
  listError: string | null = null;

  // Detail cache — keyed by meeting id. One-shot fetch per id.
  detailCache: Record<number, MeetingDetail> = {};
  detailStatus: Record<number, ListStatus> = {};

  /** ID the store wants the UI to auto-navigate to on Completed. */
  pendingNavigationId: number | null = null;

  private pollTimer: ReturnType<typeof setTimeout> | null = null;
  private root: RootStore;

  constructor(root: RootStore) {
    this.root = root;
    makeAutoObservable<this, "root" | "pollTimer">(this, {
      root: false,
      pollTimer: false,
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
   * Re-run transcription against the durable mp3 of a previously failed
   * meeting. Optimistically flips the cached detail to `transcribing` so the
   * UI updates immediately; meeting-detail polls itself while in that state.
   */
  async retryTranscription(id: number): Promise<void> {
    try {
      const { error } = await daemon.POST("/meetings/{id}/retry", {
        params: { path: { id } },
      });
      if (error) throw new Error(formatError(error));
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

  // ---------------------------------------------------------------
  // List + detail fetches
  // ---------------------------------------------------------------

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

  async loadDetail(id: number): Promise<void> {
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

      runInAction(() => {
        this.active = s.active;
        this.phase = nextPhase;
        this.meetingId = s.meeting_id ?? null;
        this.title = s.title ?? null;
        this.durationSeconds = s.duration_seconds ?? null;
        this.audioPath = s.audio_path ?? null;
        this.lastError = s.last_error ?? null;
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
