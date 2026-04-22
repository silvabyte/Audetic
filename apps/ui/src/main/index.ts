import { BrowserWindow, app } from "electron";
import { join } from "node:path";
import { destroyTray, initTray } from "./tray";

const isDev = !app.isPackaged;

// Chrome DevTools Protocol port for chrome-devtools-mcp attach.
// 9333 avoids collision with Chrome/Brave's default 9222.
const CDP_PORT = "9333";

if (isDev) {
  app.commandLine.appendSwitch("remote-debugging-port", CDP_PORT);
}

let mainWindow: BrowserWindow | null = null;

function createWindow(): void {
  mainWindow = new BrowserWindow({
    width: 1100,
    height: 720,
    show: false,
    autoHideMenuBar: true,
    webPreferences: {
      preload: join(__dirname, "../preload/index.js"),
      sandbox: false,
      contextIsolation: true,
    },
  });

  mainWindow.on("ready-to-show", () => {
    mainWindow?.show();
    if (isDev) mainWindow?.webContents.openDevTools({ mode: "detach" });
  });

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
