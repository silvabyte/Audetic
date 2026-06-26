import { makeAutoObservable, runInAction } from "mobx";
import type { RootStore } from "./root-store";
import { daemon } from "@/api/client";
import type { components } from "@/api/schema";

export type ProviderInfo = components["schemas"]["ProviderInfo"];
export type ProviderStatus = components["schemas"]["ProviderStatus"];
// The keybind status the daemon serves is platform-tagged (`platform` plus the
// status union), so the UI can render macOS (native global hotkey) vs. Linux
// (Hyprland config) affordances.
export type KeybindStatus = components["schemas"]["KeybindStatusResponse"];
export type UpdateReport = components["schemas"]["UpdateReport"];

type Status = "idle" | "loading" | "loaded" | "error";

/**
 * ConfigStore backs the /settings/* routes. Each section tracks its
 * own load state so a slow endpoint (e.g. update check hitting the
 * network) doesn't block the rest of the page.
 *
 * Everything here is read-or-toggle: the daemon exposes no generic
 * PUT /config, so [behavior] / [meeting] / [whisper] tuning happens
 * by editing ~/.config/audetic/config.toml directly. The Settings →
 * Config file page opens that file via the preload bridge.
 */
export class ConfigStore {
  provider: ProviderInfo | null = null;
  providerState: Status = "idle";

  providerStatus: ProviderStatus | null = null;
  providerStatusState: Status = "idle";

  keybind: KeybindStatus | null = null;
  keybindState: Status = "idle";

  update: UpdateReport | null = null;
  updateState: Status = "idle";

  autoUpdate: boolean = false;
  autoUpdateState: Status = "idle";

  /** Error stashed for the last explicitly user-triggered op. */
  lastError: string | null = null;

  private root: RootStore;

  constructor(root: RootStore) {
    this.root = root;
    makeAutoObservable<this, "root">(this, { root: false });
  }

  /** Fire off the read-only fetches in parallel. */
  async loadAll(): Promise<void> {
    await Promise.allSettled([
      this.loadProvider(),
      this.loadProviderStatus(),
      this.loadKeybind(),
      this.loadUpdate(),
      this.loadAutoUpdate(),
    ]);
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

  async loadUpdate(): Promise<void> {
    runInAction(() => {
      this.updateState = "loading";
    });
    try {
      const { data, error } = await daemon.GET("/update/check");
      if (error || !data) throw new Error(formatError(error ?? "empty response"));
      runInAction(() => {
        this.update = data;
        this.updateState = "loaded";
      });
    } catch {
      runInAction(() => {
        this.updateState = "error";
      });
    }
  }

  async installKeybind(key?: string): Promise<void> {
    try {
      const { error } = await daemon.POST("/keybind/install", {
        body: key ? { key } : {},
      });
      if (error) throw new Error(formatError(error));
      await this.loadKeybind();
    } catch (e) {
      runInAction(() => {
        this.lastError = e instanceof Error ? e.message : String(e);
      });
    }
  }

  async uninstallKeybind(): Promise<void> {
    try {
      const { error } = await daemon.DELETE("/keybind", {});
      if (error) throw new Error(formatError(error));
      await this.loadKeybind();
    } catch (e) {
      runInAction(() => {
        this.lastError = e instanceof Error ? e.message : String(e);
      });
    }
  }

  async installUpdate(force = false): Promise<void> {
    runInAction(() => {
      this.updateState = "loading";
    });
    try {
      const { data, error } = await daemon.POST("/update/install", {
        body: { force },
      });
      if (error || !data) throw new Error(formatError(error ?? "empty response"));
      runInAction(() => {
        this.update = data;
        this.updateState = "loaded";
      });
    } catch (e) {
      runInAction(() => {
        this.updateState = "error";
        this.lastError = e instanceof Error ? e.message : String(e);
      });
    }
  }

  async loadAutoUpdate(): Promise<void> {
    runInAction(() => {
      this.autoUpdateState = "loading";
    });
    try {
      const { data, error } = await daemon.GET("/update/auto");
      if (error || !data) throw new Error(formatError(error ?? "empty response"));
      runInAction(() => {
        this.autoUpdate = data.enabled;
        this.autoUpdateState = "loaded";
      });
    } catch {
      runInAction(() => {
        this.autoUpdateState = "error";
      });
    }
  }

  async setAutoUpdate(enabled: boolean): Promise<void> {
    const previous = this.autoUpdate;
    runInAction(() => {
      this.autoUpdate = enabled;
    });
    try {
      const { data, error } = await daemon.PUT("/update/auto", {
        body: { enabled },
      });
      if (error) throw new Error(formatError(error));
      if (data) {
        runInAction(() => {
          this.autoUpdate = data.auto_update;
        });
      }
    } catch (e) {
      runInAction(() => {
        this.autoUpdate = previous;
        this.lastError = e instanceof Error ? e.message : String(e);
      });
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
