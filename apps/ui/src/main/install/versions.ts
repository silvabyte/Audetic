import { existsSync } from "node:fs";
import { run } from "./exec";

const DAEMON_URL = "http://127.0.0.1:3737";

/**
 * Pull the version string out of `<binary> version`. The daemon's CLI uses
 * a dedicated `version` subcommand (not the usual `--version` flag) and
 * prints "Audetic 0.1.21". Used to decide whether the installed binary
 * needs an in-place refresh against the bundle's copy.
 */
export async function binaryVersion(path: string): Promise<string | null> {
  if (!existsSync(path)) return null;
  const r = await run(path, ["version"]);
  if (r.code !== 0) return null;
  const m = r.stdout.match(/(\d+\.\d+\.\d+(?:[-+][\w.]+)?)/);
  return m ? m[1] : null;
}

/**
 * Ask the running daemon for its version via the HTTP API. Returns null if
 * unreachable. Useful when the binary path on disk is stale (e.g. the user
 * is still running a previous install) but the running process is fine.
 */
export async function runningDaemonVersion(): Promise<string | null> {
  try {
    const r = await fetch(`${DAEMON_URL}/version`, {
      signal: AbortSignal.timeout(1500),
    });
    if (!r.ok) return null;
    const data = (await r.json()) as { version?: string };
    return data.version ?? null;
  } catch {
    return null;
  }
}

export interface DaemonSystemDeps {
  ffmpeg: boolean;
}

/**
 * Ask the daemon which external tools it sees on PATH. The daemon is the
 * source of truth: it runs the ffmpeg binary at compress time, so its view
 * of PATH (under systemd user env) is what actually matters — querying
 * `which ffmpeg` from the Electron process can disagree.
 *
 * Returns null if the daemon is unreachable (in which case onboarding will
 * already be steering the user toward bringing the daemon up first).
 */
export async function daemonSystemDeps(): Promise<DaemonSystemDeps | null> {
  try {
    const r = await fetch(`${DAEMON_URL}/system/deps`, {
      signal: AbortSignal.timeout(1500),
    });
    if (!r.ok) return null;
    const data = (await r.json()) as Partial<DaemonSystemDeps>;
    return { ffmpeg: data.ffmpeg === true };
  } catch {
    return null;
  }
}
