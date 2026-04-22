import { app, BrowserWindow } from "electron";
import { join } from "node:path";

const isDev = !app.isPackaged;

// Chrome DevTools Protocol port for chrome-devtools-mcp attach.
// 9333 is chosen to avoid collision with Chrome/Brave's default 9222.
const CDP_PORT = "9333";

if (isDev) {
  app.commandLine.appendSwitch("remote-debugging-port", CDP_PORT);
}

function createWindow(): void {
  const win = new BrowserWindow({
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

  win.on("ready-to-show", () => {
    win.show();
    if (isDev) win.webContents.openDevTools({ mode: "detach" });
  });

  if (isDev && process.env["ELECTRON_RENDERER_URL"]) {
    win.loadURL(process.env["ELECTRON_RENDERER_URL"]);
  } else {
    win.loadFile(join(__dirname, "../renderer/index.html"));
  }
}

app.whenReady().then(() => {
  createWindow();

  app.on("activate", () => {
    if (BrowserWindow.getAllWindows().length === 0) createWindow();
  });
});

app.on("window-all-closed", () => {
  if (process.platform !== "darwin") app.quit();
});
