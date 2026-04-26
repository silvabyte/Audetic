import { app, BrowserWindow, ipcMain } from "electron";
import { autoUpdater, type UpdateInfo } from "electron-updater";

/**
 * App-update orchestrator. Wraps electron-updater so the renderer can
 * subscribe to a single typed event stream and trigger Check / Install.
 *
 * Behavior:
 *   - production-only. In dev (`!app.isPackaged`) we skip everything;
 *     the autoUpdater would no-op anyway because there's no app-update.yml.
 *   - check on launch (5s grace), then every 6h.
 *   - download-on-finding-update is the electron-updater default and
 *     we keep it — fewer round trips for the user.
 *   - install is gated behind an explicit user action (toast button or
 *     Settings → Updates "Install"). We never quitAndInstall silently.
 */

const SIX_HOURS_MS = 6 * 60 * 60 * 1000;
const CHECK_GRACE_MS = 5_000;

export type AppUpdateEvent =
  | { kind: "checking" }
  | { kind: "available"; version: string; releaseName?: string }
  | { kind: "not-available"; currentVersion: string }
  | { kind: "progress"; percent: number; bytesPerSecond: number }
  | { kind: "downloaded"; version: string }
  | { kind: "error"; message: string };

const EVENT_CHANNEL = "audetic:autoUpdate:event";

let pollTimer: NodeJS.Timeout | null = null;
let registered = false;

export function registerAutoUpdateIpc(
  getMainWindow: () => BrowserWindow | null,
): void {
  if (registered) return;
  registered = true;

  // Don't auto-update in dev. electron-updater can run, but it'll fail to
  // find dev-app-update.yml which produces noisy errors in the renderer.
  const enabled = app.isPackaged;

  // Forward electron-updater events to the renderer through one channel.
  const send = (event: AppUpdateEvent): void => {
    const win = getMainWindow();
    if (!win || win.isDestroyed()) return;
    win.webContents.send(EVENT_CHANNEL, event);
  };

  if (enabled) {
    autoUpdater.autoDownload = true;
    autoUpdater.autoInstallOnAppQuit = false;

    autoUpdater.on("checking-for-update", () => send({ kind: "checking" }));
    autoUpdater.on("update-available", (info: UpdateInfo) =>
      send({
        kind: "available",
        version: info.version,
        releaseName: info.releaseName ?? undefined,
      }),
    );
    autoUpdater.on("update-not-available", (info: UpdateInfo) =>
      send({ kind: "not-available", currentVersion: info.version }),
    );
    autoUpdater.on("error", (err: Error) =>
      send({ kind: "error", message: err.message }),
    );
    autoUpdater.on("download-progress", (p) =>
      send({
        kind: "progress",
        percent: p.percent,
        bytesPerSecond: p.bytesPerSecond,
      }),
    );
    autoUpdater.on("update-downloaded", (info: UpdateInfo) =>
      send({ kind: "downloaded", version: info.version }),
    );

    setTimeout(() => {
      void autoUpdater.checkForUpdates().catch((err) => {
        send({ kind: "error", message: String(err) });
      });
    }, CHECK_GRACE_MS);

    pollTimer = setInterval(() => {
      void autoUpdater.checkForUpdates().catch((err) => {
        send({ kind: "error", message: String(err) });
      });
    }, SIX_HOURS_MS);
  }

  // IPC — these are always exposed; in dev they just no-op so the renderer
  // doesn't have to branch.
  ipcMain.handle("audetic:autoUpdate:check", async () => {
    if (!enabled) return { ok: false, reason: "dev" };
    try {
      await autoUpdater.checkForUpdates();
      return { ok: true };
    } catch (err) {
      return { ok: false, reason: err instanceof Error ? err.message : String(err) };
    }
  });

  ipcMain.handle("audetic:autoUpdate:install", () => {
    if (!enabled) return { ok: false, reason: "dev" };
    // `quitAndInstall(isSilent=false, isForceRunAfter=true)` — show no UAC
    // splash on win, relaunch the app after install. AppImage / DMG handle
    // their own restart semantics.
    autoUpdater.quitAndInstall(false, true);
    return { ok: true };
  });

  ipcMain.handle("audetic:autoUpdate:currentVersion", () => app.getVersion());
}

export function destroyAutoUpdate(): void {
  if (pollTimer !== null) {
    clearInterval(pollTimer);
    pollTimer = null;
  }
}
