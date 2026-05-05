import { Observer } from "mobx-react-lite";
import {
  useFetcher,
  type ActionFunctionArgs,
  type RouteObject,
} from "react-router-dom";
import { CheckCircle2, RefreshCcw, TriangleAlert } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { cn } from "@/lib/utils";
import { useStore } from "@/stores/root-store";
import { getRootStore } from "@/stores/singleton";

const PROVIDER_INTENTS = {
  test: "test-provider",
} as const;

export const settingsProviderRoute: RouteObject = {
  index: true,
  action: async ({ request }: ActionFunctionArgs) => {
    const form = await request.formData();
    const intent = form.get("intent");
    const root = getRootStore();
    switch (intent) {
      case PROVIDER_INTENTS.test:
        await Promise.all([
          root.config.loadProvider(),
          root.config.loadProviderStatus(),
        ]);
        return null;
      default:
        return null;
    }
  },
  Component: SettingsProvider,
};

function SettingsProvider() {
  const store = useStore();
  return (
    <div className="space-y-6">
      <header>
        <h2 className="text-xl font-semibold">Provider</h2>
        <p className="text-sm text-muted-foreground">
          Transcription backend configuration. Edit{" "}
          <code className="font-mono text-xs">config.toml</code> to change.
        </p>
      </header>

      <ProviderInfoCard />
      <ProviderStatusCard />
    </div>
  );
}

function ProviderInfoCard() {
  const store = useStore();
  return (
    <Observer>
      {() => {
        const info = store.config.provider;
        const state = store.config.providerState;
        return (
          <Card>
            <CardHeader>
              <CardTitle className="text-base">Current configuration</CardTitle>
            </CardHeader>
            <CardContent>
              {state === "loading" && !info ? (
                <dl className="grid grid-cols-[8rem_1fr] gap-y-2 text-sm">
                  {[0, 1, 2, 3, 4].map((i) => (
                    <FieldSkeleton key={i} />
                  ))}
                </dl>
              ) : !info ? (
                <p className="text-sm text-destructive">Couldn't load provider info.</p>
              ) : (
                <dl className="grid grid-cols-[8rem_1fr] gap-y-2 text-sm">
                  <Field label="Provider" value={info.provider} />
                  <Field label="Model" value={info.model} />
                  <Field label="Language" value={info.language} />
                  <Field label="Endpoint" value={info.api_endpoint} mono />
                  <Field label="API key" value={info.has_api_key ? "set" : "not set"} />
                  <Field label="CLI path" value={info.command_path} mono />
                  <Field label="Model path" value={info.model_path} mono />
                </dl>
              )}
            </CardContent>
          </Card>
        );
      }}
    </Observer>
  );
}

function ProviderStatusCard() {
  const store = useStore();
  const fetcher = useFetcher();
  const testing = fetcher.state !== "idle";

  return (
    <Observer>
      {() => {
        const status = store.config.providerStatus;
        const loading = store.config.providerStatusState === "loading";

        return (
          <Card>
            <CardHeader className="flex-row items-center justify-between gap-4 space-y-0">
              <div>
                <CardTitle className="text-base">Status</CardTitle>
                <CardDescription>
                  Ping the configured provider. Runs against the daemon's
                  <code className="mx-1 font-mono text-xs">
                    /provider/status
                  </code>
                  endpoint.
                </CardDescription>
              </div>
              <fetcher.Form method="post">
                <input
                  type="hidden"
                  name="intent"
                  value={PROVIDER_INTENTS.test}
                />
                <Button
                  type="submit"
                  variant="outline"
                  size="sm"
                  disabled={testing || loading}
                >
                  <RefreshCcw
                    className={cn("mr-1 h-3.5 w-3.5", testing && "animate-spin")}
                  />
                  {testing ? "Testing…" : "Test"}
                </Button>
              </fetcher.Form>
            </CardHeader>
            <CardContent>
              <StatusBadge status={status} loading={loading && !status} />
            </CardContent>
          </Card>
        );
      }}
    </Observer>
  );
}

function StatusBadge({
  status,
  loading,
}: {
  status: ReturnType<typeof useStore>["config"]["providerStatus"];
  loading: boolean;
}) {
  if (loading) {
    return (
      <div className="flex items-center gap-2">
        <Skeleton className="h-4 w-4 rounded-full" />
        <Skeleton className="h-3 w-40" />
      </div>
    );
  }
  if (!status) return <span className="text-sm text-muted-foreground">Unknown.</span>;

  if (status.status === "ready") {
    return (
      <div className="flex items-center gap-2 text-sm text-primary">
        <CheckCircle2 className="h-4 w-4" />
        <span>
          Ready — {status.provider}
          {status.model ? ` (${status.model})` : ""}
          {status.language ? ` · ${status.language}` : ""}
        </span>
      </div>
    );
  }
  if (status.status === "config_error") {
    return (
      <div className="space-y-1">
        <div className="flex items-center gap-2 text-sm text-destructive">
          <TriangleAlert className="h-4 w-4" />
          <span>Config error — {status.provider}</span>
        </div>
        <p className="pl-6 text-xs text-muted-foreground">{status.error}</p>
      </div>
    );
  }
  return (
    <div className="flex items-center gap-2 text-sm text-muted-foreground">
      <TriangleAlert className="h-4 w-4" />
      <span>Not configured.</span>
    </div>
  );
}

function Field({
  label,
  value,
  mono,
}: {
  label: string;
  value: string | null | undefined;
  mono?: boolean;
}) {
  return (
    <>
      <dt className="text-muted-foreground">{label}</dt>
      <dd
        className={cn(
          "min-w-0 break-words",
          mono && "font-mono text-xs",
          !value && "text-muted-foreground italic",
        )}
      >
        {value ?? "—"}
      </dd>
    </>
  );
}

function FieldSkeleton() {
  return (
    <>
      <Skeleton className="h-3 w-20" />
      <Skeleton className="h-3 w-full max-w-[16rem]" />
    </>
  );
}
