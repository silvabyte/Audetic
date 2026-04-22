import idleIcon from "../../../resources/tray/idle.png?asset";
import recordingIcon from "../../../resources/tray/recording.png?asset";
import processingIcon from "../../../resources/tray/processing.png?asset";
import errorIcon from "../../../resources/tray/error.png?asset";
import type { Phase, TrayAdapter } from "./adapter";
import { TrayController } from "./controller";
import { LinuxTrayAdapter } from "./linux";
import { MacosTrayAdapter } from "./macos";
import { WindowsTrayAdapter } from "./windows";

const iconPaths: Record<Phase, string> = {
  idle: idleIcon,
  recording: recordingIcon,
  processing: processingIcon,
  error: errorIcon,
};

let controller: TrayController | null = null;

/**
 * Construct the right tray adapter for the current platform. Adapters
 * live under ./{linux,macos,windows}.ts and document their
 * platform-specific TODOs inline.
 */
function pickAdapter(): TrayAdapter {
  switch (process.platform) {
    case "darwin":
      return new MacosTrayAdapter();
    case "win32":
      return new WindowsTrayAdapter();
    case "linux":
      return new LinuxTrayAdapter();
    default:
      // Unknown platform — fall back to the Linux adapter, which uses only
      // portable Electron Tray APIs. Better than crashing; may not render.
      return new LinuxTrayAdapter();
  }
}

export function initTray(onToggleWindow: () => void): void {
  if (controller) return;
  const adapter = pickAdapter();
  controller = new TrayController(adapter, { iconPaths, onToggleWindow });
  controller.start();
}

export function destroyTray(): void {
  if (!controller) return;
  controller.destroy();
  controller = null;
}
