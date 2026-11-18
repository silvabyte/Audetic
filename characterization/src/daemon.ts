import { closeSync, mkdirSync, openSync, readFileSync, statSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { type FixtureKind, preUpgradeBackupPath, writeFixture } from "./fixture.ts";
import {
  assertInside,
  assertSafeDatabase,
  daemonDataDir,
  defaultBinary,
  isLoopbackUrl,
} from "./paths.ts";

export interface DaemonSession {
  readonly kind: FixtureKind;
  readonly baseUrl: string;
  readonly ingressPort: number | null;
  readonly dbPath: string;
  readonly backupPath: string;
  readonly root: string;
  /** True when this process spawned the binary. URL mode does not. */
  readonly spawned: boolean;
  stop(): Promise<void>;
  stderr(): string;
}

/**
 * Start today's daemon against a temp database, or attach to one that is
 * already running.
 *
 * Spawn mode (CI, and the default):
 *   `AUDETIC_BIN` is the executable. It defaults to `target/debug/audeticd`.
 *   The process gets a private `HOME` and XDG directories, an ephemeral
 *   `AUDETIC_PORT`, and a migration fixture that is never the live file.
 *
 * URL mode:
 *   `AUDETIC_URL` is the origin (`http://127.0.0.1:port`). The suite does not
 *   spawn. `AUDETIC_DB` must be the database that server opened, and it must
 *   not be the live Audetic file. The caller prepared that file; these tests
 *   still assert the same HTTP and checksum contract.
 */
export async function startDaemon(kind: FixtureKind): Promise<DaemonSession> {
  const url = process.env.AUDETIC_URL?.trim();
  if (url) {
    return attach(kind, url);
  }
  return spawn(kind);
}

async function attach(kind: FixtureKind, url: string): Promise<DaemonSession> {
  if (!isLoopbackUrl(url)) {
    throw new Error(`AUDETIC_URL must be an http(s) loopback origin, got ${url}`);
  }
  const dbPath = process.env.AUDETIC_DB?.trim();
  if (!dbPath) {
    throw new Error(
      "AUDETIC_DB is required when AUDETIC_URL is set, and it must be a temp copy rather than the live database",
    );
  }
  assertSafeDatabase(dbPath);
  const baseUrl = url.replace(/\/$/, "");
  return {
    kind,
    baseUrl,
    ingressPort: null,
    dbPath,
    backupPath: preUpgradeBackupPath(dbPath),
    root: dbPath,
    spawned: false,
    async stop() {},
    stderr: () => "",
  };
}

async function spawn(kind: FixtureKind): Promise<DaemonSession> {
  const binary = process.env.AUDETIC_BIN?.trim() || defaultBinary();
  try {
    if (!statSync(binary).isFile()) {
      throw new Error("not a file");
    }
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    throw new Error(
      `Daemon binary not found at ${binary} (${detail}). Build it with \`cargo build -p audetic --bin audeticd\` or set AUDETIC_BIN.`,
    );
  }

  const root = join(tmpdir(), `audetic-characterization-${crypto.randomUUID()}`);
  const home = join(root, "home");
  const xdgData = join(root, "xdg-data");
  const xdgConfig = join(root, "xdg-config");
  const xdgState = join(root, "xdg-state");
  const xdgCache = join(root, "xdg-cache");
  mkdirSync(home, { recursive: true });
  mkdirSync(xdgData, { recursive: true });
  mkdirSync(xdgConfig, { recursive: true });
  mkdirSync(xdgState, { recursive: true });
  mkdirSync(xdgCache, { recursive: true });

  const dataDir = daemonDataDir(home, xdgData);
  const fixture = writeFixture(dataDir, kind);
  assertSafeDatabase(fixture.dbPath);
  assertInside(root, fixture.dbPath);

  const [apiPort, ingressPort] = await allocatePorts();

  const logPath = join(root, "daemon.log");
  const logFd = openSync(logPath, "w");
  const childEnv: Record<string, string> = {};
  for (const [key, value] of Object.entries(process.env)) {
    if (value !== undefined) childEnv[key] = value;
  }
  childEnv.HOME = home;
  childEnv.XDG_DATA_HOME = xdgData;
  childEnv.XDG_CONFIG_HOME = xdgConfig;
  childEnv.XDG_STATE_HOME = xdgState;
  childEnv.XDG_CACHE_HOME = xdgCache;
  childEnv.AUDETIC_PORT = String(apiPort);
  childEnv.AUDETIC_INGRESS_PORT = String(ingressPort);
  delete childEnv.AUDETIC_URL;
  delete childEnv.AUDETIC_DB;

  const proc = Bun.spawn([binary], {
    cwd: root,
    env: childEnv,
    stdin: "ignore",
    stdout: logFd,
    stderr: logFd,
  });

  const baseUrl = `http://127.0.0.1:${apiPort}`;
  let stopped = false;
  const session: DaemonSession = {
    kind,
    baseUrl,
    ingressPort,
    dbPath: fixture.dbPath,
    backupPath: preUpgradeBackupPath(fixture.dbPath),
    root,
    spawned: true,
    stderr: () => {
      try {
        return readFileSync(logPath, "utf8");
      } catch {
        return "";
      }
    },
    async stop() {
      if (stopped) return;
      stopped = true;
      if (proc.exitCode === null) {
        proc.kill("SIGTERM");
        const exited = await Promise.race([
          proc.exited.then(() => true),
          Bun.sleep(2_000).then(() => false),
        ]);
        if (!exited && proc.exitCode === null) {
          proc.kill("SIGKILL");
          await proc.exited;
        }
      }
      closeSync(logFd);
    },
  };

  try {
    await waitUntilReady(session, proc);
  } catch (error) {
    await session.stop();
    throw error;
  }
  return session;
}

async function waitUntilReady(
  session: DaemonSession,
  proc: ReturnType<typeof Bun.spawn>,
): Promise<void> {
  const deadline = Date.now() + 30_000;
  let last = "no response";
  while (Date.now() < deadline) {
    if (proc.exitCode !== null) {
      throw new Error(
        `audeticd exited ${proc.exitCode} before serving HTTP\n${session.stderr()}`,
      );
    }
    try {
      const response = await fetch(`${session.baseUrl}/api/version`, {
        signal: AbortSignal.timeout(1_000),
      });
      if (response.ok) return;
      last = `HTTP ${response.status}`;
    } catch (error) {
      last = error instanceof Error ? error.message : String(error);
    }
    await Bun.sleep(100);
  }
  throw new Error(`audeticd did not become ready (${last})\n${session.stderr()}`);
}

async function allocatePorts(): Promise<[number, number]> {
  const reserved = new Set([3737, 3739]);
  for (let attempt = 0; attempt < 10; attempt++) {
    const api = await freePort();
    const ingress = await freePort();
    if (api !== ingress && !reserved.has(api) && !reserved.has(ingress)) {
      return [api, ingress];
    }
  }
  throw new Error("Could not allocate loopback ports away from the installed daemon");
}

function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      if (address === null || typeof address === "string") {
        reject(new Error("could not allocate a loopback port"));
        return;
      }
      const { port } = address;
      server.close((error) => (error ? reject(error) : resolve(port)));
    });
  });
}
