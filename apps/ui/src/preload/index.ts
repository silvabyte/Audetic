import { contextBridge, ipcRenderer } from "electron";

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
};

try {
  contextBridge.exposeInMainWorld("audetic", audetic);
} catch (error) {
  console.error(error);
}

export type AudeticBridge = typeof audetic;
