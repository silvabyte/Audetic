import { realpathSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { isAbsolute, join, resolve, sep } from "node:path";

export function repoRoot(): string {
  return resolve(import.meta.dir, "../..");
}

export function migrationDir(): string {
  return join(repoRoot(), "crates/audetic/src/db/migrations");
}

export function defaultBinary(): string {
  return join(repoRoot(), "target/debug/audeticd");
}

/**
 * Directories the installed daemon would open for the account that launched
 * the suite. The suite refuses to read or write these.
 */
export function liveDatabasePaths(env: NodeJS.ProcessEnv = process.env): string[] {
  const paths: string[] = [];
  const home = env.HOME && env.HOME.length > 0 ? env.HOME : homedir();
  if (home) {
    paths.push(join(home, "Library", "Application Support", "audetic", "audetic.db"));
    paths.push(join(home, ".local", "share", "audetic", "audetic.db"));
  }
  if (env.XDG_DATA_HOME && env.XDG_DATA_HOME.length > 0) {
    paths.push(join(env.XDG_DATA_HOME, "audetic", "audetic.db"));
  }
  return paths;
}

export function assertSafeDatabase(dbPath: string, env: NodeJS.ProcessEnv = process.env): void {
  const resolved = resolve(dbPath);
  for (const live of liveDatabasePaths(env)) {
    if (resolved === resolve(live)) {
      throw new Error(`Refusing to use the live Audetic database at ${resolved}`);
    }
  }
}

export function assertInside(root: string, path: string): void {
  const rootReal = realpathSync(root);
  const pathReal = realpathSync(path);
  const prefix = rootReal.endsWith(sep) ? rootReal : rootReal + sep;
  if (pathReal !== rootReal && !pathReal.startsWith(prefix)) {
    throw new Error(`${pathReal} is outside the temp root ${rootReal}`);
  }
}

/**
 * Data directory the daemon will choose for a temp home. This matches the
 * `dirs` crate: XDG on Linux, Application Support on macOS.
 */
export function daemonDataDir(home: string, xdgDataHome: string): string {
  if (process.platform === "darwin") {
    return join(home, "Library", "Application Support", "audetic");
  }
  return join(xdgDataHome, "audetic");
}

export function isLoopbackUrl(value: string): boolean {
  try {
    const url = new URL(value);
    return (
      (url.protocol === "http:" || url.protocol === "https:") &&
      (url.hostname === "127.0.0.1" || url.hostname === "localhost") &&
      isAbsolute(url.pathname)
    );
  } catch {
    return false;
  }
}

export function tempRootParent(): string {
  return tmpdir();
}
