import { app } from "electron";
import { homedir } from "node:os";
import { existsSync } from "node:fs";
import { join } from "node:path";

/**
 * Centralizes every filesystem path the onboarding flow touches, so the rest
 * of the install code doesn't carry "is this dev? is this packaged?" branches.
 *
 * Paths come in two flavors:
 *
 *   bundled.* — read-only. Lives inside the app bundle. In production this is
 *               under `process.resourcesPath`; in dev it's the working tree.
 *   installed.* — read-write. Where the onboarding flow copies things on the
 *               user's machine. User-local; no sudo needed.
 */

export interface InstallPaths {
  /** Bundled binary that ships in the AppImage / DMG. */
  bundledBinary: string;
  /** Bundled systemd unit template. */
  bundledServiceTemplate: string;
  /** Where we copy the daemon binary on install. */
  installedBinary: string;
  /** Directory we own under the user's data dir. */
  installedDir: string;
  /** Location of the systemd user unit on Linux. */
  systemdUnit: string;
  /** Location of the launchd plist on macOS (phase 7). */
  launchdPlist: string;
}

export function resolveInstallPaths(): InstallPaths {
  const arch = nativeArch();
  const home = homedir();

  // In packaged builds, electron-builder unpacks `extraResources` under
  // `process.resourcesPath`. In dev (electron-vite) the bundled binary lives
  // at apps/ui/resources/bin/audetic-${arch} relative to the workspace.
  const isPackaged = app.isPackaged;
  const resourcesRoot = isPackaged
    ? process.resourcesPath
    : devResourcesRoot();

  const installedDir = join(home, ".local", "share", "audetic", "bin");

  return {
    bundledBinary: join(resourcesRoot, "bin", `audetic-${arch}`),
    bundledServiceTemplate: join(resourcesRoot, "audetic.service.tmpl"),
    installedBinary: join(installedDir, "audetic"),
    installedDir,
    systemdUnit: join(home, ".config", "systemd", "user", "audetic.service"),
    launchdPlist: join(
      home,
      "Library",
      "LaunchAgents",
      "com.audetic.daemon.plist",
    ),
  };
}

function nativeArch(): "x64" | "arm64" {
  // electron's process.arch returns x64 / arm64 directly — same labels we
  // use when staging via build-daemon.ts.
  if (process.arch === "x64" || process.arch === "arm64") return process.arch;
  // Fall back to x64 for unknown arches; build-daemon.ts won't have produced
  // an artifact for them so install will fail at copy-time with a clear
  // message anyway.
  return "x64";
}

function devResourcesRoot(): string {
  // electron-vite leaves the main process running from
  // apps/ui/out/main/index.js (production-like) OR straight from the source
  // path under bun-run-dev. Walk up until we find resources/.
  // __dirname during dev is apps/ui/out/main; resources/ is two up.
  const candidates = [
    join(__dirname, "..", "..", "resources"),
    join(__dirname, "..", "..", "..", "resources"),
    join(process.cwd(), "apps", "ui", "resources"),
    join(process.cwd(), "resources"),
  ];
  for (const c of candidates) {
    if (existsSync(c)) return c;
  }
  // Last-ditch: return the most-likely path; fs operations will surface a
  // clear error if it's wrong.
  return candidates[0];
}
