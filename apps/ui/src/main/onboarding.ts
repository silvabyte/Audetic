import { BrowserWindow, ipcMain } from "electron";
import * as linux from "./install/linux";
import { resolveInstallPaths, type InstallPaths } from "./install/paths";
import { binaryVersion, daemonSystemDeps, runningDaemonVersion } from "./install/versions";

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
  /**
   * Whether the daemon can find `ffmpeg` on PATH. Sourced from
   * GET /system/deps. Defaults to `false` whenever the daemon is
   * unreachable — the onboarding decision tree gates on `daemonReachable`
   * first, so a missing daemon never gets misreported as "missing FFmpeg".
   */
  ffmpegAvailable: boolean;
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

  ipcMain.handle(
    "audetic:onboarding:install-ffmpeg",
    async (): Promise<{ ok: true } | { ok: false; error: string }> => {
      const send = progressEmitter(getMainWindow);
      try {
        await runFfmpegInstall(send);
        return { ok: true };
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        send({ step: "error", detail: msg });
        return { ok: false, error: msg };
      }
    },
  );
}

const DAEMON_URL = "http://127.0.0.1:3737";
const POLL_INTERVAL_MS = 500;
const POLL_TIMEOUT_MS = 5 * 60 * 1000;

interface InstallStatusResponse {
  phase: "idle" | "starting" | "downloading" | "extracting" | "done" | "error";
  downloadedBytes?: number;
  totalBytes?: number;
  percent?: number;
  binaryPath?: string;
  message?: string;
}

/**
 * Drives the daemon's `POST /system/install-ffmpeg` + status-polling flow,
 * forwarding each phase change to the renderer over the existing
 * onboarding-progress channel. Resolves when the daemon reports `done`,
 * throws on `error` or timeout.
 */
async function runFfmpegInstall(
  send: (p: OnboardingProgress) => void,
): Promise<void> {
  send({ step: "starting", detail: "Requesting FFmpeg install" });

  const post = await fetch(`${DAEMON_URL}/system/install-ffmpeg`, {
    method: "POST",
  });
  if (!post.ok && post.status !== 202 && post.status !== 409) {
    throw new Error(`Daemon refused install request (HTTP ${post.status})`);
  }
  const initial = (await post.json()) as InstallStatusResponse;
  emitStatus(initial, send);
  if (initial.phase === "done") return;
  if (initial.phase === "error") {
    throw new Error(initial.message ?? "FFmpeg install failed");
  }

  const deadline = Date.now() + POLL_TIMEOUT_MS;
  while (Date.now() < deadline) {
    await new Promise<void>((resolve) => setTimeout(resolve, POLL_INTERVAL_MS));
    const r = await fetch(`${DAEMON_URL}/system/install-ffmpeg/status`);
    if (!r.ok) continue;
    const status = (await r.json()) as InstallStatusResponse;
    emitStatus(status, send);
    if (status.phase === "done") return;
    if (status.phase === "error") {
      throw new Error(status.message ?? "FFmpeg install failed");
    }
  }
  throw new Error("FFmpeg install timed out");
}

function emitStatus(
  status: InstallStatusResponse,
  send: (p: OnboardingProgress) => void,
): void {
  let detail: string | undefined;
  if (status.phase === "downloading") {
    if (typeof status.percent === "number") {
      detail = `${status.percent}%`;
    } else if (
      typeof status.downloadedBytes === "number" &&
      typeof status.totalBytes === "number" &&
      status.totalBytes > 0
    ) {
      const pct = Math.round((status.downloadedBytes / status.totalBytes) * 100);
      detail = `${pct}%`;
    } else if (typeof status.downloadedBytes === "number") {
      detail = `${formatBytes(status.downloadedBytes)} downloaded`;
    }
  } else if (status.phase === "error" && status.message) {
    detail = status.message;
  }
  send({ step: status.phase, detail });
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
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

  // Only ask the daemon for its tool inventory if it's actually up — the
  // request would just time out otherwise and slow down detection.
  const deps = running !== null ? await daemonSystemDeps() : null;

  return {
    platform,
    bundledVersion: bundled,
    installedVersion: installed,
    runningVersion: running,
    daemonReachable: running !== null,
    unitInstalled: unitInstalled_,
    unitEnabled: unitEnabled_,
    unitActive: unitActive_,
    ffmpegAvailable: deps?.ffmpeg === true,
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
