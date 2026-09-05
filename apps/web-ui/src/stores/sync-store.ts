import { makeAutoObservable, runInAction } from "mobx";
import { daemon } from "@/api/client";
import type { components } from "@/api/schema";

export type CacheLevel = components["schemas"]["CacheLevel"];
export type HubCandidate = components["schemas"]["HubCandidate"];
export type HubConnection = components["schemas"]["HubConnection"];
export type SyncDiscoveryFailure = components["schemas"]["SyncDiscoveryFailure"];
export type SyncRole = components["schemas"]["SyncRole"];
export type SyncSetupRequest = components["schemas"]["SyncSetupRequest"];
export type SyncSetupResult = components["schemas"]["SyncSetupResult"];
export type SyncStatus = components["schemas"]["SyncStatus"];

export type SyncPreferences = {
  deviceName: string | null;
  uploadRecordingPayloads: boolean;
  cacheLevel: CacheLevel;
  sharedConfigEnabled: boolean;
};

type LoadState = "idle" | "loading" | "loaded" | "error";
type SyncOperation =
  | "discovering"
  | "previewing_home_hub"
  | "activating_home_hub"
  | "connecting"
  | "updating_payload_policy"
  | "retrying"
  | "demoting";

const POLL_MS = 10_000;

/** Typed consumer for the daemon-owned Shared Library setup surface. */
export class SyncStore {
  status: SyncStatus | null = null;
  state: LoadState = "idle";
  error: string | null = null;
  actionError: string | null = null;
  operation: SyncOperation | null = null;

  discoveredHubs: HubCandidate[] = [];
  discoveryFailures: SyncDiscoveryFailure[] = [];
  discoveryAttempted = false;
  selectedHubId: string | null = null;

  servePreview: string | null = null;
  setupCommand: string | null = null;

  private pollTimer: ReturnType<typeof setTimeout> | null = null;
  private polling = false;

  constructor() {
    makeAutoObservable<this, "pollTimer" | "polling">(this, {
      pollTimer: false,
      polling: false,
    });
  }

  get loading(): boolean {
    return this.state === "loading";
  }

  get selectedHub(): HubCandidate | null {
    return (
      this.discoveredHubs.find(
        (candidate) => candidate.connection.hub_id === this.selectedHubId,
      ) ?? null
    );
  }

  start(): void {
    if (this.polling) return;
    this.polling = true;
    void this.pollNow();
  }

  stop(): void {
    this.polling = false;
    if (this.pollTimer !== null) {
      clearTimeout(this.pollTimer);
      this.pollTimer = null;
    }
  }

  async refresh(): Promise<void> {
    await this.loadStatus(false);
  }

  selectHub(hubId: string): void {
    this.selectedHubId = hubId;
  }

  async discover(): Promise<SyncSetupResult | null> {
    runInAction(() => {
      this.operation = "discovering";
      this.actionError = null;
      this.discoveryAttempted = true;
    });
    try {
      const { data, error } = await daemon.POST("/sync/discover");
      if (error || !data) throw new Error(formatError(error ?? "empty response"));
      runInAction(() => {
        this.applyResult(data);
        this.operation = null;
      });
      return data;
    } catch (error) {
      runInAction(() => {
        this.operation = null;
        this.actionError = error instanceof Error ? error.message : String(error);
      });
      return null;
    }
  }

  async previewHomeHub(preferences: SyncPreferences): Promise<SyncSetupResult | null> {
    return this.configure(
      requestFor("home_hub", preferences, null, false),
      "previewing_home_hub",
    );
  }

  async activateHomeHub(preferences: SyncPreferences): Promise<SyncSetupResult | null> {
    return this.configure(
      requestFor("home_hub", preferences, null, true),
      "activating_home_hub",
    );
  }

  async connectSelectedHub(preferences: SyncPreferences): Promise<SyncSetupResult | null> {
    const hub = this.selectedHub?.connection;
    if (!hub) {
      runInAction(() => {
        this.actionError = "Choose a Home Hub before connecting.";
      });
      return null;
    }
    return this.configure(
      requestFor("connected_device", preferences, hub, false),
      "connecting",
    );
  }

  async demoteToStandalone(): Promise<SyncSetupResult | null> {
    const preferences = preferencesFromStatus(this.status);
    return this.configure(
      requestFor("standalone", preferences, null, false),
      "demoting",
    );
  }

