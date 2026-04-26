import { copyFile, mkdir, readFile, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { dirname } from "node:path";
import type { InstallPaths } from "./paths";
import { run, runStreaming } from "./exec";

export type ProgressCallback = (
  step: string,
  detail?: string,
) => void;

/** True if `~/.config/systemd/user/audetic.service` exists. */
export async function unitInstalled(paths: InstallPaths): Promise<boolean> {
  return existsSync(paths.systemdUnit);
}

/** True if systemd reports the unit as enabled. */
export async function unitEnabled(): Promise<boolean> {
  const r = await run("systemctl", ["--user", "is-enabled", "audetic.service"]);
  return r.code === 0 && r.stdout.trim() === "enabled";
}

/** True if systemd reports the unit as active. */
export async function unitActive(): Promise<boolean> {
  const r = await run("systemctl", ["--user", "is-active", "audetic.service"]);
  return r.code === 0 && r.stdout.trim() === "active";
}

/**
 * Bootstrap install on Linux:
 *   1. mkdir -p ~/.local/share/audetic/bin
 *   2. copy bundled binary -> ~/.local/share/audetic/bin/audetic (chmod +x)
 *   3. render service template -> ~/.config/systemd/user/audetic.service
 *   4. systemctl --user daemon-reload
 *   5. systemctl --user enable --now audetic.service
 *
 * Streams progress via `onProgress`; callers wire that through IPC to the
 * renderer's onboarding card.
 */
export async function installService(
  paths: InstallPaths,
  onProgress: ProgressCallback,
): Promise<void> {
  onProgress("copy-binary", `Copying daemon binary to ${paths.installedBinary}`);
  await mkdir(paths.installedDir, { recursive: true });
  if (!existsSync(paths.bundledBinary)) {
    throw new Error(
      `Bundled daemon binary not found at ${paths.bundledBinary}. ` +
        `If you're running in dev, run \`bun apps/ui/scripts/build-daemon.ts\` first.`,
    );
  }
  await copyFile(paths.bundledBinary, paths.installedBinary);
  // Set executable bit. copyFile preserves perms, but the staged binary
  // could land without +x in some pipelines (e.g. cross-platform CI).
  const { chmod } = await import("node:fs/promises");
  await chmod(paths.installedBinary, 0o755);

  onProgress("render-unit", `Writing ${paths.systemdUnit}`);
  await mkdir(dirname(paths.systemdUnit), { recursive: true });
  const tmpl = await readFile(paths.bundledServiceTemplate, "utf8");
  const unit = tmpl.replace("__EXEC_START__", paths.installedBinary);
  await writeFile(paths.systemdUnit, unit);

  onProgress("daemon-reload", "systemctl --user daemon-reload");
  const reload = await run("systemctl", ["--user", "daemon-reload"]);
  if (reload.code !== 0) {
    throw new Error(
      `systemctl daemon-reload failed (code ${reload.code}): ${reload.stderr}`,
    );
  }

  onProgress("enable-start", "systemctl --user enable --now audetic.service");
  const enable = await runStreaming(
    "systemctl",
    ["--user", "enable", "--now", "audetic.service"],
    (stream, line) => {
      if (line.trim().length > 0) {
        onProgress("enable-start", `[${stream}] ${line}`);
      }
    },
  );
  if (enable !== 0) {
    throw new Error(`systemctl enable --now failed (code ${enable})`);
  }

  onProgress("done", "Daemon service started");
}

/** Stop and disable the unit. Used by the future "uninstall daemon" flow. */
export async function uninstallService(): Promise<void> {
  await run("systemctl", ["--user", "disable", "--now", "audetic.service"]);
}

/** Replace the installed binary in place; called when the bundled version
 *  is newer than what's on disk. The unit gets restarted to pick up the
 *  new binary. */
export async function updateBinary(
  paths: InstallPaths,
  onProgress: ProgressCallback,
): Promise<void> {
  onProgress("copy-binary", `Replacing ${paths.installedBinary}`);
  if (!existsSync(paths.bundledBinary)) {
    throw new Error(
      `Bundled daemon binary not found at ${paths.bundledBinary}.`,
    );
  }
  await copyFile(paths.bundledBinary, paths.installedBinary);
  const { chmod } = await import("node:fs/promises");
  await chmod(paths.installedBinary, 0o755);

  onProgress("restart", "systemctl --user restart audetic.service");
  const r = await run("systemctl", ["--user", "restart", "audetic.service"]);
  if (r.code !== 0) {
    throw new Error(`systemctl restart failed (code ${r.code}): ${r.stderr}`);
  }
  onProgress("done", "Daemon restarted with the new binary");
}
