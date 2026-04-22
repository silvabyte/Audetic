import { makeAutoObservable, runInAction } from "mobx";
import type { RootStore } from "./root-store";
import { daemon } from "@/api/client";
import type { components } from "@/api/schema";

type RecordingStatusResponse = components["schemas"]["RecordingStatusResponse"];
type CompletedJobSummary = components["schemas"]["CompletedJobSummary"];

export type RecordingPhase = "idle" | "recording" | "processing" | "error";

const FAST_POLL_MS = 500;
const SLOW_POLL_MS = 3000;

export class StatusStore {
  phase: RecordingPhase = "idle";
  currentJobId: string | null = null;
  lastCompletedJob: CompletedJobSummary | null = null;
  lastError: string | null = null;

  /** Whether the last `/status` fetch succeeded. */
  reachable = false;
  /** Have we completed at least one poll attempt? Drives daemon-down UI. */
  firstPollDone = false;

  private pollTimer: ReturnType<typeof setTimeout> | null = null;
  private root: RootStore;

  constructor(root: RootStore) {
    this.root = root;
    makeAutoObservable<this, "pollTimer" | "root">(this, {
      pollTimer: false,
      root: false,
    });
  }

  get isBusy(): boolean {
    return this.phase === "recording" || this.phase === "processing";
  }

  start(): void {
    if (this.pollTimer !== null) return;
    void this.pollNow();
  }

  stop(): void {
    if (this.pollTimer !== null) {
      clearTimeout(this.pollTimer);
      this.pollTimer = null;
    }
  }

  /** Toggle recording via POST /toggle and nudge the poll. */
  async toggle(opts?: {
    copy_to_clipboard?: boolean;
    auto_paste?: boolean;
  }): Promise<void> {
    try {
      const { error } = await daemon.POST("/toggle", { body: opts ?? {} });
      if (error) throw new Error(formatError(error));
      this.stop();
      void this.pollNow();
    } catch (e) {
      runInAction(() => {
        this.lastError = e instanceof Error ? e.message : String(e);
      });
    }
  }

  private async pollNow(): Promise<void> {
    try {
      const { data, error } = await daemon.GET("/status");
      if (error || !data) throw new Error(error ? formatError(error) : "empty response");

      // The /status endpoint has a polymorphic response (waybar vs default);
      // here we only care about the default shape.
      const s = data as RecordingStatusResponse;
      runInAction(() => {
        this.phase = normalizePhase(s.phase);
        this.currentJobId = s.job_id ?? null;
        this.lastCompletedJob = s.last_completed_job ?? null;
        this.lastError = s.last_error ?? null;
        this.reachable = true;
        this.firstPollDone = true;
      });
    } catch {
      runInAction(() => {
        this.reachable = false;
        this.firstPollDone = true;
      });
    } finally {
      const next = this.isBusy ? FAST_POLL_MS : SLOW_POLL_MS;
      this.pollTimer = setTimeout(() => {
        void this.pollNow();
      }, next);
    }
  }
}

function normalizePhase(phase: string): RecordingPhase {
  const p = phase.toLowerCase();
  if (p === "recording" || p === "processing" || p === "error") return p;
  return "idle";
}

function formatError(err: unknown): string {
  if (typeof err === "string") return err;
  if (err && typeof err === "object" && "message" in err && typeof (err as { message: unknown }).message === "string") {
    return (err as { message: string }).message;
  }
  try {
    return JSON.stringify(err);
  } catch {
    return String(err);
  }
}
