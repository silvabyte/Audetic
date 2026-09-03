import { makeAutoObservable, observable, runInAction } from "mobx";
import type { RootStore } from "./root-store";
import { daemon } from "@/api/client";
import type { components } from "@/api/schema";

export type ProviderInfo = components["schemas"]["ProviderInfo"];
export type ProviderStatus = components["schemas"]["ProviderStatus"];
export type ProviderTestResult = components["schemas"]["ProviderTestResult"];
export type WhisperConfig = components["schemas"]["WhisperConfig"];
export type RestartAccepted = components["schemas"]["RestartAccepted"];
export type ProviderRuntimeStatus = components["schemas"]["ProviderRuntimeStatus"];
export type VersionInfo = components["schemas"]["VersionInfo"];
export type KeybindStatus = components["schemas"]["KeybindStatus"];
export type KeybindStatuses = components["schemas"]["KeybindStatuses"];
export type KeybindTarget = components["schemas"]["KeybindTarget"];
export type InstallResult = components["schemas"]["InstallResult"];
export type UninstallResult = components["schemas"]["UninstallResult"];
export type ModelDescriptor = components["schemas"]["ModelDescriptor"];

type Status = "idle" | "loading" | "loaded" | "error";
type MutationStatus = "idle" | "loading" | "success" | "error";
type RestartStatus = "idle" | "requesting" | "waiting" | "complete" | "error";

/**
 * ConfigStore backs the /settings/* routes. Each section tracks its
 * own load state so a slow endpoint doesn't block the rest of the page.
 *
 * Provider settings use their dedicated typed API. Other config sections that
 * do not have a domain-specific endpoint remain config-file driven.
 */
export class ConfigStore {
  provider: ProviderInfo | null = null;
  providerState: Status = "idle";

  providerStatus: ProviderStatus | null = null;
  providerStatusState: Status = "idle";

  providerRuntime: ProviderRuntimeStatus | null = null;
  providerRuntimeState: Status = "idle";

  providerConfig: WhisperConfig | null = null;
  providerConfigState: Status = "idle";
  providerConfigError: string | null = null;

  providerValidation: ProviderStatus | null = null;
  providerValidationState: MutationStatus = "idle";
  providerValidationError: string | null = null;

  providerSaveState: MutationStatus = "idle";
  providerSaveError: string | null = null;

  providerTestResult: ProviderTestResult | null = null;
  providerTestState: MutationStatus = "idle";
  providerTestError: string | null = null;

  restartRequired = false;
  restartState: RestartStatus = "idle";
  restartResult: RestartAccepted | null = null;
  restartError: string | null = null;

  keybind: KeybindStatuses | null = null;
  keybindState: Status = "idle";

  /** On-device transcription models (catalog + install/download state). */
  models: ModelDescriptor[] = [];
  modelsState: Status = "idle";

  /** Error stashed for the last explicitly user-triggered op. */
  lastError: string | null = null;

  private root: RootStore;
  private modelPolls = new Map<string, ReturnType<typeof setInterval>>();

  constructor(root: RootStore) {
    this.root = root;
    makeAutoObservable<this, "root">(this, {
      root: false,
      providerConfig: observable.ref,
    });
  }

  /** Fire off the read-only fetches in parallel. */
  async loadAll(): Promise<void> {
    await Promise.allSettled([
      this.loadProvider(),
      this.loadProviderStatus(),
      this.loadProviderRuntime(),
      this.loadProviderConfig(),
      this.loadModels(),
      this.loadKeybind(),
    ]);
  }

  async loadModels(): Promise<void> {
    runInAction(() => {
      this.modelsState = "loading";
    });
    try {
      const { data, error } = await daemon.GET("/models");
      if (error || !data) throw new Error(formatError(error ?? "empty response"));
      runInAction(() => {
        this.models = data.models;
        this.modelsState = "loaded";
      });
    } catch {
      runInAction(() => {
        this.modelsState = "error";
      });
    }
  }

  /** Start a model download and poll its status until it finishes or errors. */
  async downloadModel(id: string): Promise<void> {
    try {
      const { data, error } = await daemon.POST("/models/{id}/download", {
        params: { path: { id } },
      });
      if (error) throw new Error(formatError(error));
      if (data) this.mergeModel(data);
    } catch (e) {
      runInAction(() => {
        this.lastError = e instanceof Error ? e.message : String(e);
      });
      return;
    }
    this.pollModel(id);
  }

  private pollModel(id: string): void {
    if (this.modelPolls.has(id)) return;
    const handle = setInterval(async () => {
      const { data } = await daemon.GET("/models/{id}", {
        params: { path: { id } },
      });
      if (!data) return;
      this.mergeModel(data);
      const done =
        data.installed ||
        data.download?.state === "completed" ||
        data.download?.state === "error";
      if (done) {
        clearInterval(handle);
        this.modelPolls.delete(id);
      }
    }, 1000);
    this.modelPolls.set(id, handle);
  }

