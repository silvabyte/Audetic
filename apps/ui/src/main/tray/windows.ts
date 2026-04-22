import { Menu, Tray, nativeImage } from "electron";
import type { MenuContext, Phase, TrayAdapter, TrayAdapterInit } from "./adapter";

/**
 * Windows system-tray adapter — STUB.
 *
 * Windows isn't a target platform for Audetic right now, but this file
 * keeps the door open and the selection logic in `index.ts` exhaustive.
 * Current behavior mirrors Linux so `make ui-dev` doesn't break if
 * someone happens to run the repo on Windows.
 *
 * TODO (if/when Windows support is ever in scope):
 *   - Prefer 16x16 (or 32x32 for high-DPI) ICO/PNG tray icons; 22x22
 *     shows blurry on the taskbar. Regenerate during icon render.
 *   - Single-click activates, double-click is historically "open main
 *     window"; map both to `onToggleWindow`. Right-click opens the menu.
 *   - `tray.displayBalloon({ ... })` for transient notifications
 *     (transcription complete, etc.) — consider wiring in Phase 5.
 *   - Windows runs a whole "hidden icons" overflow flyout. Users may
 *     need to drag our icon into the always-visible region manually.
 *     Document in install instructions rather than trying to automate.
 *   - Jump lists + taskbar thumbnail button bar are out of scope for v1
 *     but are the usual next step if we grow Windows-specific UX.
 */
export class WindowsTrayAdapter implements TrayAdapter {
  private iconPaths!: Record<Phase, string>;

  create(init: TrayAdapterInit): Tray {
    this.iconPaths = init.iconPaths;
    const tray = new Tray(nativeImage.createFromPath(init.iconPaths.idle));
    tray.setToolTip("Audetic");
    tray.on("click", () => init.onToggleWindow());
    tray.on("double-click", () => init.onToggleWindow());
    return tray;
  }

  applyState(tray: Tray, state: { phase: Phase; reachable: boolean }): void {
    tray.setImage(nativeImage.createFromPath(this.iconPaths[state.phase]));
    tray.setToolTip(
      state.reachable ? `Audetic — ${state.phase}` : "Audetic (daemon unreachable)",
    );
  }

  rebuildMenu(tray: Tray, ctx: MenuContext): void {
    tray.setContextMenu(
      Menu.buildFromTemplate([
        {
          label: ctx.reachable
            ? `Status: ${ctx.phase}`
            : "Daemon unreachable (127.0.0.1:3737)",
          enabled: false,
        },
        { type: "separator" },
        {
          label: ctx.phase === "recording" ? "Stop recording" : "Toggle recording",
          enabled: ctx.reachable,
          click: () => ctx.onToggleRecording(),
        },
        {
          label: "Show / hide window",
          click: () => ctx.onToggleWindow(),
        },
        { type: "separator" },
        { role: "quit", label: "Quit Audetic" },
      ]),
    );
  }

  destroy(tray: Tray): void {
    tray.destroy();
  }
}
