import { useState } from "react";
import { Observer } from "mobx-react-lite";
import {
  useFetcher,
  type ActionFunctionArgs,
  type RouteObject,
} from "react-router-dom";
import { CheckCircle2, Download, RefreshCcw, TriangleAlert } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { useStore } from "@/stores/root-store";
import { getRootStore } from "@/stores/singleton";
import { cn } from "@/lib/utils";

const UPDATE_INTENTS = {
  check: "check-update",
  install: "install-update",
  setAuto: "set-auto-update",
} as const;

export const settingsUpdatesRoute: RouteObject = {
  path: "updates",
  action: async ({ request }: ActionFunctionArgs) => {
    const form = await request.formData();
    const intent = form.get("intent");
    const root = getRootStore();
    switch (intent) {
      case UPDATE_INTENTS.check:
        await root.config.loadUpdate();
        return null;
      case UPDATE_INTENTS.install: {
        const force = form.get("force") === "true";
        await root.config.installUpdate(force);
        return null;
      }
      case UPDATE_INTENTS.setAuto: {
        const enabled = form.get("enabled") === "true";
        await root.config.setAutoUpdate(enabled);
        return null;
      }
      default:
        return null;
    }
  },
  Component: SettingsUpdates,
};

function SettingsUpdates() {
  return (
    <div className="space-y-6">
      <header>
        <h2 className="text-xl font-semibold">Updates</h2>
        <p className="text-sm text-muted-foreground">
          Daemon self-update. Binary is fetched from install.audetic.ai on
          the configured channel.
        </p>
      </header>

      <UpdateStatusCard />
      <AutoUpdateCard />
    </div>
  );
}

function UpdateStatusCard() {
  const store = useStore();
  const checkFetcher = useFetcher();
  const installFetcher = useFetcher();
  const checking = checkFetcher.state !== "idle";
  const installing = installFetcher.state !== "idle";

  return (
    <Observer>
      {() => {
        const report = store.config.update;
        const loading = store.config.updateState === "loading";

        return (
          <Card>
            <CardHeader className="flex-row items-start justify-between gap-4 space-y-0">
              <div className="space-y-1">
                <CardTitle className="text-base">Version</CardTitle>
                <CardDescription>
                  <UpdateSummary
                    report={report}
                    loading={loading && !report}
                  />
                </CardDescription>
              </div>
              <div className="flex gap-2">
                <checkFetcher.Form method="post">
                  <input type="hidden" name="intent" value={UPDATE_INTENTS.check} />
                  <Button
                    type="submit"
                    variant="outline"
                    size="sm"
                    disabled={checking || installing}
                  >
                    <RefreshCcw
                      className={cn("mr-1 h-3.5 w-3.5", checking && "animate-spin")}
                    />
                    {checking ? "Checking…" : "Check"}
                  </Button>
                </checkFetcher.Form>
                <installFetcher.Form method="post">
                  <input type="hidden" name="intent" value={UPDATE_INTENTS.install} />
                  <input type="hidden" name="force" value="false" />
                  <Button
                    type="submit"
                    size="sm"
                    disabled={
                      installing ||
                      checking ||
                      !report ||
                      !report.remote_version ||
                      report.remote_version === report.current_version
                    }
                  >
                    <Download className="mr-1 h-3.5 w-3.5" />
                    {installing ? "Installing…" : "Install"}
                  </Button>
                </installFetcher.Form>
              </div>
            </CardHeader>
            {report?.installed && (
              <CardContent className="text-xs text-muted-foreground">
                Install complete.
                {report.restart_required ? " Restart the daemon to pick up the new binary." : ""}
              </CardContent>
            )}
            {store.config.lastError && (
              <CardContent className="text-xs text-destructive">
                {store.config.lastError}
              </CardContent>
            )}
          </Card>
        );
      }}
    </Observer>
  );
}

function UpdateSummary({
  report,
  loading,
}: {
  report: ReturnType<typeof useStore>["config"]["update"];
  loading: boolean;
}) {
  if (loading) return <span>Loading…</span>;
  if (!report) return <span>Unknown.</span>;

  const hasUpdate =
    !!report.remote_version && report.remote_version !== report.current_version;

  return (
    <span className="flex items-center gap-2">
      {hasUpdate ? (
        <TriangleAlert className="h-4 w-4 text-amber-500" />
      ) : (
        <CheckCircle2 className="h-4 w-4 text-primary" />
      )}
      <span>
        Current <code className="font-mono">{report.current_version}</code>
        {report.remote_version
          ? ` · latest ${report.remote_version}`
          : " · no remote version info"}
      </span>
    </span>
  );
}

function AutoUpdateCard() {
  // The daemon doesn't report the current auto-update flag anywhere the
  // UI can read; we track the user's last toggle locally so the switch
  // feels responsive. Flipping fires PUT /update/auto.
  const [enabled, setEnabled] = useState(false);
  const fetcher = useFetcher();
  const submitting = fetcher.state !== "idle";

  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-base">Auto-update</CardTitle>
        <CardDescription>
          When enabled, the daemon checks for + installs updates in the
          background on the configured channel.
        </CardDescription>
      </CardHeader>
      <CardContent>
        <fetcher.Form
          method="post"
          onSubmit={() => setEnabled((prev) => !prev)}
          className="flex items-center gap-3"
        >
          <input type="hidden" name="intent" value={UPDATE_INTENTS.setAuto} />
          <input type="hidden" name="enabled" value={enabled ? "false" : "true"} />
          <Button type="submit" variant="outline" size="sm" disabled={submitting}>
            {submitting ? "Saving…" : enabled ? "Disable" : "Enable"}
          </Button>
          <span className="text-sm text-muted-foreground">
            Currently{" "}
            <span className={cn(enabled ? "text-primary" : "text-foreground")}>
              {enabled ? "enabled" : "disabled"}
            </span>{" "}
            (locally tracked — daemon doesn't expose the flag).
          </span>
        </fetcher.Form>
      </CardContent>
    </Card>
  );
}
