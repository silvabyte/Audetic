import { useEffect } from "react";
import { Observer } from "mobx-react-lite";
import {
  useFetcher,
  type ActionFunctionArgs,
  type RouteObject,
} from "react-router-dom";
import {
  CheckCircle2,
  Download,
  Loader2,
  RefreshCcw,
  TriangleAlert,
} from "lucide-react";
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
      <ModelsCard />
    </div>
  );
}

function ModelsCard() {
  const store = useStore();

  useEffect(() => {
    if (store.config.modelsState === "idle") {
      void store.config.loadModels();
    }
  }, [store]);

  return (
    <Observer>
      {() => {
        const models = store.config.models;
        const loading = store.config.modelsState === "loading" && models.length === 0;
        return (
          <Card>
            <CardHeader>
              <CardTitle className="text-base">On-device models</CardTitle>
              <CardDescription>
                Download a model to transcribe locally (no cloud). Used when the
                provider is set to{" "}
                <code className="font-mono text-xs">local</code>. Parakeet runs
                fast on CPU; Whisper is higher accuracy.
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-3">
              {loading ? (
                [0, 1, 2, 3].map((i) => <Skeleton key={i} className="h-12 w-full" />)
              ) : models.length === 0 ? (
                <p className="text-sm text-muted-foreground">No models available.</p>
              ) : (
                models.map((model) => <ModelRow key={model.id} model={model} />)
              )}
            </CardContent>
          </Card>
        );
      }}
    </Observer>
  );
}

function ModelRow({
  model,
}: {
  model: ReturnType<typeof useStore>["config"]["models"][number];
}) {
  const store = useStore();
  const download = model.download;
  const downloading = download?.state === "downloading";
  const errored = download?.state === "error";
  const percent =
    download?.state === "downloading" && download.total_bytes > 0
      ? Math.min(100, Math.round((download.downloaded_bytes / download.total_bytes) * 100))
      : 0;

  return (
    <div className="rounded-md border p-3">
      <div className="flex items-center justify-between gap-3">
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <span className="text-sm font-medium">{model.label}</span>
            {model.recommended ? (
              <span className="rounded bg-primary/10 px-1.5 py-0.5 text-[10px] font-medium text-primary">
                recommended
              </span>
            ) : null}
          </div>
          <p className="text-xs text-muted-foreground">{model.description}</p>
          <p className="font-mono text-[11px] text-muted-foreground">
            {model.id} · {formatGb(model.size_bytes)}
          </p>
        </div>
        <div className="shrink-0">
          {model.installed ? (
            <span className="flex items-center gap-1 text-sm text-primary">
              <CheckCircle2 className="h-4 w-4" /> Installed
            </span>
          ) : (
            <Button
              size="sm"
              variant="outline"
              disabled={downloading}
              onClick={() => void store.config.downloadModel(model.id)}
            >
              {downloading ? (
                <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" />
              ) : (
                <Download className="mr-1 h-3.5 w-3.5" />
              )}
              {downloading ? `${percent}%` : "Download"}
            </Button>
          )}
        </div>
      </div>
      {downloading ? (
        <div className="mt-2 h-1.5 w-full overflow-hidden rounded-full bg-muted">
          <div
            className="h-full bg-primary transition-all"
            style={{ width: `${percent}%` }}
          />
        </div>
      ) : null}
      {errored ? (
        <p className="mt-2 text-xs text-destructive">
          {download?.state === "error" ? download.message : "Download failed."}
        </p>
      ) : null}
    </div>
  );
}

function formatGb(bytes: number): string {
  const gb = bytes / 1_000_000_000;
  if (gb >= 1) return `${gb.toFixed(2)} GB`;
  return `${Math.round(bytes / 1_000_000)} MB`;
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
