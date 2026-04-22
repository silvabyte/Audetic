import { Menu, Tray, nativeImage } from "electron";
import idleIcon from "../../resources/tray/idle.png?asset";
import recordingIcon from "../../resources/tray/recording.png?asset";
import processingIcon from "../../resources/tray/processing.png?asset";
import errorIcon from "../../resources/tray/error.png?asset";

type Phase = "idle" | "recording" | "processing" | "error";

const DAEMON_URL = "http://127.0.0.1:3737";
const FAST_POLL_MS = 1000;
const SLOW_POLL_MS = 3000;

const iconPaths: Record<Phase, string> = {
  idle: idleIcon,
  recording: recordingIcon,
  processing: processingIcon,
  error: errorIcon,
};

let tray: Tray | null = null;
let pollTimer: NodeJS.Timeout | null = null;
let currentPhase: Phase = "idle";
let reachable = true;
let onToggleWindow: (() => void) | null = null;

export function initTray(toggleWindow: () => void): void {
  onToggleWindow = toggleWindow;

  tray = new Tray(nativeImage.createFromPath(iconPaths.idle));
  tray.setToolTip("Audetic");
  tray.on("click", () => onToggleWindow?.());

  rebuildMenu();
  void pollStatus();
}

export function destroyTray(): void {
  if (pollTimer) clearTimeout(pollTimer);
  pollTimer = null;
  if (tray) {
    tray.destroy();
    tray = null;
  }
}

async function pollStatus(): Promise<void> {
  try {
    const r = await fetch(`${DAEMON_URL}/status`, {
      signal: AbortSignal.timeout(2000),
    });
    if (!r.ok) throw new Error(`HTTP ${r.status}`);
    const data = (await r.json()) as { phase?: string };
    const phase = normalizePhase(data.phase);
    const changed = phase !== currentPhase || !reachable;
    currentPhase = phase;
    reachable = true;
    if (changed) applyPhase();
  } catch {
    const changed = reachable;
    reachable = false;
    if (changed) applyPhase();
  } finally {
    const next =
      currentPhase === "recording" || currentPhase === "processing"
        ? FAST_POLL_MS
        : SLOW_POLL_MS;
    pollTimer = setTimeout(() => {
      void pollStatus();
    }, next);
  }
}

function applyPhase(): void {
  if (!tray) return;
  tray.setImage(nativeImage.createFromPath(iconPaths[currentPhase]));
  const prefix = reachable ? "Audetic" : "Audetic (daemon unreachable)";
  tray.setToolTip(`${prefix} — ${currentPhase}`);
  rebuildMenu();
}

function rebuildMenu(): void {
  if (!tray) return;
  const canToggle = reachable;
  tray.setContextMenu(
    Menu.buildFromTemplate([
      {
        label: reachable
          ? `Status: ${currentPhase}`
          : "Daemon unreachable (127.0.0.1:3737)",
        enabled: false,
      },
      { type: "separator" },
      {
        label: currentPhase === "recording" ? "Stop recording" : "Toggle recording",
        enabled: canToggle,
        click: () => {
          void fetch(`${DAEMON_URL}/toggle`, { method: "POST" }).catch(
            () => undefined,
          );
        },
      },
      {
        label: "Show / hide window",
        click: () => onToggleWindow?.(),
      },
      { type: "separator" },
      { role: "quit", label: "Quit Audetic" },
    ]),
  );
}

function normalizePhase(p: string | undefined): Phase {
  const lower = p?.toLowerCase() ?? "idle";
  if (lower === "recording" || lower === "processing" || lower === "error")
    return lower;
  return "idle";
}
