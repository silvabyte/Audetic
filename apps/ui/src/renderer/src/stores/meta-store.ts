import { makeAutoObservable, runInAction } from "mobx";
import type { RootStore } from "./root-store";
import { daemon } from "@/api/client";

/**
 * Small, one-shot daemon metadata that drives the Dashboard footer and
 * anywhere else we show "which provider are we using" or "what version".
 *
 * Loaded lazily via `prefetch()` called from route loaders. Will likely
 * fold into ConfigStore when Phase 4 lands; keeping it standalone for
 * Phase 1 so we don't grow ConfigStore prematurely.
 */
type FetchStatus = "idle" | "loading" | "loaded" | "error";

export class MetaStore {
  version: string | null = null;
  providerName: string | null = null;

  private status: FetchStatus = "idle";
  private root: RootStore;

  constructor(root: RootStore) {
    this.root = root;
    makeAutoObservable<this, "root" | "status">(this, {
      root: false,
      status: false,
    });
  }

  /**
   * Idempotent initial load. Does nothing if already loaded or in
   * flight. On failure we stay in `error` state and the next call
   * (e.g. when the user navigates back to the Dashboard) retries.
   */
  async prefetch(): Promise<void> {
    if (this.status === "loading" || this.status === "loaded") return;
    this.status = "loading";
    try {
      const [v, p] = await Promise.all([
        daemon.GET("/version"),
        daemon.GET("/provider"),
      ]);
      if (v.error || p.error || !v.data || !p.data) {
        this.status = "error";
        return;
      }
      runInAction(() => {
        this.version = (v.data as { version: string }).version;
        this.providerName =
          (p.data as { provider?: string | null }).provider ?? null;
      });
      this.status = "loaded";
    } catch {
      this.status = "error";
    }
  }
}