  async updateRecordingPayloadPolicy(enabled: boolean): Promise<boolean> {
    const status = this.status;
    if (!status || status.role === "standalone") return false;
    runInAction(() => {
      this.operation = "updating_payload_policy";
      this.actionError = null;
    });
    try {
      const { data, error } = await daemon.PUT("/sync/payload-policy", {
        body: { upload_recording_payloads: enabled },
      });
      if (error || !data) throw new Error(formatError(error ?? "empty response"));
      runInAction(() => {
        if (this.status) {
          this.status = {
            ...this.status,
            upload_recording_payloads: data.upload_recording_payloads,
          };
        }
        this.operation = null;
      });
      return true;
    } catch (error) {
      runInAction(() => {
        this.operation = null;
        this.actionError = error instanceof Error ? error.message : String(error);
      });
      return false;
    }
  }

  async retryPending(): Promise<void> {
    runInAction(() => {
      this.operation = "retrying";
      this.actionError = null;
    });
    try {
      const { error } = await daemon.POST("/sync/retry");
      if (error) throw new Error(formatError(error));
      await this.loadStatus(true);
      runInAction(() => { this.operation = null; });
    } catch (error) {
      runInAction(() => {
        this.operation = null;
        this.actionError = error instanceof Error ? error.message : String(error);
      });
    }
  }

  private async configure(
    request: SyncSetupRequest,
    operation: SyncOperation,
  ): Promise<SyncSetupResult | null> {
    runInAction(() => {
      this.operation = operation;
      this.actionError = null;
    });
    try {
      const { data, error } = await daemon.POST("/sync/configure", {
        body: request,
      });
      if (error || !data) throw new Error(formatError(error ?? "empty response"));
      runInAction(() => {
        this.applyResult(data);
        this.operation = null;
      });
      return data;
    } catch (error) {
      runInAction(() => {
        this.operation = null;
        this.actionError = error instanceof Error ? error.message : String(error);
      });
      return null;
    }
  }

  private applyResult(result: SyncSetupResult): void {
    this.status = result.status;
    this.discoveredHubs = result.discovered_hubs;
    this.discoveryFailures = result.discovery_failures;
    this.servePreview = result.serve_preview ?? null;
    this.setupCommand = result.setup_command ?? null;

    const selectedStillExists = result.discovered_hubs.some(
      (candidate) => candidate.connection.hub_id === this.selectedHubId,
    );
    if (result.discovered_hubs.length === 1) {
      this.selectedHubId = result.discovered_hubs[0]?.connection.hub_id ?? null;
    } else if (!selectedStillExists) {
      this.selectedHubId = null;
    }

    if (result.status.role === "standalone" && result.serve_preview === null) {
      this.setupCommand = null;
    }
  }

  private async loadStatus(background: boolean): Promise<void> {
    if (!background) {
      runInAction(() => {
        this.state = "loading";
        this.error = null;
      });
    }
    try {
      const { data, error } = await daemon.GET("/sync/status");
      if (error || !data) throw new Error(formatError(error ?? "empty response"));
      runInAction(() => {
        this.status = data;
        this.state = "loaded";
        this.error = null;
      });
    } catch (error) {
      runInAction(() => {
        if (!background || this.status === null) this.state = "error";
        this.error = error instanceof Error ? error.message : String(error);
      });
    }
  }

  private async pollNow(): Promise<void> {
    try {
      await this.loadStatus(this.status !== null);
    } finally {
      if (this.polling) {
        this.pollTimer = setTimeout(() => {
          void this.pollNow();
        }, POLL_MS);
      }
    }
  }
}

export function preferencesFromStatus(status: SyncStatus | null): SyncPreferences {
  return {
    deviceName: status?.device_name ?? null,
    uploadRecordingPayloads: status?.upload_recording_payloads ?? false,
    cacheLevel: status?.cache_level ?? "live_only",
    sharedConfigEnabled: status?.shared_config_enabled ?? true,
  };
}

function requestFor(
  role: SyncRole,
  preferences: SyncPreferences,
  hub: HubConnection | null,
  confirmServeChange: boolean,
): SyncSetupRequest {
  return {
    role,
    device_name: preferences.deviceName?.trim() || null,
    hub,
    upload_recording_payloads: preferences.uploadRecordingPayloads,
    cache_level: preferences.cacheLevel,
    shared_config_enabled: preferences.sharedConfigEnabled,
    confirm_serve_change: confirmServeChange,
  };
}

function formatError(error: unknown): string {
  if (typeof error === "string") return error;
  if (error && typeof error === "object" && "message" in error) {
    return String((error as { message: unknown }).message);
  }
  try {
    return JSON.stringify(error);
  } catch {
    return String(error);
  }
}
