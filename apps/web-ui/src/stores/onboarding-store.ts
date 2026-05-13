import { makeAutoObservable, runInAction } from "mobx";
import { daemon } from "@/api/client";
import type { components } from "@/api/schema";
import type { RootStore } from "./root-store";

type DepsState = "unknown" | "checking" | "ready" | "needs-ffmpeg" | "error";
type InstallPhase = components["schemas"]["InstallPhase"];

const POLL_INTERVAL_MS = 500;
const POLL_TIMEOUT_MS = 5 * 60 * 1000;

/**
 * Drives the SPA onboarding overlay: check system deps, prompt the user
 * to install missing ones, poll until install finishes, re-check, clear.
 *
 * Daemon binary install is handled by `audetic install` before the user
 * ever loads the SPA, so the only first-run gate left here is ffmpeg.
 */
export class OnboardingStore {
  state: DepsState = "unknown";
  installPhase: InstallPhase | "idle" = "idle";
  installPercent: number | null = null;
  installMessage: string | null = null;
  installError: string | null = null;

  private root: RootStore;

  constructor(root: RootStore) {
    this.root = root;
    makeAutoObservable<this, "root">(this, { root: false });
  }

  get blocking(): boolean {
    return this.state === "needs-ffmpeg" || this.installPhase === "downloading"
      || this.installPhase === "extracting" || this.installPhase === "starting";
  }

  async check(): Promise<void> {
    runInAction(() => {
      if (this.state === "unknown") this.state = "checking";
    });
    try {
      const { data, error } = await daemon.GET("/system/deps");
      if (error || !data) {
        runInAction(() => {
          this.state = "error";
        });
        return;
      }
      runInAction(() => {
        this.state = data.ffmpeg ? "ready" : "needs-ffmpeg";
      });
    } catch {
      runInAction(() => {
        this.state = "error";
      });
    }
  }

  async installFfmpeg(): Promise<void> {
    runInAction(() => {
      this.installPhase = "starting";
      this.installError = null;
      this.installMessage = "Requesting FFmpeg install";
      this.installPercent = null;
    });

    try {
      const { data, error, response } = await daemon.POST("/system/install-ffmpeg", {});
      // 202 (started) and 409 (already running) both ship a status body — both fine.
      if (error && response.status !== 409) {
        throw new Error(formatError(error));
      }
      if (data) {
        this.applyStatus(data);
        if (data.phase === "done") {
          await this.check();
          return;
        }
        if (data.phase === "error") {
          throw new Error(data.message ?? "FFmpeg install failed");
        }
      }

      const deadline = Date.now() + POLL_TIMEOUT_MS;
      while (Date.now() < deadline) {
        await sleep(POLL_INTERVAL_MS);
        const r = await daemon.GET("/system/install-ffmpeg/status");
        if (r.error || !r.data) continue;
        this.applyStatus(r.data);
        if (r.data.phase === "done") {
          await this.check();
          return;
        }
        if (r.data.phase === "error") {
          throw new Error(r.data.message ?? "FFmpeg install failed");
        }
      }
      throw new Error("FFmpeg install timed out after 5 minutes");
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      runInAction(() => {
        this.installPhase = "error";
        this.installError = msg;
      });
    }
  }

  private applyStatus(status: components["schemas"]["InstallStatusResponse"]): void {
    runInAction(() => {
      this.installPhase = status.phase;
      this.installMessage = status.message ?? null;
      if (typeof status.percent === "number") {
        this.installPercent = status.percent;
      } else if (
        typeof status.downloadedBytes === "number" &&
        typeof status.totalBytes === "number" &&
        status.totalBytes > 0
      ) {
        this.installPercent = Math.round(
          (status.downloadedBytes / status.totalBytes) * 100,
        );
      }
    });
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function formatError(err: unknown): string {
  if (typeof err === "string") return err;
  if (err && typeof err === "object" && "message" in err) {
    return String((err as { message: unknown }).message);
  }
  try {
    return JSON.stringify(err);
  } catch {
    return String(err);
  }
}
