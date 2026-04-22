import { makeAutoObservable, reaction, runInAction } from "mobx";
import type { RootStore } from "./root-store";

export type ThemeMode = "system" | "light" | "dark";
export type EffectiveTheme = "light" | "dark";

/**
 * UiStore owns renderer-level presentation preferences. Today that's
 * theme mode only; as we grow (collapsed sidebar, etc.) those land here.
 *
 * Theme application uses a `reaction` to toggle `.dark` on <html>
 * whenever the effective theme changes. We listen to the system
 * prefers-color-scheme media query so "system" mode stays live when
 * the OS flips theme after we mount.
 *
 * Persistence goes through the preload bridge (electron-store). The
 * IPC round-trip is small enough that waiting for the initial read
 * before applying the class is fine — we briefly start in the default
 * "system" mode then realign once the bridge resolves, which matches
 * the current no-flicker default (starts light/dark based on OS).
 */
export class UiStore {
  themeMode: ThemeMode = "system";
  private systemPrefersDark = false;
  private root: RootStore;

  constructor(root: RootStore) {
    this.root = root;
    makeAutoObservable<this, "root" | "systemPrefersDark">(this, {
      root: false,
      systemPrefersDark: true,
    });
  }

  /** Resolved theme — what the UI should actually render as. */
  get effectiveTheme(): EffectiveTheme {
    if (this.themeMode === "light") return "light";
    if (this.themeMode === "dark") return "dark";
    return this.systemPrefersDark ? "dark" : "light";
  }

  /** Called once at app mount. Reads persisted mode, then wires up the
   * media-query listener + the `<html>` class reaction. */
  async start(): Promise<void> {
    // Query system preference immediately so the first paint after
    // hydration uses the right class.
    const mq =
      typeof window !== "undefined" && "matchMedia" in window
        ? window.matchMedia("(prefers-color-scheme: dark)")
        : null;
    const systemDark = mq?.matches ?? false;

    let persistedMode: ThemeMode = "system";
    try {
      persistedMode = (await window.audetic?.getThemeMode()) ?? "system";
    } catch {
      // IPC can fail in non-Electron sandboxes (e.g. if we ever run the
      // renderer through plain `vite dev`). Fall back to "system".
    }

    runInAction(() => {
      this.systemPrefersDark = systemDark;
      this.themeMode = persistedMode;
    });

    // Apply the resolved theme AND re-apply whenever it changes.
    reaction(
      () => this.effectiveTheme,
      (theme) => {
        applyThemeClass(theme);
      },
      { fireImmediately: true },
    );

    // Keep "system" mode live.
    if (mq) {
      const listener = (e: MediaQueryListEvent): void => {
        runInAction(() => {
          this.systemPrefersDark = e.matches;
        });
      };
      if ("addEventListener" in mq) {
        mq.addEventListener("change", listener);
      } else {
        // Safari pre-14 fallback — not strictly needed inside Electron
        // but keeps type-noise down.
        (mq as MediaQueryList & {
          addListener(cb: (e: MediaQueryListEvent) => void): void;
        }).addListener(listener);
      }
    }
  }

  /** Update the persisted preference and re-render. */
  setThemeMode(mode: ThemeMode): void {
    if (mode !== "system" && mode !== "light" && mode !== "dark") return;
    this.themeMode = mode;
    // Fire-and-forget — failure to persist is not worth blocking the UI
    // on, and we don't have a user-visible error channel for this.
    void window.audetic?.setThemeMode(mode);
  }
}

function applyThemeClass(theme: EffectiveTheme): void {
  if (typeof document === "undefined") return;
  const el = document.documentElement;
  if (theme === "dark") el.classList.add("dark");
  else el.classList.remove("dark");
}
