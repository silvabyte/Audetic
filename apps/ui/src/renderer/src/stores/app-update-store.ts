import { makeAutoObservable, runInAction } from "mobx";
import { toast } from "sonner";
import type { RootStore } from "./root-store";

// Mirrors AppUpdateEvent in src/preload/index.ts. Defined here so the
// renderer doesn't need a dependency on the preload module's runtime.
export type AppUpdateEvent =
  | { kind: "checking" }
  | { kind: "available"; version: string; releaseName?: string }
  | { kind: "not-available"; currentVersion: string }
  | { kind: "progress"; percent: number; bytesPerSecond: number }
  | { kind: "downloaded"; version: string }
  | { kind: "error"; message: string };

type Phase =
  | "idle"
  | "checking"
  | "available"
  | "downloading"
  | "downloaded"
  | "not-available"
  | "error";

/**
 * AppUpdateStore mirrors the electron-updater event stream from main into
 * observable state for the Settings → Updates page and surfaces a toast
 * when a downloaded update is ready to install.
 *
 * Distinct from ConfigStore (which manages the daemon's `/update/*`
 * endpoints — relevant to curl-bash users who didn't bootstrap from the
 * bundled binary). For bundled-install users, this is the canonical
 * update path and ConfigStore's update card is hidden.
 */
export class AppUpdateStore {
  phase: Phase = "idle";
  currentVersion: string | null = null;
  latestVersion: string | null = null;
  /** 0–100, only meaningful when phase === "downloading". */
  progressPercent = 0;
  bytesPerSecond = 0;
  error: string | null = null;
  /** Set after an `update-downloaded` event. Drives the "Restart and update" button. */
  ready = false;

  private root: RootStore;
  private unsubscribe: (() => void) | null = null;

  constructor(root: RootStore) {
    this.root = root;
    makeAutoObservable<this, "root" | "unsubscribe">(this, {
      root: false,
      unsubscribe: false,
    });
  }

  start(): void {
    const bridge = window.audetic;
    if (!bridge?.autoUpdate) return;

    void bridge.autoUpdate.currentVersion().then((v) => {
      runInAction(() => {
        this.currentVersion = v;
      });
    });

    this.unsubscribe = bridge.autoUpdate.onEvent((event) => {
      this.handleEvent(event);
    });
  }

  stop(): void {
    if (this.unsubscribe) {
      this.unsubscribe();
      this.unsubscribe = null;
    }
  }

  async check(): Promise<void> {
    const bridge = window.audetic;
    if (!bridge?.autoUpdate) return;
    runInAction(() => {
      this.error = null;
    });
    const r = await bridge.autoUpdate.check();
    if (!r.ok) {
      runInAction(() => {
        this.phase = "error";
        this.error = r.reason;
      });
    }
  }

  async install(): Promise<void> {
    const bridge = window.audetic;
    if (!bridge?.autoUpdate) return;
    const r = await bridge.autoUpdate.install();
    if (!r.ok) {
      runInAction(() => {
        this.error = r.reason;
      });
      toast.error("Couldn't install update", { description: r.reason });
    }
    // On success the app quits + relaunches; no further state to track.
  }

  private handleEvent(event: AppUpdateEvent): void {
    runInAction(() => {
      switch (event.kind) {
        case "checking":
          this.phase = "checking";
          this.error = null;
          break;
        case "available":
          this.phase = "downloading";
          this.latestVersion = event.version;
          this.progressPercent = 0;
          break;
        case "not-available":
          this.phase = "not-available";
          this.currentVersion = event.currentVersion;
          break;
        case "progress":
          this.phase = "downloading";
          this.progressPercent = Math.round(event.percent);
          this.bytesPerSecond = event.bytesPerSecond;
          break;
        case "downloaded":
          this.phase = "downloaded";
          this.latestVersion = event.version;
          this.ready = true;
          break;
        case "error":
          this.phase = "error";
          this.error = event.message;
          break;
      }
    });

    if (event.kind === "downloaded") {
      // Surface the ready-to-install state through a toast even if the
      // user isn't currently on Settings → Updates. Action button calls
      // install() which triggers quitAndInstall.
      toast.success(`Audetic ${event.version} ready to install`, {
        description: "The app will restart with the new version.",
        duration: Infinity,
        action: {
          label: "Restart and update",
          onClick: () => {
            void this.install();
          },
        },
      });
    }
  }
}
