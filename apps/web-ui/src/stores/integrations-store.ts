import { makeAutoObservable, runInAction } from "mobx";
import { daemon } from "@/api/client";
import type { components } from "@/api/schema";

export type AccessKey = components["schemas"]["AccessKeyInfo"];
export type AccessKeyScope = components["schemas"]["AccessKeyScope"];
export type ExternalImport = components["schemas"]["ExternalImportInfo"];
export type IntegrationOverview = components["schemas"]["IntegrationOverview"];
export type IssuedAccessKey = components["schemas"]["IssuedAccessKey"];
export type PlaudStatus = components["schemas"]["PlaudStatus"];

type LoadState = "idle" | "loading" | "loaded" | "error";

export class IntegrationsStore {
  overview: IntegrationOverview | null = null;
  keys: AccessKey[] = [];
  imports: ExternalImport[] = [];
  plaud: PlaudStatus | null = null;
  issuedKey: IssuedAccessKey | null = null;
  intervalMinutes = 15;
  state: LoadState = "idle";
  working = false;
  error: string | null = null;
  loadEpoch = 0;
  syncPollEpoch = 0;
  private client: typeof daemon;

  constructor(client: typeof daemon = daemon) {
    this.client = client;
    makeAutoObservable<this, "client">(this, { client: false });
  }

  async load(): Promise<void> {
    if (this.working) return;
    this.syncPollEpoch += 1;
    const epoch = ++this.loadEpoch;
    this.state = "loading";
    this.error = null;
    try {
      const [overview, keys, imports, plaud] = await Promise.all([
        this.client.GET("/integrations"),
        this.client.GET("/integrations/keys"),
        this.client.GET("/integrations/imports", { params: { query: { limit: 20 } } }),
        this.client.GET("/integrations/plaud"),
      ]);
      const overviewData = requireData(overview);
      const keysData = requireData(keys);
      const importsData = requireData(imports);
      const plaudData = requireData(plaud);
      if (epoch !== this.loadEpoch) return;
      runInAction(() => {
        this.overview = overviewData;
        this.keys = keysData.keys;
        this.imports = importsData.imports;
        this.plaud = plaudData;
        this.intervalMinutes = plaudData.interval_minutes;
        this.state = "loaded";
      });
      if (plaudData.running) {
        const pollEpoch = ++this.syncPollEpoch;
        void this.refreshAfterSync(pollEpoch);
      }
    } catch (error) {
      if (epoch !== this.loadEpoch) return;
      this.fail(error);
      runInAction(() => {
        this.state = "error";
      });
    }
  }

  setIntervalMinutes(value: number): void {
    this.intervalMinutes = value;
  }

  clearIssuedKey(): void {
    this.issuedKey = null;
  }

  async createKey(name: string, scope: AccessKeyScope): Promise<boolean> {
    if (this.issuedKey) {
      this.error = "Save or dismiss the current one-time key before creating another";
      return false;
    }
    return this.perform(async () => {
      const { data, error } = await this.client.POST("/integrations/keys", {
        body: { name, scope },
      });
      if (error || !data) throw new Error(formatError(error));
      const { secret, ...info } = data;
      runInAction(() => {
        this.issuedKey = { ...info, secret };
        this.keys = [info, ...this.keys];
      });
    });
  }

  async revokeKey(id: string): Promise<boolean> {
    return this.perform(async () => {
      const { data, error } = await this.client.DELETE("/integrations/keys/{id}", {
        params: { path: { id } },
      });
      if (error || !data) throw new Error(formatError(error));
      runInAction(() => {
        this.keys = this.keys.map((key) => (key.id === id ? data : key));
      });
    });
  }

  async updatePlaud(enabled: boolean): Promise<boolean> {
    if (this.intervalMinutes < 5 || this.intervalMinutes > 1440) {
      this.error = "Sync interval must be between 5 and 1440 minutes";
      return false;
    }
    return this.perform(async () => {
      const { data, error } = await this.client.PUT("/integrations/plaud", {
        body: { enabled, interval_minutes: this.intervalMinutes },
      });
      if (error || !data) throw new Error(formatError(error));
      runInAction(() => {
        this.plaud = data;
      });
    });
  }

  async syncPlaud(backfill = false): Promise<boolean> {
    const pollEpoch = ++this.syncPollEpoch;
    const succeeded = await this.perform(async () => {
      const response = backfill
        ? await this.client.POST("/integrations/plaud/backfill")
        : await this.client.POST("/integrations/plaud/sync");
      if (response.error || !response.data) throw new Error(formatError(response.error));
      runInAction(() => {
        if (this.plaud) this.plaud.running = response.data.scheduled;
      });
    });
    if (succeeded) void this.refreshAfterSync(pollEpoch);
    return succeeded;
  }

  private async refreshAfterSync(epoch: number): Promise<void> {
    for (let attempt = 0; attempt < 150; attempt += 1) {
      await new Promise((resolve) => setTimeout(resolve, 2000));
      if (epoch !== this.syncPollEpoch) return;
      try {
        const plaud = requireData(await this.client.GET("/integrations/plaud"));
        runInAction(() => {
          this.plaud = plaud;
        });
        if (plaud.running) continue;
        const imports = requireData(
          await this.client.GET("/integrations/imports", { params: { query: { limit: 20 } } }),
        );
        runInAction(() => {
          this.imports = imports.imports;
        });
        return;
      } catch (error) {
        this.fail(error);
        return;
      }
    }
    this.fail(new Error("Plaud sync is still running; refresh to check it later"));
  }

  private async perform(operation: () => Promise<void>): Promise<boolean> {
    if (this.working || this.state === "loading") return false;
    this.loadEpoch += 1;
    this.working = true;
    this.error = null;
    try {
      await operation();
      return true;
    } catch (error) {
      this.fail(error);
      return false;
    } finally {
      runInAction(() => {
        this.working = false;
      });
    }
  }

  private fail(error: unknown): void {
    runInAction(() => {
      this.error = error instanceof Error ? error.message : String(error);
    });
  }
}

function formatError(error: unknown): string {
  if (typeof error === "string") return error;
  if (error && typeof error === "object" && "message" in error) {
    return String((error as { message: unknown }).message);
  }
  try {
    return JSON.stringify(error ?? "empty response");
  } catch {
    return String(error);
  }
}

function requireData<T>(response: { data?: T; error?: unknown }): T {
  if (response.data === undefined) throw new Error(formatError(response.error));
  return response.data;
}
