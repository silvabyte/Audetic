import { BrowserWindow, app, ipcMain, screen, shell } from "electron";
import Store from "electron-store";
import { homedir } from "node:os";
import { join } from "node:path";
import { registerOnboardingIpc } from "./onboarding";
import { destroyTray, initTray } from "./tray";

type ThemeMode = "system" | "light" | "dark";

interface WindowBounds {
  x?: number;
  y?: number;
  width: number;
  height: number;
}

interface PersistedPrefs {
  themeMode?: ThemeMode;
  windowBounds?: WindowBounds;
}

// Persists UI preferences to ~/.config/<app>/config.json. Not to be
// confused with the daemon's config.toml — this is UI shell state only
// (window bounds, theme override). Lives in electron-store's default
// userData location.
const prefsStore = new Store<PersistedPrefs>({
  name: "ui-prefs",
  defaults: {
    themeMode: "system",
    windowBounds: { width: 1100, height: 720 },
  },
});

function resolveConfigPath(): string {
  // Mirrors src/config/mod.rs: $XDG_CONFIG_HOME/audetic/config.toml,
  // falling back to ~/.config/audetic/config.toml.
  const xdg = process.env["XDG_CONFIG_HOME"];
  const base = xdg && xdg.length > 0 ? xdg : join(homedir(), ".config");
  return join(base, "audetic", "config.toml");
}

ipcMain.handle("audetic:openConfigFile", async () => {
  const path = resolveConfigPath();
  // shell.openPath returns "" on success, or an error string.
  return shell.openPath(path);
});

ipcMain.handle("audetic:getThemeMode", (): ThemeMode => {
  return (prefsStore.get("themeMode") as ThemeMode | undefined) ?? "system";
});

ipcMain.handle(
  "audetic:setThemeMode",
  (_event, mode: ThemeMode): void => {
    if (mode !== "system" && mode !== "light" && mode !== "dark") return;
    prefsStore.set("themeMode", mode);
  },
);

const isDev = !app.isPackaged;

// Chrome DevTools Protocol port for chrome-devtools-mcp attach.
// 9333 avoids collision with Chrome/Brave's default 9222.
const CDP_PORT = "9333";

if (isDev) {
  app.commandLine.appendSwitch("remote-debugging-port", CDP_PORT);
}

let mainWindow: BrowserWindow | null = null;
let saveBoundsTimer: ReturnType<typeof setTimeout> | null = null;

function clampBoundsToDisplay(bounds: WindowBounds): WindowBounds {
  // Guard against persisted off-screen placement — e.g. user unplugged
  // a secondary monitor. If the stored position isn't inside any
  // display's work area, drop it and let Electron center the window.
  if (typeof bounds.x !== "number" || typeof bounds.y !== "number") {
    return { width: bounds.width, height: bounds.height };
  }
  const displays = screen.getAllDisplays();
  const insideSomeDisplay = displays.some((d) => {
    const wa = d.workArea;
    const withinX = bounds.x! >= wa.x && bounds.x! < wa.x + wa.width;
    const withinY = bounds.y! >= wa.y && bounds.y! < wa.y + wa.height;
    return withinX && withinY;
  });
  if (!insideSomeDisplay) {
    return { width: bounds.width, height: bounds.height };
  }
  return bounds;
}

function scheduleBoundsSave(): void {
  if (saveBoundsTimer !== null) clearTimeout(saveBoundsTimer);
  saveBoundsTimer = setTimeout(() => {
    saveBoundsTimer = null;
    if (!mainWindow || mainWindow.isDestroyed()) return;
    // Don't overwrite bounds when the window is minimized/hidden — the
    // reported values are meaningless in those states.
    if (mainWindow.isMinimized() || !mainWindow.isVisible()) return;
    const b = mainWindow.getBounds();
    prefsStore.set("windowBounds", {
      x: b.x,
      y: b.y,
      width: b.width,
      height: b.height,
    });
  }, 500);
}

function createWindow(): void {
  const stored =
    (prefsStore.get("windowBounds") as WindowBounds | undefined) ?? {
      width: 1100,
      height: 720,
    };
  const bounds = clampBoundsToDisplay(stored);

  mainWindow = new BrowserWindow({
    x: bounds.x,
    y: bounds.y,
    width: bounds.width,
    height: bounds.height,
    show: false,
    autoHideMenuBar: true,
    webPreferences: {
      preload: join(__dirname, "../preload/index.mjs"),
      sandbox: false,
      contextIsolation: true,
    },
  });

  mainWindow.on("ready-to-show", () => {
    mainWindow?.show();
    if (isDev) mainWindow?.webContents.openDevTools({ mode: "detach" });
  });

  mainWindow.on("move", scheduleBoundsSave);
  mainWindow.on("resize", scheduleBoundsSave);

  mainWindow.on("close", (event) => {
    // Hide to tray instead of quitting on window close.
    if (!(app as unknown as { isQuitting?: boolean }).isQuitting) {
      event.preventDefault();
      mainWindow?.hide();
    }
  });

  mainWindow.on("closed", () => {
    mainWindow = null;
  });

  if (isDev && process.env["ELECTRON_RENDERER_URL"]) {
    mainWindow.loadURL(process.env["ELECTRON_RENDERER_URL"]);
  } else {
    mainWindow.loadFile(join(__dirname, "../renderer/index.html"));
  }
}

function toggleWindow(): void {
  if (!mainWindow) {
    createWindow();
    return;
  }
  if (mainWindow.isVisible()) {
    mainWindow.hide();
  } else {
    mainWindow.show();
    mainWindow.focus();
  }
}

app.whenReady().then(() => {
  createWindow();
  initTray(toggleWindow);
  registerOnboardingIpc(() => mainWindow);

  app.on("activate", () => {
    if (BrowserWindow.getAllWindows().length === 0) createWindow();
    else mainWindow?.show();
  });
});

app.on("before-quit", () => {
  (app as unknown as { isQuitting: boolean }).isQuitting = true;
  destroyTray();
});

// On Linux/Windows we hide to tray on window close instead of quitting, and
// on macOS apps traditionally stay open even with no windows — so this hook
// is a no-op. Quit happens via tray menu or app.before-quit.
app.on("window-all-closed", () => {
  /* intentionally empty */
});
