import { Observer } from "mobx-react-lite";
import { Download, AlertCircle } from "lucide-react";
import { Button } from "@/components/ui/button";
import { useStore } from "@/stores/root-store";

/**
 * First-run blocker shown when the daemon reports `ffmpeg: false`.
 * Daemon binary install + systemd setup is handled by `audetic install`
 * before the user lands on the SPA, so this only covers ffmpeg.
 *
 * Renders nothing once the daemon reports ffmpeg is available.
 */
export function OnboardingOverlay() {
  const store = useStore();
  return (
    <Observer>
      {() => {
        if (
          store.onboarding.state !== "needs-ffmpeg" &&
          store.onboarding.installPhase !== "downloading" &&
          store.onboarding.installPhase !== "extracting" &&
          store.onboarding.installPhase !== "starting"
        ) {
          return null;
        }

        const phase = store.onboarding.installPhase;
        const installing =
          phase === "starting" || phase === "downloading" || phase === "extracting";

        return (
          <div className="fixed inset-0 z-50 flex items-center justify-center bg-background/90 backdrop-blur-sm">
            <div className="w-full max-w-md rounded-lg border bg-card p-6 shadow-lg space-y-4">
              <div className="space-y-1">
                <h2 className="text-lg font-semibold">Set up audetic</h2>
                <p className="text-sm text-muted-foreground">
                  audetic needs FFmpeg to compress meeting audio before
                  uploading. We'll install a private copy under
                  <code className="mx-1 rounded bg-muted px-1.5 py-0.5 font-mono text-xs">
                    ~/.local/share/audetic
                  </code>
                  — no system-wide changes.
                </p>
              </div>

              {store.onboarding.installError && (
                <div className="flex items-start gap-2 rounded-md border border-destructive/50 bg-destructive/10 p-3 text-sm">
                  <AlertCircle className="h-4 w-4 shrink-0 text-destructive mt-0.5" />
                  <span className="text-destructive">
                    {store.onboarding.installError}
                  </span>
                </div>
              )}

              {installing && <ProgressBar />}

              <div className="flex items-center justify-end gap-2">
                <Button
                  size="sm"
                  onClick={() => void store.onboarding.installFfmpeg()}
                  disabled={installing}
                >
                  <Download className="mr-1 h-3.5 w-3.5" />
                  {installing ? "Installing…" : "Install FFmpeg"}
                </Button>
              </div>
            </div>
          </div>
        );
      }}
    </Observer>
  );
}

function ProgressBar() {
  const store = useStore();
  return (
    <Observer>
      {() => {
        const phase = store.onboarding.installPhase;
        const pct = store.onboarding.installPercent;
        const label =
          phase === "starting"
            ? "Starting…"
            : phase === "downloading"
              ? `Downloading ${pct ?? 0}%`
              : phase === "extracting"
                ? "Extracting…"
                : "";
        return (
          <div className="space-y-1">
            <div className="text-xs text-muted-foreground">{label}</div>
            <div className="h-1.5 w-full overflow-hidden rounded-full bg-muted">
              <div
                className="h-full bg-primary transition-all"
                style={{
                  width:
                    phase === "downloading" && pct !== null
                      ? `${pct}%`
                      : phase === "extracting"
                        ? "100%"
                        : "10%",
                }}
              />
            </div>
          </div>
        );
      }}
    </Observer>
  );
}