  private mergeModel(model: ModelDescriptor): void {
    runInAction(() => {
      this.models = this.models.map((m) => (m.id === model.id ? model : m));
    });
  }

  async loadProvider(): Promise<void> {
    runInAction(() => {
      this.providerState = "loading";
    });
    try {
      const { data, error } = await daemon.GET("/provider");
      if (error || !data) throw new Error(formatError(error ?? "empty response"));
      runInAction(() => {
        this.provider = data;
        this.providerState = "loaded";
      });
    } catch {
      runInAction(() => {
        this.providerState = "error";
      });
    }
  }

  async loadProviderStatus(): Promise<void> {
    runInAction(() => {
      this.providerStatusState = "loading";
    });
    try {
      const { data, error } = await daemon.GET("/provider/status");
      if (error || !data) throw new Error(formatError(error ?? "empty response"));
      runInAction(() => {
        this.providerStatus = data;
        this.providerStatusState = "loaded";
      });
    } catch {
      runInAction(() => {
        this.providerStatusState = "error";
      });
    }
  }

  async loadProviderRuntime(): Promise<void> {
    runInAction(() => {
      this.providerRuntimeState = "loading";
    });
    try {
      const { data, error } = await daemon.GET("/provider/runtime");
      if (error || !data) throw new Error(formatError(error ?? "empty response"));
      runInAction(() => {
        this.providerRuntime = data;
        this.providerRuntimeState = "loaded";
        this.restartRequired = data.restart_required;
      });
    } catch {
      runInAction(() => {
        this.providerRuntimeState = "error";
      });
    }
  }

  async loadProviderConfig(): Promise<void> {
    runInAction(() => {
      this.providerConfigState = "loading";
      this.providerConfigError = null;
    });
    try {
      const { data, error } = await daemon.GET("/provider/config");
      if (error || !data) throw new Error(formatError(error ?? "empty response"));
      runInAction(() => {
        this.providerConfig = data;
        this.providerConfigState = "loaded";
      });
    } catch (error) {
      runInAction(() => {
        this.providerConfigState = "error";
        this.providerConfigError = error instanceof Error ? error.message : String(error);
      });
    }
  }

  async validateProviderConfig(config: WhisperConfig): Promise<ProviderStatus | null> {
    runInAction(() => {
      this.providerValidationState = "loading";
      this.providerValidation = null;
      this.providerValidationError = null;
    });
    try {
      const { data, error } = await daemon.POST("/provider/validate", { body: config });
      if (error || !data) throw new Error(formatError(error ?? "empty response"));
      runInAction(() => {
        this.providerValidation = data;
        this.providerValidationState = "success";
      });
      return data;
    } catch (error) {
      runInAction(() => {
        this.providerValidationState = "error";
        this.providerValidationError = error instanceof Error ? error.message : String(error);
      });
      return null;
    }
  }

  async saveProviderConfig(config: WhisperConfig): Promise<WhisperConfig | null> {
    runInAction(() => {
      this.providerSaveState = "loading";
      this.providerSaveError = null;
    });

    const validation = await this.validateProviderConfig(config);
    if (!validation || validation.status !== "ready") {
      runInAction(() => {
        this.providerSaveState = "error";
        this.providerSaveError =
          validation?.status === "config_error"
            ? validation.error
            : validation?.status === "not_configured"
              ? "Choose a transcription provider."
              : this.providerValidationError ?? "Provider validation failed.";
      });
      return null;
    }

    try {
      const { data, error } = await daemon.PUT("/provider/config", { body: config });
      if (error || !data) throw new Error(formatError(error ?? "empty response"));
      runInAction(() => {
        this.providerConfig = data;
        this.providerConfigState = "loaded";
        this.providerSaveState = "success";
        // Be conservative until the daemon-authoritative runtime comparison
        // below proves this process already uses the same configuration.
        this.restartRequired = true;
        this.restartState = "idle";
        this.restartResult = null;
        this.restartError = null;
        this.providerTestState = "idle";
        this.providerTestResult = null;
        this.providerTestError = null;
      });
      await Promise.all([this.loadProvider(), this.loadProviderRuntime()]);
      return data;
    } catch (error) {
      runInAction(() => {
        this.providerSaveState = "error";
        this.providerSaveError = error instanceof Error ? error.message : String(error);
      });
      return null;
    }
  }

