import { Menu, Tray, nativeImage } from "electron";
import type { MenuContext, Phase, TrayAdapter, TrayAdapterInit } from "./adapter";

/**
 * Linux tray adapter — StatusNotifierItem (SNI) via Electron's Tray.
 *
 * Hyprland has no built-in tray; the icon renders in whichever bar
 * software the user runs (Waybar's `tray` module, swaybar, etc.).
 * Single click activates — we wire that to toggle the window.
 *
 * Icons are full-color 22x22 PNGs. No template-image treatment; SNI
 * consumers honor the pixels as-is.
 */
export class LinuxTrayAdapter implements TrayAdapter {
  private iconPaths!: Record<Phase, string>;

  create(init: TrayAdapterInit): Tray {
    this.iconPaths = init.iconPaths;
    const tray = new Tray(nativeImage.createFromPath(init.iconPaths.idle));
    tray.setToolTip("Audetic");
    tray.on("click", () => init.onToggleWindow());
    return tray;
  }

  applyState(tray: Tray, state: { phase: Phase; reachable: boolean }): void {
    tray.setImage(nativeImage.createFromPath(this.iconPaths[state.phase]));
    const prefix = state.reachable ? "Audetic" : "Audetic (daemon unreachable)";
    tray.setToolTip(`${prefix} — ${state.phase}`);
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
