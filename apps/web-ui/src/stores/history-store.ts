import { makeAutoObservable, runInAction } from "mobx";
import type { RootStore } from "./root-store";
import { daemon } from "@/api/client";
import type { components } from "@/api/schema";

export type HistoryEntry = components["schemas"]["HistoryEntry"];

export interface HistoryQuery {
  q?: string;
  from?: string;
  to?: string;
  limit?: number;
}

type Status = "idle" | "loading" | "loaded" | "error";

/**
 * HistoryStore mirrors GET /api/history. The route is the source of truth
 * for the *query* (via URL searchParams); this store holds fetched
 * entries + loading/error state.
 *
 * load(params) is idempotent per-params and safe to call from the
 * loader on every navigation — it only re-fetches when params change
 * (or the caller explicitly invalidates).
 */
export class HistoryStore {
  entries: HistoryEntry[] = [];
  loadingState: Status = "idle";
  error: string | null = null;

  private lastQueryKey: string | null = null;
  private root: RootStore;

  constructor(root: RootStore) {
    this.root = root;
    makeAutoObservable<this, "root" | "lastQueryKey">(this, {
      root: false,
      lastQueryKey: false,
    });
  }

  get isLoading(): boolean {
    return this.loadingState === "loading";
  }

  /**
   * Fetch entries matching `params`. No-op if a completed fetch for the
   * same key is already in cache. Called from the /history loader.
   */
  async load(params: HistoryQuery = {}): Promise<void> {
    const key = queryKey(params);
    if (this.loadingState === "loaded" && this.lastQueryKey === key) return;
    if (this.loadingState === "loading" && this.lastQueryKey === key) return;

    this.lastQueryKey = key;
    runInAction(() => {
      this.loadingState = "loading";
      this.error = null;
    });

    // Only include populated keys — openapi-fetch serializes explicit
    // `undefined` as empty strings, which the daemon treats as
    // "match empty" and returns zero results.
    const query: Record<string, string | number> = { limit: params.limit ?? 50 };
    if (params.q) query.q = params.q;
    if (params.from) query.from = params.from;
    if (params.to) query.to = params.to;

    try {
      const { data, error } = await daemon.GET("/history", {
        params: {
          query: query as {
            q?: string;
            from?: string;
            to?: string;
            limit?: number;
          },
        },
      });
      if (error || !data) throw new Error(formatError(error ?? "empty response"));
      // Guard against out-of-order responses (user kept typing): only
      // commit if this fetch is still the latest one.
      if (this.lastQueryKey !== key) return;
      runInAction(() => {
        this.entries = data as HistoryEntry[];
        this.loadingState = "loaded";
      });
    } catch (e) {
      if (this.lastQueryKey !== key) return;
      runInAction(() => {
        this.error = e instanceof Error ? e.message : String(e);
        this.loadingState = "error";
      });
    }
  }

  /**
   * Drop the cache and re-fetch the last query. Called by StatusStore
   * after a recording completes so new entries surface without a
   * manual refresh.
   */
  async invalidate(): Promise<void> {
    if (this.lastQueryKey === null) return;
    const key = this.lastQueryKey;
    this.lastQueryKey = null;
    const params = parseQueryKey(key);
    await this.load(params);
  }
}

function queryKey(params: HistoryQuery): string {
  return JSON.stringify({
    q: params.q ?? "",
    from: params.from ?? "",
    to: params.to ?? "",
    limit: params.limit ?? 50,
  });
}

function parseQueryKey(key: string): HistoryQuery {
  try {
    return JSON.parse(key) as HistoryQuery;
  } catch {
    return {};
  }
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
