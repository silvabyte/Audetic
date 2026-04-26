import { contextBridge, ipcRenderer } from "electron";

export type ThemeMode = "system" | "light" | "dark";

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

export type OnboardingResult =
  | { ok: true }
  | { ok: false; error: string };

const PROGRESS_CHANNEL = "audetic:onboarding:progress";

const audetic = {
  platform: process.platform as NodeJS.Platform,

  /**
   * Open `~/.config/audetic/config.toml` in the user's default
   * editor / file handler via `shell.openPath` in the main process.
   * Returns the empty string on success, or an error message from
   * Electron.
   */
  openConfigFile(): Promise<string> {
    return ipcRenderer.invoke("audetic:openConfigFile");
  },

  /**
   * Read the persisted theme-mode preference from electron-store.
   * Resolves to the last user override — "system" if never set.
   */
  getThemeMode(): Promise<ThemeMode> {
    return ipcRenderer.invoke("audetic:getThemeMode");
  },

  /**
   * Persist the user's theme-mode override. No-op on invalid input;
   * the renderer is expected to pass one of the three valid strings.
   */
  setThemeMode(mode: ThemeMode): Promise<void> {
    return ipcRenderer.invoke("audetic:setThemeMode", mode);
  },

  /** Onboarding flow — see src/main/onboarding.ts. */
  onboarding: {
    detect(): Promise<OnboardingState> {
      return ipcRenderer.invoke("audetic:onboarding:detect");
    },
    install(): Promise<OnboardingResult> {
      return ipcRenderer.invoke("audetic:onboarding:install");
    },
    enable(): Promise<OnboardingResult> {
      return ipcRenderer.invoke("audetic:onboarding:enable");
    },
    update(): Promise<OnboardingResult> {
      return ipcRenderer.invoke("audetic:onboarding:update");
    },
    onProgress(callback: (p: OnboardingProgress) => void): () => void {
      const listener = (
        _event: Electron.IpcRendererEvent,
        progress: OnboardingProgress,
      ): void => {
        callback(progress);
      };
      ipcRenderer.on(PROGRESS_CHANNEL, listener);
      return () => ipcRenderer.removeListener(PROGRESS_CHANNEL, listener);
    },
  },
};

try {
  contextBridge.exposeInMainWorld("audetic", audetic);
} catch (error) {
  console.error(error);
}

export type AudeticBridge = typeof audetic;
