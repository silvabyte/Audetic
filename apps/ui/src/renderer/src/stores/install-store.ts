import { makeAutoObservable, runInAction } from "mobx";
import type { RootStore } from "./root-store";

// Mirrors the types exported by src/preload/index.ts. We don't import them
// directly because the preload module pulls in Electron's node APIs, which
// the renderer's tsconfig (correctly) doesn't allow. Window.audetic is
// already typed via src/preload/index.d.ts.
export interface OnboardingState {
  platform: NodeJS.Platform;
  bundledVersion: string | null;
  installedVersion: string | null;
  runningVersion: string | null;
  daemonReachable: boolean;
  unitInstalled: boolean;
  unitEnabled: boolean;
  unitActive: boolean;
}

export interface OnboardingProgress {
  step: string;
  detail?: string;
}

type FetchStatus = "idle" | "loading" | "loaded" | "error";
type InstallStatus = "idle" | "running" | "done" | "error";

/**
 * Onboarding "what should the UI show right now" derivation. Maps directly
 * onto the rows of the onboarding state-machine table in the project plan.
 */
export type OnboardingDecision =
  | { kind: "happy" }
  | { kind: "install" }
  | { kind: "enable" }
  | { kind: "start" }
  | { kind: "update"; bundled: string; installed: string | null }
  | { kind: "unknown" };

const DEFAULT_STATE: OnboardingState = {
  platform: (typeof process !== "undefined" ? (process.platform as NodeJS.Platform) : "linux"),
  bundledVersion: null,
  installedVersion: null,
  runningVersion: null,
  daemonReachable: false,
  unitInstalled: false,
  unitEnabled: false,
  unitActive: false,
};

/**
 * InstallStore polls the main-process onboarding IPC and exposes a
 * derived `decision` the OnboardingCard switches on. Mutations
 * (install / enable / update) re-detect on completion so the card
 * collapses to "happy" without needing a manual refresh.
 *
 * Lives outside ConfigStore because Settings → Updates and the
 * onboarding card both consume it; ConfigStore is route-local.
 */
export class InstallStore {
  state: OnboardingState = DEFAULT_STATE;
  detectStatus: FetchStatus = "idle";
  detectError: string | null = null;

  installStatus: InstallStatus = "idle";
  installError: string | null = null;
  /** Most recent progress event from the main-process flow. */
  progress: OnboardingProgress | null = null;

  private root: RootStore;
  private unsubscribeProgress: (() => void) | null = null;

  constructor(root: RootStore) {
    this.root = root;
    makeAutoObservable<this, "root" | "unsubscribeProgress">(this, {
      root: false,
      unsubscribeProgress: false,
    });
  }

  /**
   * Wire up the progress event listener and run an initial detect.
   * Called once from RootStore.start().
   */
  start(): void {
    const bridge = window.audetic;
    if (!bridge?.onboarding) {
      // Renderer not running inside Electron (or preload failed). Stay
      // idle; the detect call would just throw.
      return;
    }
    this.unsubscribeProgress = bridge.onboarding.onProgress((p) => {
      runInAction(() => {
        this.progress = p;
      });
    });
    void this.detect();
  }

  stop(): void {
    if (this.unsubscribeProgress) {
      this.unsubscribeProgress();
      this.unsubscribeProgress = null;
    }
  }

  get decision(): OnboardingDecision {
    const s = this.state;
    if (!this.detectStatus || this.detectStatus === "idle") return { kind: "unknown" };

    if (s.daemonReachable) {
      // Happy path — but offer an in-place daemon refresh if the bundle
      // is newer than what's installed (and the user has a bundle to
      // install from at all).
      if (
        s.bundledVersion &&
        s.installedVersion &&
        s.bundledVersion !== s.installedVersion
      ) {
        return {
          kind: "update",
          bundled: s.bundledVersion,
          installed: s.installedVersion,
        };
      }
      return { kind: "happy" };
    }

    // Daemon unreachable — figure out why.
    if (!s.unitInstalled) return { kind: "install" };
    if (!s.unitEnabled) return { kind: "enable" };
    return { kind: "start" };
  }

  async detect(): Promise<void> {
    const bridge = window.audetic;
    if (!bridge?.onboarding) return;
    runInAction(() => {
      this.detectStatus = "loading";
      this.detectError = null;
    });
    try {
      const next = await bridge.onboarding.detect();
      runInAction(() => {
        this.state = next;
        this.detectStatus = "loaded";
      });
    } catch (e) {
      runInAction(() => {
        this.detectError = e instanceof Error ? e.message : String(e);
        this.detectStatus = "error";
      });
    }
  }

  async install(): Promise<void> {
    await this.runFlow("install");
  }

  async enable(): Promise<void> {
    await this.runFlow("enable");
  }

  async update(): Promise<void> {
    await this.runFlow("update");
  }

  private async runFlow(
    op: "install" | "enable" | "update",
  ): Promise<void> {
    const bridge = window.audetic;
    if (!bridge?.onboarding) return;
    runInAction(() => {
      this.installStatus = "running";
      this.installError = null;
      this.progress = null;
    });
    try {
      const result = await bridge.onboarding[op]();
      if (result.ok) {
        runInAction(() => {
          this.installStatus = "done";
        });
      } else {
        runInAction(() => {
          this.installStatus = "error";
          this.installError = result.error;
        });
      }
    } catch (e) {
      runInAction(() => {
        this.installStatus = "error";
        this.installError = e instanceof Error ? e.message : String(e);
      });
    }

    // Always re-detect so the UI moves to the next card / collapses to
    // happy. detect() itself manages its own observable state so we
    // don't await it inside runInAction.
    await this.detect();
  }

  clearError(): void {
    this.installError = null;
  }
}
