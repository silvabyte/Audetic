import { Observer } from "mobx-react-lite";
import { Download, PlayCircle, RefreshCcw, Settings2, Loader2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { useStore } from "@/stores/root-store";
import { cn } from "@/lib/utils";

/**
 * Onboarding entry point. Replaces the daemon-down banner whenever the
 * install state isn't "happy" — we know more than just "daemon is gone",
 * so we can take the user directly into install / enable / start / update.
 *
 * Renders nothing on the happy path so existing routes aren't pushed down.
 */
export function OnboardingCard() {
  const store = useStore();
  return (
    <Observer>
      {() => {
        const decision = store.install.decision;
        if (decision.kind === "happy" || decision.kind === "unknown") return null;

        return (
          <div className="border-b bg-card">
            <div className="mx-auto max-w-3xl px-6 py-5">
              <Card>
                <CardHeader>
                  <CardTitle className="text-base flex items-center gap-2">
                    {iconForDecision(decision.kind)}
                    {titleForDecision(decision.kind)}
                  </CardTitle>
                  <CardDescription>
                    {descriptionForDecision(decision.kind, decision)}
                  </CardDescription>
                </CardHeader>
                <CardContent className="space-y-3">
                  <ActionRow />
                  <ProgressLine />
                  <ErrorLine />
                </CardContent>
              </Card>
            </div>
          </div>
        );
      }}
    </Observer>
  );
}

function ActionRow() {
  const store = useStore();
  return (
    <Observer>
      {() => {
        const decision = store.install.decision;
        const running = store.install.installStatus === "running";

        let label = "Install";
        let onClick = (): void => {
          void store.install.install();
        };
        let Icon = Download;
        switch (decision.kind) {
          case "install":
            label = running ? "Installing…" : "Install Audetic daemon";
            Icon = Download;
            onClick = (): void => {
              void store.install.install();
            };
            break;
          case "enable":
            label = running ? "Enabling…" : "Enable Audetic daemon";
            Icon = Settings2;
            onClick = (): void => {
              void store.install.enable();
            };
            break;
          case "start":
            label = running ? "Starting…" : "Start Audetic daemon";
            Icon = PlayCircle;
            // "Start" is the same set of systemctl commands as enable; the
            // user just sees a different message.
            onClick = (): void => {
              void store.install.enable();
            };
            break;
          case "update":
            label = running ? "Updating…" : "Update daemon";
            Icon = RefreshCcw;
            onClick = (): void => {
              void store.install.update();
            };
            break;
          default:
            return null;
        }

        return (
          <div className="flex items-center gap-3">
            <Button onClick={onClick} disabled={running}>
              {running ? (
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              ) : (
                <Icon className="mr-2 h-4 w-4" />
              )}
              {label}
            </Button>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => {
                void store.install.detect();
              }}
              disabled={running}
            >
              Re-check
            </Button>
          </div>
        );
      }}
    </Observer>
  );
}

function ProgressLine() {
  const store = useStore();
  return (
    <Observer>
      {() => {
        const p = store.install.progress;
        const status = store.install.installStatus;
        if (status !== "running" || !p) return null;
        return (
          <div className="text-xs text-muted-foreground">
            <span className="font-mono">{p.step}</span>
            {p.detail ? <span className="ml-2">{p.detail}</span> : null}
          </div>
        );
      }}
    </Observer>
  );
}

function ErrorLine() {
  const store = useStore();
  return (
    <Observer>
      {() => {
        const err = store.install.installError;
        if (!err) return null;
        return (
          <pre className="whitespace-pre-wrap rounded border border-destructive/40 bg-destructive/10 p-2 text-xs text-destructive">
            {err}
          </pre>
        );
      }}
    </Observer>
  );
}

function iconForDecision(kind: string): React.ReactNode {
  const cls = "h-4 w-4";
  switch (kind) {
    case "install":
      return <Download className={cn(cls, "text-primary")} />;
    case "enable":
      return <Settings2 className={cn(cls, "text-primary")} />;
    case "start":
      return <PlayCircle className={cn(cls, "text-primary")} />;
    case "update":
      return <RefreshCcw className={cn(cls, "text-amber-500")} />;
    default:
      return null;
  }
}

function titleForDecision(kind: string): string {
  switch (kind) {
    case "install":
      return "Set up the Audetic daemon";
    case "enable":
      return "Enable the Audetic daemon";
    case "start":
      return "Start the Audetic daemon";
    case "update":
      return "Daemon update available";
    default:
      return "";
  }
}

function descriptionForDecision(
  kind: string,
  decision: import("@/stores/install-store").OnboardingDecision,
): string {
  switch (kind) {
    case "install":
      return "Audetic needs a background service so global hotkeys (Super+R / Super+Shift+R) keep working when this window is closed. We'll copy the bundled daemon to ~/.local/share/audetic/bin and enable it as a systemd user service. No sudo required.";
    case "enable":
      return "A systemd unit is installed but not enabled — that means the daemon won't auto-start at login. Enabling it brings the service up immediately.";
    case "start":
      return "The systemd unit is enabled but the daemon isn't running. Start it to bring the dashboard back online.";
    case "update": {
      if (decision.kind !== "update") return "";
      return `Bundled daemon ${decision.bundled} differs from the installed copy${decision.installed ? ` (${decision.installed})` : ""}. Updating restarts the user service with the new binary.`;
    }
    default:
      return "";
  }
}