  async testProvider(file?: string): Promise<ProviderTestResult | null> {
    runInAction(() => {
      this.providerTestState = "loading";
      this.providerTestResult = null;
      this.providerTestError = null;
    });
    try {
      const { data, error } = await daemon.POST("/provider/test", {
        body: file ? { file } : {},
      });
      if (error || !data) throw new Error(formatError(error ?? "empty response"));
      runInAction(() => {
        this.providerTestResult = data;
        this.providerTestState = data.success ? "success" : "error";
        this.providerTestError = data.success
          ? null
          : data.error ?? "Provider initialization validation failed.";
      });
      await this.loadProviderStatus();
      return data;
    } catch (error) {
      runInAction(() => {
        this.providerTestState = "error";
        this.providerTestError = error instanceof Error ? error.message : String(error);
      });
      return null;
    }
  }

  async restartDaemon(): Promise<boolean> {
    runInAction(() => {
      this.restartState = "requesting";
      this.restartResult = null;
      this.restartError = null;
    });
    try {
      const before = await getDaemonVersion();
      const { data, error } = await daemon.POST("/system/restart");
      if (error || !data) throw new Error(formatError(error ?? "empty response"));
      runInAction(() => {
        this.restartResult = data;
        this.restartState = "waiting";
      });

      await sleep(Math.max(700, data.delay_ms + 350));
      await waitForDaemonRestart(before.instance_id);
      await Promise.allSettled([
        this.loadProvider(),
        this.loadProviderStatus(),
        this.loadProviderConfig(),
        this.loadProviderRuntime(),
        this.root.setup.recheck(),
      ]);
      if (this.providerRuntimeState !== "loaded" || this.restartRequired) {
        throw new Error("The new daemon did not activate the saved provider configuration.");
      }
      runInAction(() => {
        this.restartState = "complete";
      });
      return true;
    } catch (error) {
      runInAction(() => {
        this.restartState = "error";
        this.restartError = error instanceof Error ? error.message : String(error);
      });
      return false;
    }
  }

  clearProviderFeedback(): void {
    if (this.providerValidationState !== "loading") {
      this.providerValidationState = "idle";
      this.providerValidation = null;
      this.providerValidationError = null;
    }
    if (this.providerSaveState !== "loading") {
      this.providerSaveState = "idle";
      this.providerSaveError = null;
    }
  }

  async loadKeybind(): Promise<void> {
    runInAction(() => {
      this.keybindState = "loading";
    });
    try {
      const { data, error } = await daemon.GET("/keybind/status");
      if (error || !data) throw new Error(formatError(error ?? "empty response"));
      runInAction(() => {
        this.keybind = data;
        this.keybindState = "loaded";
      });
    } catch {
      runInAction(() => {
        this.keybindState = "error";
      });
    }
  }

  async previewKeybind(
    target: KeybindTarget,
    key?: string,
  ): Promise<InstallResult | null> {
    return this.installKeybind(target, key, true);
  }

  async installKeybind(
    target: KeybindTarget,
    key?: string,
    dryRun = false,
  ): Promise<InstallResult | null> {
    runInAction(() => {
      this.lastError = null;
    });
    try {
      const { data, error } = await daemon.POST("/keybind/install", {
        body: { target, dry_run: dryRun, ...(key ? { key } : {}) },
      });
      if (error || !data) throw new Error(formatError(error ?? "empty response"));
      if (!dryRun) await this.loadKeybind();
      return data;
    } catch (e) {
      runInAction(() => {
        this.lastError = e instanceof Error ? e.message : String(e);
      });
      return null;
    }
  }

  async uninstallKeybind(
    target: KeybindTarget,
    dryRun = false,
  ): Promise<UninstallResult | null> {
    runInAction(() => {
      this.lastError = null;
    });
    try {
      const { data, error } = await daemon.DELETE("/keybind", {
        params: { query: { target, dry_run: dryRun } },
      });
      if (error || !data) throw new Error(formatError(error ?? "empty response"));
      if (!dryRun) await this.loadKeybind();
      return data;
    } catch (e) {
      runInAction(() => {
        this.lastError = e instanceof Error ? e.message : String(e);
      });
      return null;
    }
  }

  clearError(): void {
    this.lastError = null;
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

function sleep(milliseconds: number): Promise<void> {
  return new Promise((resolve) => window.setTimeout(resolve, milliseconds));
}

async function getDaemonVersion(): Promise<VersionInfo> {
  const { data, error } = await daemon.GET("/version");
  if (error || !data) throw new Error(formatError(error ?? "empty response"));
  return data;
}

async function waitForDaemonRestart(previousInstanceId: string): Promise<void> {
  const deadline = Date.now() + 15_000;
  while (Date.now() < deadline) {
    try {
      const { data, error } = await daemon.GET("/version");
      if (!error && data && data.instance_id !== previousInstanceId) return;
    } catch {
      // The expected middle of a restart. Keep polling until the deadline.
    }
    await sleep(350);
  }
  throw new Error("A new daemon instance did not start within 15 seconds.");
}
