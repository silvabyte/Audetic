import { contextBridge, ipcRenderer } from "electron";

export type ThemeMode = "system" | "light" | "dark";

const audetic = {
  platform: process.platform as NodeJS.Platform,

  /**
   * Open `~/.config/audetic/config.toml` in the user's default
   * editor / file handler via `shell.openPath` in the main process.
   * Returns the empty string on success, or an error message from
   * Electron. Resolves to "daemon-not-running: <reason>" on IPC
   * failure so the renderer can surface a clear message.
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
};

try {
  contextBridge.exposeInMainWorld("audetic", audetic);
} catch (error) {
  console.error(error);
}

export type AudeticBridge = typeof audetic;
