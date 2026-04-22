import type { Tray } from "electron";
import type { MenuContext, Phase, TrayAdapter, TrayAdapterInit } from "./adapter";

const DAEMON_URL = "http://127.0.0.1:3737";
const FAST_POLL_MS = 1000;
const SLOW_POLL_MS = 3000;

/**
 * Shared state + polling loop. Platform-agnostic; delegates rendering
 * to a {@link TrayAdapter}.
 */
export class TrayController {
  private tray: Tray | null = null;
  private pollTimer: NodeJS.Timeout | null = null;
  private phase: Phase = "idle";
  private reachable = true;
  private toggleWindow: () => void;

  constructor(
    private readonly adapter: TrayAdapter,
    private readonly init: TrayAdapterInit,
  ) {
    this.toggleWindow = init.onToggleWindow;
  }

  start(): void {
    this.tray = this.adapter.create(this.init);
    this.rebuildMenu();
    void this.pollStatus();
  }

  destroy(): void {
    if (this.pollTimer) clearTimeout(this.pollTimer);
    this.pollTimer = null;
    if (this.tray) {
      this.adapter.destroy(this.tray);
      this.tray = null;
    }
  }

  private async pollStatus(): Promise<void> {
    try {
      const r = await fetch(`${DAEMON_URL}/status`, {
        signal: AbortSignal.timeout(2000),
      });
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      const data = (await r.json()) as { phase?: string };
      const phase = normalizePhase(data.phase);
      const changed = phase !== this.phase || !this.reachable;
      this.phase = phase;
      this.reachable = true;
      if (changed) this.apply();
    } catch {
      const changed = this.reachable;
      this.reachable = false;
      if (changed) this.apply();
    } finally {
      const next =
        this.phase === "recording" || this.phase === "processing"
          ? FAST_POLL_MS
          : SLOW_POLL_MS;
      this.pollTimer = setTimeout(() => void this.pollStatus(), next);
    }
  }

  private apply(): void {
    if (!this.tray) return;
    this.adapter.applyState(this.tray, {
      phase: this.phase,
      reachable: this.reachable,
    });
    this.rebuildMenu();
  }

  private rebuildMenu(): void {
    if (!this.tray) return;
    const ctx: MenuContext = {
      phase: this.phase,
      reachable: this.reachable,
      onToggleRecording: () => {
        void fetch(`${DAEMON_URL}/toggle`, { method: "POST" }).catch(
          () => undefined,
        );
      },
      onToggleWindow: () => this.toggleWindow(),
    };
    this.adapter.rebuildMenu(this.tray, ctx);
  }
}

function normalizePhase(p: string | undefined): Phase {
  const lower = p?.toLowerCase() ?? "idle";
  if (lower === "recording" || lower === "processing" || lower === "error") {
    return lower;
  }
  return "idle";
}
