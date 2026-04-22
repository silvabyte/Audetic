import type { Tray } from "electron";

/**
 * Phase values we mirror from the daemon's /status response. Kept in sync
 * with crates/audetic/src/audio/recording_machine.rs::RecordingPhase.
 */
export type Phase = "idle" | "recording" | "processing" | "error";

/** Input for rebuilding the context menu on a state change. */
export interface MenuContext {
  phase: Phase;
  reachable: boolean;
  /** POST /toggle and nudge the renderer. */
  onToggleRecording: () => void;
  /** Show/hide the main window. */
  onToggleWindow: () => void;
}

/** Everything a platform adapter needs at construction time. */
export interface TrayAdapterInit {
  /** Per-phase pre-rendered PNG paths, resolved via electron-vite `?asset`. */
  iconPaths: Record<Phase, string>;
  /** Invoked when the adapter decides the user clicked to show/hide the window. */
  onToggleWindow: () => void;
}

/**
 * Platform-specific tray behavior. The shared controller owns state +
 * polling; each adapter decides how to render and what click events mean
 * on its platform.
 *
 * Platforms differ in three axes:
 *   1. **Icon semantics.** macOS wants a template image (black + alpha),
 *      auto-inverted for light/dark menu bars. Linux/Windows want full-color
 *      PNGs.
 *   2. **Click behavior.** macOS opens the menu on a single click, so
 *      "toggle window" lives inside the menu. Linux/Windows typically
 *      treat single-click as "activate", which we wire to toggle the window.
 *   3. **Menu conventions.** macOS uses template role items ("quit",
 *      "about"). Linux/Windows use explicit labels.
 */
export interface TrayAdapter {
  /** Build the platform-native Tray and hook up click handlers. */
  create(init: TrayAdapterInit): Tray;

  /** Apply a new state snapshot (icon + tooltip). Called on every change. */
  applyState(tray: Tray, state: { phase: Phase; reachable: boolean }): void;

  /** Rebuild the context menu after a state change. */
  rebuildMenu(tray: Tray, ctx: MenuContext): void;

  /** Tear down any adapter-specific resources. */
  destroy(tray: Tray): void;
}
