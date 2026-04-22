import { Menu, Tray, nativeImage } from "electron";
import type { MenuContext, Phase, TrayAdapter, TrayAdapterInit } from "./adapter";

/**
 * macOS menu-bar adapter — STUB.
 *
 * Not fully implemented in Phase 1. This file is the landing pad for
 * Mac-specific work in Phase 5 (or whenever we first smoke-test on the
 * MacBook). Current behavior mirrors Linux so the dev loop still works
 * if someone runs `make ui-dev` on macOS.
 *
 * TODO (Phase 5):
 *   - Replace the 22x22 color PNGs with template images: solid black +
 *     alpha, suffixed `-Template` so macOS auto-inverts them for light
 *     and dark menu bars. Generate during the icon render step.
 *     nativeImage.setTemplateImage(true) for each.
 *   - macOS convention is that single-click opens the menu (no separate
 *     activate event). Drop the `click -> onToggleWindow` handler and
 *     move "Show / hide window" higher in the menu.
 *   - Hide the Dock icon when the window is closed: call
 *     `app.dock.hide()` on window close and `app.dock.show()` when the
 *     user picks "Show window" from the menu.
 *   - Accessibility role items: use `role: "about"` etc. where
 *     appropriate; macOS renders these with the standard keybindings.
 *   - Consider a title-bar `tray.setTitle(...)` showing the current
 *     phase text next to the icon (e.g. "●REC") during active states.
 */
export class MacosTrayAdapter implements TrayAdapter {
  private iconPaths!: Record<Phase, string>;

  create(init: TrayAdapterInit): Tray {
    this.iconPaths = init.iconPaths;
    const image = nativeImage.createFromPath(init.iconPaths.idle);
    // TODO: image.setTemplateImage(true) once we have template assets.
    const tray = new Tray(image);
    tray.setToolTip("Audetic");
    tray.on("click", () => init.onToggleWindow());
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
