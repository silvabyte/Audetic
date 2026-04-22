import { Observer } from "mobx-react-lite";
import { PlugZap } from "lucide-react";
import { useStore } from "@/stores/root-store";

/**
 * Sticky banner shown whenever the daemon has been confirmed unreachable.
 * Suppressed on boot (before the first poll completes) to avoid flicker.
 */
export function DaemonDownBanner() {
  const store = useStore();
  return (
    <Observer>
      {() => {
        if (store.daemonReachable) return null;

        const startCmd =
          window.audetic?.platform === "darwin"
            ? "launchctl start com.audetic.daemon # or run `audetic` in a terminal"
            : "systemctl --user start audetic.service";

        return (
          <div className="w-full border-b border-destructive/40 bg-destructive/10 text-destructive">
            <div className="mx-auto max-w-3xl px-4 py-3 flex items-start gap-3">
              <PlugZap className="h-5 w-5 shrink-0 mt-0.5" />
              <div className="space-y-1">
                <div className="text-sm font-medium">
                  Can't reach the Audetic daemon at 127.0.0.1:3737.
                </div>
                <code className="block rounded bg-background/60 px-2 py-1 text-xs text-foreground">
                  {startCmd}
                </code>
              </div>
            </div>
          </div>
        );
      }}
    </Observer>
  );
}
