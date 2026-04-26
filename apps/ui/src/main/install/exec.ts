import { execFile, spawn } from "node:child_process";
import { promisify } from "node:util";

const execFileP = promisify(execFile);

export interface ExecResult {
  stdout: string;
  stderr: string;
  code: number;
}

/**
 * Run a process and capture stdout/stderr. Resolves regardless of exit code
 * — callers inspect `code` to decide. Rejects only if the binary itself
 * isn't found.
 *
 * `execFile` (no shell) is used so that user-supplied strings (e.g. binary
 * paths from preferences) can never trigger shell-metachar interpretation.
 */
export async function run(
  cmd: string,
  args: string[],
): Promise<ExecResult> {
  try {
    const { stdout, stderr } = await execFileP(cmd, args);
    return { stdout, stderr, code: 0 };
  } catch (err) {
    const e = err as NodeJS.ErrnoException & {
      stdout?: string;
      stderr?: string;
      code?: number | string;
    };
    if (e.code === "ENOENT") throw err;
    return {
      stdout: e.stdout ?? "",
      stderr: e.stderr ?? "",
      code: typeof e.code === "number" ? e.code : 1,
    };
  }
}

/** Same as run() but streams stdout/stderr line-by-line through a callback
 * — used by the install flow to surface progress to the renderer. */
export async function runStreaming(
  cmd: string,
  args: string[],
  onLine: (stream: "stdout" | "stderr", line: string) => void,
): Promise<number> {
  return new Promise((resolve, reject) => {
    const child = spawn(cmd, args);
    forward(child.stdout, "stdout", onLine);
    forward(child.stderr, "stderr", onLine);
    child.once("error", reject);
    child.once("close", (code) => resolve(code ?? 0));
  });
}

function forward(
  stream: NodeJS.ReadableStream,
  label: "stdout" | "stderr",
  onLine: (s: "stdout" | "stderr", line: string) => void,
): void {
  let buf = "";
  stream.setEncoding("utf8");
  stream.on("data", (chunk: string) => {
    buf += chunk;
    let nl = buf.indexOf("\n");
    while (nl !== -1) {
      onLine(label, buf.slice(0, nl));
      buf = buf.slice(nl + 1);
      nl = buf.indexOf("\n");
    }
  });
  stream.on("end", () => {
    if (buf.length > 0) onLine(label, buf);
  });
}
