import { BrowserWindow, ipcMain } from "electron";
import * as linux from "./install/linux";
import { resolveInstallPaths, type InstallPaths } from "./install/paths";
import { binaryVersion, runningDaemonVersion } from "./install/versions";

/**
 * Top-level state the renderer needs to decide which onboarding card to
 * render. See plan ("Onboarding state machine") for the full table.
 */
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

/** Per-step progress event streamed to the renderer during install. */
export interface OnboardingProgress {
  step: string;
  detail?: string;
}

const PROGRESS_CHANNEL = "audetic:onboarding:progress";

/**
 * Wire the onboarding IPC surface. Call once on app start.
 *
 * `getMainWindow` returns the current main window (so we can target progress
 * events at it without holding a stale reference if the window gets
 * recreated after a quit-to-tray cycle).
 */
export function registerOnboardingIpc(
  getMainWindow: () => BrowserWindow | null,
): void {
  ipcMain.handle("audetic:onboarding:detect", async (): Promise<OnboardingState> => {
    return detectInstallState();
  });

  ipcMain.handle(
    "audetic:onboarding:install",
    async (): Promise<{ ok: true } | { ok: false; error: string }> => {
      const paths = resolveInstallPaths();
      const send = progressEmitter(getMainWindow);
      try {
        if (process.platform === "linux") {
          await linux.installService(paths, (step, detail) => send({ step, detail }));
        } else if (process.platform === "darwin") {
          // Phase 7 fills this in (launchd plist + launchctl bootstrap).
          throw new Error("macOS install not implemented yet");
        } else {
          throw new Error(`unsupported platform: ${process.platform}`);
        }
        return { ok: true };
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        send({ step: "error", detail: msg });
        return { ok: false, error: msg };
      }
    },
  );

  ipcMain.handle(
    "audetic:onboarding:enable",
    async (): Promise<{ ok: true } | { ok: false; error: string }> => {
      // Reuses the same install path's `systemctl enable --now` step. For
      // already-installed-but-disabled units this is the only step that
      // matters; redoing the copy is a harmless no-op since the file
      // contents match.
      const paths = resolveInstallPaths();
      const send = progressEmitter(getMainWindow);
      try {
        if (process.platform === "linux") {
          await linux.installService(paths, (step, detail) => send({ step, detail }));
        } else if (process.platform === "darwin") {
          throw new Error("macOS enable not implemented yet");
        }
        return { ok: true };
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        send({ step: "error", detail: msg });
        return { ok: false, error: msg };
      }
    },
  );

  ipcMain.handle(
    "audetic:onboarding:update",
    async (): Promise<{ ok: true } | { ok: false; error: string }> => {
      const paths = resolveInstallPaths();
      const send = progressEmitter(getMainWindow);
      try {
        if (process.platform === "linux") {
          await linux.updateBinary(paths, (step, detail) => send({ step, detail }));
        } else {
          throw new Error(`update not implemented on ${process.platform}`);
        }
        return { ok: true };
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        send({ step: "error", detail: msg });
        return { ok: false, error: msg };
      }
    },
  );
}

async function detectInstallState(): Promise<OnboardingState> {
  const paths = resolveInstallPaths();
  const platform = process.platform;

  const [bundled, installed, running] = await Promise.all([
    binaryVersion(paths.bundledBinary),
    binaryVersion(paths.installedBinary),
    runningDaemonVersion(),
  ]);

  let unitInstalled_ = false;
  let unitEnabled_ = false;
  let unitActive_ = false;
  if (platform === "linux") {
    unitInstalled_ = await linux.unitInstalled(paths);
    if (unitInstalled_) {
      [unitEnabled_, unitActive_] = await Promise.all([
        linux.unitEnabled(),
        linux.unitActive(),
      ]);
    }
  }

  return {
    platform,
    bundledVersion: bundled,
    installedVersion: installed,
    runningVersion: running,
    daemonReachable: running !== null,
    unitInstalled: unitInstalled_,
    unitEnabled: unitEnabled_,
    unitActive: unitActive_,
  };
}

function progressEmitter(
  getMainWindow: () => BrowserWindow | null,
): (p: OnboardingProgress) => void {
  return (progress) => {
    const win = getMainWindow();
    if (!win || win.isDestroyed()) return;
    win.webContents.send(PROGRESS_CHANNEL, progress);
  };
}

export { resolveInstallPaths } from "./install/paths";
