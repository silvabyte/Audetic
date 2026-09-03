import { useEffect, useState, type ComponentProps } from "react";
import { Observer } from "mobx-react-lite";
import type { RouteObject } from "react-router-dom";
import {
  CheckCircle2,
  Download,
  Loader2,
  RefreshCcw,
  RotateCcw,
  Save,
  ShieldCheck,
  TriangleAlert,
} from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import { cn } from "@/lib/utils";
import type {
  ModelDescriptor,
  ProviderStatus,
  WhisperConfig,
} from "@/stores/config-store";
import { useStore } from "@/stores/root-store";

type ProviderName =
  | "audetic-api"
  | "local"
  | "assembly-ai"
  | "openai-api"
  | "openai-cli"
  | "whisper-cpp";

interface ProviderChoice {
  value: ProviderName;
  label: string;
  description: string;
}

interface ProviderDraft {
  provider: ProviderName;
  model: string;
  language: string;
  apiEndpoint: string;
  apiKey: string;
  commandPath: string;
  modelPath: string;
}

const PROVIDERS: ProviderChoice[] = [
  {
    value: "audetic-api",
    label: "Audetic API",
    description: "Hosted transcription with no API key required.",
  },
  {
    value: "local",
    label: "On-device",
    description: "Embedded Parakeet or Whisper using a downloaded model.",
  },
  {
    value: "assembly-ai",
    label: "AssemblyAI",
    description: "AssemblyAI's transcription API using your key.",
  },
  {
    value: "openai-api",
    label: "OpenAI API",
    description: "OpenAI audio transcription using your API key.",
  },
  {
    value: "openai-cli",
    label: "OpenAI CLI",
    description: "A locally installed Python whisper executable.",
  },
  {
    value: "whisper-cpp",
    label: "whisper.cpp",
    description: "A local whisper.cpp binary and GGML/GGUF model file.",
  },
];

const PROVIDER_DEFAULTS: Record<
  ProviderName,
  Pick<ProviderDraft, "model" | "language" | "apiEndpoint">
> = {
  "audetic-api": {
    model: "base",
    language: "en",
    apiEndpoint: "https://audio.audetic.link/api/v1/transcriptions",
  },
  local: { model: "parakeet-tdt-0.6b-v3", language: "auto", apiEndpoint: "" },
  "assembly-ai": {
    model: "",
    language: "en",
    apiEndpoint: "https://api.assemblyai.com/v2",
  },
  "openai-api": {
    model: "whisper-1",
    language: "en",
    apiEndpoint: "https://api.openai.com/v1/audio/transcriptions",
  },
  "openai-cli": { model: "base", language: "en", apiEndpoint: "" },
  "whisper-cpp": { model: "base", language: "en", apiEndpoint: "" },
};

export const settingsProviderRoute: RouteObject = {
  index: true,
  Component: SettingsProvider,
};

function SettingsProvider() {
  const store = useStore();

  useEffect(() => {
    if (store.config.providerConfigState === "idle") {
      void store.config.loadAll();
    }
  }, [store]);

  return (
    <div className="space-y-5">
      <header>
        <div className="mb-1 flex items-center gap-2 text-xs font-medium uppercase tracking-[0.16em] text-muted-foreground">
          <ShieldCheck className="h-3.5 w-3.5" /> Transcription
        </div>
        <h2 className="text-xl font-semibold">Provider</h2>
        <p className="max-w-2xl text-sm text-muted-foreground">
          Choose, validate, and save the transcription backend used for dictation and
          meetings.
        </p>
      </header>

      <RestartRequiredNotice />
      <ProviderConfigurationCard />
      <ProviderStatusCard />
      <ModelsCard />
    </div>
  );
}

function ProviderConfigurationCard() {
  const store = useStore();
  return (
    <Observer>
      {() => {
        const config = store.config.providerConfig;
        const loading = store.config.providerConfigState === "loading" && !config;
        if (loading) {
          return <Skeleton className="h-[30rem] w-full" />;
        }
        if (!config) {
          return (
            <Card>
              <CardContent className="p-5 text-sm text-destructive" role="alert">
                Could not load provider configuration
                {store.config.providerConfigError
                  ? `: ${store.config.providerConfigError}`
                  : "."}
              </CardContent>
            </Card>
          );
        }

        const models = store.config.models.map((model) => ({ ...model }));
        return <ProviderForm config={config} models={models} />;
      }}
    </Observer>
  );
}

function ProviderForm({
  config,
  models,
}: {
  config: WhisperConfig;
  models: ModelDescriptor[];
}) {
  const store = useStore();
  const [draft, setDraft] = useState<ProviderDraft>(() => draftFromConfig(config));

  useEffect(() => {
    setDraft(draftFromConfig(config));
  }, [config]);

  const selectedModel = models.find((model) => model.id === draft.model);
  const currentProvider = normalizeProvider(config.provider);
  const hasPreservableKey =
    providerNeedsKey(draft.provider) &&
    draft.provider === currentProvider &&
    Boolean(config.api_key);

  function update<K extends keyof ProviderDraft>(key: K, value: ProviderDraft[K]): void {
    store.config.clearProviderFeedback();
    setDraft((current) => ({ ...current, [key]: value }));
  }

  function changeProvider(provider: ProviderName): void {
    store.config.clearProviderFeedback();
    const defaults = PROVIDER_DEFAULTS[provider];
    const recommended = models.find((model) => model.recommended)?.id;
    setDraft((current) => ({
      ...current,
      provider,
      model: provider === "local" ? (recommended ?? defaults.model) : defaults.model,
      language: defaults.language,
      apiEndpoint: defaults.apiEndpoint,
      apiKey: "",
      commandPath: provider === current.provider ? current.commandPath : "",
      modelPath: provider === current.provider ? current.modelPath : "",
    }));
  }

  function candidate(): WhisperConfig {
    return configFromDraft(draft, config);
  }

  async function validate(): Promise<void> {
    const result = await store.config.validateProviderConfig(candidate());
    if (result?.status === "ready") {
      toast.success("Provider configuration is valid");
    } else {
      toast.error("Provider configuration is not ready", {
        description:
          result?.status === "config_error"
            ? result.error
            : store.config.providerValidationError ?? "Choose a provider.",
      });
    }
  }

  async function save(): Promise<void> {
    const saved = await store.config.saveProviderConfig(candidate());
    if (!saved) {
      toast.error("Could not save provider configuration", {
        description: store.config.providerSaveError ?? undefined,
      });
      return;
    }
    setDraft(draftFromConfig(saved));
    toast.success("Provider configuration saved", {
      description: store.config.restartRequired
        ? "Restart the daemon to apply it to transcription workflows."
        : "The active provider configuration was unchanged.",
    });
  }

  return (
    <Observer>
      {() => {
        const validating = store.config.providerValidationState === "loading";
        const saving = store.config.providerSaveState === "loading";
        const validation = store.config.providerValidation;
        return (
          <Card>
            <CardHeader>
              <CardTitle className="text-base">Configuration</CardTitle>
              <CardDescription>
                Proposed values are initialized by the daemon before anything is saved.
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-5">
              <div className="space-y-2">
                <Label htmlFor="provider">Provider</Label>
                <select
                  id="provider"
                  value={draft.provider}
                  onChange={(event) => changeProvider(event.target.value as ProviderName)}
                  className="flex h-9 w-full rounded-md border border-input bg-transparent px-3 py-1 text-sm shadow-sm outline-none focus-visible:ring-1 focus-visible:ring-ring"
                >
                  {PROVIDERS.map((provider) => (
                    <option key={provider.value} value={provider.value}>
                      {provider.label}
                    </option>
                  ))}
                </select>
                <p className="text-xs text-muted-foreground">
                  {PROVIDERS.find((provider) => provider.value === draft.provider)?.description}
                </p>
              </div>

              <div className="grid gap-4 sm:grid-cols-2">
                {draft.provider === "local" ? (
                  <LocalModelField
                    draft={draft}
                    models={models}
                    selectedModel={selectedModel}
                    onModelChange={(model) => {
                      update("model", model.id);
                      if (!model.supports_language_selection) update("language", "auto");
                    }}
                  />
                ) : draft.provider !== "assembly-ai" ? (
                  <TextField
                    id="provider-model"
                    label={draft.provider === "whisper-cpp" ? "Model label" : "Model"}
                    value={draft.model}
                    onChange={(value) => update("model", value)}
                    placeholder={PROVIDER_DEFAULTS[draft.provider].model}
                  />
                ) : null}

                <TextField
                  id="provider-language"
                  label="Language"
                  value={draft.language}
                  onChange={(value) => update("language", value)}
                  placeholder="en or auto"
                  disabled={draft.provider === "local" && selectedModel?.supports_language_selection === false}
                  hint={
                    draft.provider === "local" && selectedModel?.supports_language_selection === false
                      ? "This model detects language automatically."
                      : "ISO 639-1 code, or auto."
                  }
                />

                {providerUsesEndpoint(draft.provider) ? (
                  <TextField
                    id="provider-endpoint"
                    label={draft.provider === "assembly-ai" ? "API base URL" : "API endpoint"}
                    value={draft.apiEndpoint}
                    onChange={(value) => update("apiEndpoint", value)}
                    className="sm:col-span-2"
                  />
                ) : null}

                {providerNeedsKey(draft.provider) ? (
                  <TextField
                    id="provider-api-key"
                    label="API key"
                    value={draft.apiKey}
                    onChange={(value) => update("apiKey", value)}
                    type="password"
                    autoComplete="new-password"
                    placeholder={hasPreservableKey ? "Existing key is set" : "Required"}
                    hint={
                      hasPreservableKey
                        ? "Leave blank to preserve the current key. It is never displayed."
                        : "Stored only in your local Audetic config."
                    }
                    className="sm:col-span-2"
                  />
                ) : null}

                {providerUsesCommand(draft.provider) ? (
                  <TextField
                    id="provider-command"
                    label="Command path"
                    value={draft.commandPath}
                    onChange={(value) => update("commandPath", value)}
                    placeholder={
                      draft.provider === "openai-cli"
                        ? "/usr/bin/whisper"
                        : "/usr/local/bin/whisper-cli"
                    }
                    className="sm:col-span-2"
                  />
                ) : null}

                {draft.provider === "whisper-cpp" ? (
                  <TextField
                    id="provider-model-path"
                    label="Model file path"
                    value={draft.modelPath}
                    onChange={(value) => update("modelPath", value)}
                    placeholder="/path/to/ggml-base.bin"
                    className="sm:col-span-2"
                  />
                ) : null}
              </div>

              {validation ? <ValidationMessage status={validation} /> : null}
              {store.config.providerValidationError ? (
                <InlineError message={store.config.providerValidationError} />
              ) : null}
              {store.config.providerSaveError ? (
                <InlineError message={store.config.providerSaveError} />
              ) : null}

              <div className="flex flex-col-reverse gap-2 border-t pt-4 sm:flex-row sm:justify-end">
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => void validate()}
                  disabled={validating || saving}
                >
                  {validating ? (
                    <Loader2 className="mr-1.5 h-4 w-4 animate-spin" />
                  ) : (
                    <ShieldCheck className="mr-1.5 h-4 w-4" />
                  )}
                  Validate changes
                </Button>
                <Button type="button" onClick={() => void save()} disabled={validating || saving}>
                  {saving ? (
                    <Loader2 className="mr-1.5 h-4 w-4 animate-spin" />
                  ) : (
                    <Save className="mr-1.5 h-4 w-4" />
                  )}
                  {saving ? "Validating…" : "Save configuration"}
                </Button>
              </div>
            </CardContent>
          </Card>
        );
      }}
    </Observer>
  );
}

function LocalModelField({
  draft,
  models,
  selectedModel,
  onModelChange,
}: {
  draft: ProviderDraft;
  models: ModelDescriptor[];
  selectedModel: ModelDescriptor | undefined;
  onModelChange: (model: ModelDescriptor) => void;
}) {
  const store = useStore();
  const download = selectedModel?.download;
  const downloading = download?.state === "downloading";
  const percent =
    download?.state === "downloading" && download.total_bytes > 0
      ? Math.min(100, Math.round((download.downloaded_bytes / download.total_bytes) * 100))
      : 0;

  return (
    <div className="space-y-2 sm:col-span-2">
      <Label htmlFor="provider-local-model">Local model</Label>
      <div className="flex flex-col gap-2 sm:flex-row">
        <select
          id="provider-local-model"
          value={draft.model}
          onChange={(event) => {
            const model = models.find((candidate) => candidate.id === event.target.value);
            if (model) onModelChange(model);
          }}
          className="flex h-9 min-w-0 flex-1 rounded-md border border-input bg-transparent px-3 py-1 text-sm shadow-sm outline-none focus-visible:ring-1 focus-visible:ring-ring"
        >
          {models.map((model) => (
            <option key={model.id} value={model.id}>
              {model.label}{model.recommended ? " — recommended" : ""}
            </option>
          ))}
        </select>
        {selectedModel && !selectedModel.installed ? (
          <Button
            type="button"
            variant="outline"
            disabled={downloading}
            onClick={() => void store.config.downloadModel(selectedModel.id)}
          >
            {downloading ? (
              <Loader2 className="mr-1.5 h-4 w-4 animate-spin" />
            ) : (
              <Download className="mr-1.5 h-4 w-4" />
            )}
            {downloading ? `${percent}%` : "Download model"}
          </Button>
        ) : null}
      </div>
      {selectedModel ? (
        <p className="text-xs text-muted-foreground">
          {selectedModel.description} · {formatGb(selectedModel.size_bytes)} · {selectedModel.installed ? "installed" : "not installed"}
        </p>
      ) : (
        <p className="text-xs text-destructive">No local model catalog is available.</p>
      )}
    </div>
  );
}

function TextField({
  id,
  label,
  value,
  onChange,
  hint,
  className,
  ...inputProps
}: {
  id: string;
  label: string;
  value: string;
  onChange: (value: string) => void;
  hint?: string;
  className?: string;
} & Omit<ComponentProps<typeof Input>, "id" | "value" | "onChange">) {
  return (
    <div className={cn("space-y-2", className)}>
      <Label htmlFor={id}>{label}</Label>
      <Input
        id={id}
        value={value}
        onChange={(event) => onChange(event.target.value)}
        {...inputProps}
      />
      {hint ? <p className="text-xs text-muted-foreground">{hint}</p> : null}
    </div>
  );
}

function RestartRequiredNotice() {
  const store = useStore();
  const [open, setOpen] = useState(false);

  async function restart(): Promise<void> {
    setOpen(true);
    const restarted = await store.config.restartDaemon();
    if (restarted) {
      toast.success("Daemon restarted", { description: "Provider and setup status were rechecked." });
    } else {
      toast.error("Could not confirm daemon restart", {
        description: store.config.restartError ?? undefined,
      });
    }
  }

  return (
    <Observer>
      {() => {
        const required = store.config.restartRequired;
        const state = store.config.restartState;
        if (!required && state !== "complete") return null;
        return (
          <>
            <Card className={cn(required ? "border-primary/50 bg-primary/5" : "bg-muted/20")}>
              <CardContent className="flex flex-col gap-3 p-4 sm:flex-row sm:items-center sm:justify-between">
                <div className="flex gap-3">
                  {required ? (
                    <TriangleAlert className="mt-0.5 h-5 w-5 shrink-0 text-primary" />
                  ) : (
                    <CheckCircle2 className="mt-0.5 h-5 w-5 shrink-0" />
                  )}
                  <div>
                    <p className="text-sm font-semibold">
                      {required ? "Restart required" : "Daemon restarted"}
                    </p>
                    <p className="text-xs text-muted-foreground">
                      {required
                        ? "The saved provider will not be used by active transcription workflows until audeticd restarts."
                        : "A new daemon instance is running and provider/setup status has been rechecked."}
                    </p>
                  </div>
                </div>
                {required ? (
                  <Button type="button" size="sm" onClick={() => void restart()}>
                    <RotateCcw className="mr-1.5 h-4 w-4" /> Restart daemon
                  </Button>
                ) : null}
              </CardContent>
            </Card>

            <Dialog open={open} onOpenChange={setOpen}>
              <DialogContent>
                <DialogHeader>
                  <DialogTitle>Restarting Audetic</DialogTitle>
                  <DialogDescription>
                    The local service manager is restarting {store.config.restartResult?.service ?? "the daemon"}.
                  </DialogDescription>
                </DialogHeader>
                <div className="flex items-center gap-3 rounded-md border bg-muted/20 p-4 text-sm" aria-live="polite">
                  {state === "error" ? (
                    <TriangleAlert className="h-5 w-5 shrink-0 text-destructive" />
                  ) : state === "complete" ? (
                    <CheckCircle2 className="h-5 w-5 shrink-0" />
                  ) : (
                    <Loader2 className="h-5 w-5 shrink-0 animate-spin" />
                  )}
                  <span>
                    {state === "requesting"
                      ? "Requesting restart…"
                      : state === "waiting"
                        ? "Waiting for a new daemon instance…"
                        : state === "complete"
                          ? "Restart complete. Provider and setup status are current."
                          : store.config.restartError ?? "Preparing restart…"}
                  </span>
                </div>
                {state === "complete" || state === "error" ? (
                  <DialogFooter>
                    <Button type="button" onClick={() => setOpen(false)}>Close</Button>
                  </DialogFooter>
                ) : null}
              </DialogContent>
            </Dialog>
          </>
        );
      }}
    </Observer>
  );
}

function ProviderStatusCard() {
  const store = useStore();

  async function testInitialization(): Promise<void> {
    const result = await store.config.testProvider();
    if (result?.success) {
      toast.success("Provider initialized successfully");
    } else {
      toast.error("Provider initialization failed", {
        description: store.config.providerTestError ?? undefined,
      });
    }
  }

  return (
    <Observer>
      {() => {
        const status = store.config.providerStatus;
        const loading = store.config.providerStatusState === "loading";
        const testing = store.config.providerTestState === "loading";
        return (
          <Card>
            <CardHeader className="flex-row items-start justify-between gap-4 space-y-0">
              <div>
                <CardTitle className="text-base">Active provider status</CardTitle>
                <CardDescription>
                  The provider loaded by this daemon process. No sample is recorded or transcribed.
                </CardDescription>
              </div>
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={testing || loading}
                onClick={() => void testInitialization()}
              >
                <RefreshCcw className={cn("mr-1 h-3.5 w-3.5", testing && "animate-spin")} />
                {testing ? "Validating…" : "Validate initialization"}
              </Button>
            </CardHeader>
            <CardContent className="space-y-2">
              <StatusBadge status={status} loading={loading && !status} />
              {store.config.providerTestError ? (
                <InlineError message={store.config.providerTestError} />
              ) : null}
            </CardContent>
          </Card>
        );
      }}
    </Observer>
  );
}

function ValidationMessage({ status }: { status: ProviderStatus }) {
  if (status.status === "ready") {
    return (
      <div className="flex items-center gap-2 rounded-md border bg-muted/20 p-3 text-sm">
        <CheckCircle2 className="h-4 w-4" /> Proposed configuration initializes successfully.
      </div>
    );
  }
  const message = status.status === "config_error" ? status.error : "Choose a provider.";
  return <InlineError message={message} />;
}

function InlineError({ message }: { message: string }) {
  return (
    <div className="flex items-start gap-2 rounded-md border border-destructive/40 bg-destructive/5 p-3 text-sm text-destructive" role="alert">
      <TriangleAlert className="mt-0.5 h-4 w-4 shrink-0" />
      <span>{message}</span>
    </div>
  );
}

function StatusBadge({ status, loading }: { status: ProviderStatus | null; loading: boolean }) {
  if (loading) {
    return <div className="flex items-center gap-2"><Skeleton className="h-4 w-4 rounded-full" /><Skeleton className="h-3 w-40" /></div>;
  }
  if (!status) return <span className="text-sm text-muted-foreground">Unknown.</span>;
  if (status.status === "ready") {
    return (
      <div className="flex items-center gap-2 text-sm">
        <CheckCircle2 className="h-4 w-4" />
        <span>Ready — {status.provider}{status.model ? ` (${status.model})` : ""}{status.language ? ` · ${status.language}` : ""}</span>
      </div>
    );
  }
  if (status.status === "config_error") {
    return <InlineError message={`${status.provider}: ${status.error}`} />;
  }
  return <InlineError message="No transcription provider is configured." />;
}

function ModelsCard() {
  const store = useStore();
  return (
    <Observer>
      {() => {
        const models = store.config.models;
        const loading = store.config.modelsState === "loading" && models.length === 0;
        return (
          <Card>
            <CardHeader>
              <CardTitle className="text-base">On-device model catalog</CardTitle>
              <CardDescription>
                Downloaded models can be selected above when the provider is On-device.
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-3">
              {loading ? (
                [0, 1, 2].map((index) => <Skeleton key={index} className="h-14 w-full" />)
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

function ModelRow({ model }: { model: ModelDescriptor }) {
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
            {model.recommended ? <span className="rounded bg-primary/10 px-1.5 py-0.5 text-[10px] font-medium text-primary">recommended</span> : null}
          </div>
          <p className="text-xs text-muted-foreground">{model.description}</p>
          <p className="font-mono text-[11px] text-muted-foreground">{model.id} · {formatGb(model.size_bytes)}</p>
        </div>
        {model.installed ? (
          <span className="flex shrink-0 items-center gap-1 text-sm"><CheckCircle2 className="h-4 w-4" /> Installed</span>
        ) : (
          <Button type="button" size="sm" variant="outline" disabled={downloading} onClick={() => void store.config.downloadModel(model.id)}>
            {downloading ? <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" /> : <Download className="mr-1 h-3.5 w-3.5" />}
            {downloading ? `${percent}%` : "Download"}
          </Button>
        )}
      </div>
      {downloading ? <div className="mt-2 h-1.5 overflow-hidden rounded-full bg-muted"><div className="h-full bg-primary transition-all" style={{ width: `${percent}%` }} /></div> : null}
      {errored ? <p className="mt-2 text-xs text-destructive">{download?.state === "error" ? download.message : "Download failed."}</p> : null}
    </div>
  );
}

function draftFromConfig(config: WhisperConfig): ProviderDraft {
  const provider = normalizeProvider(config.provider);
  const defaults = PROVIDER_DEFAULTS[provider];
  return {
    provider,
    model: config.model ?? defaults.model,
    language: config.language ?? defaults.language,
    apiEndpoint: config.api_endpoint ?? defaults.apiEndpoint,
    apiKey: "",
    commandPath: config.command_path ?? "",
    modelPath: config.model_path ?? "",
  };
}

function configFromDraft(draft: ProviderDraft, existing: WhisperConfig): WhisperConfig {
  const sameProvider = normalizeProvider(existing.provider) === draft.provider;
  const apiKey = providerNeedsKey(draft.provider)
    ? nonEmpty(draft.apiKey) ?? (sameProvider ? nonEmpty(existing.api_key ?? "") : null)
    : null;
  return {
    provider: draft.provider,
    model: draft.provider === "assembly-ai" ? null : nonEmpty(draft.model),
    language: nonEmpty(draft.language),
    api_endpoint: providerUsesEndpoint(draft.provider) ? nonEmpty(draft.apiEndpoint) : null,
    api_key: apiKey,
    command_path: providerUsesCommand(draft.provider) ? nonEmpty(draft.commandPath) : null,
    model_path: draft.provider === "whisper-cpp" ? nonEmpty(draft.modelPath) : null,
  };
}

function normalizeProvider(provider: string | null): ProviderName {
  return PROVIDERS.some((choice) => choice.value === provider)
    ? (provider as ProviderName)
    : "audetic-api";
}

function providerNeedsKey(provider: ProviderName): boolean {
  return provider === "assembly-ai" || provider === "openai-api";
}

function providerUsesEndpoint(provider: ProviderName): boolean {
  return provider === "audetic-api" || provider === "assembly-ai" || provider === "openai-api";
}

function providerUsesCommand(provider: ProviderName): boolean {
  return provider === "openai-cli" || provider === "whisper-cpp";
}

function nonEmpty(value: string): string | null {
  const trimmed = value.trim();
  return trimmed ? trimmed : null;
}

function formatGb(bytes: number): string {
  const gb = bytes / 1_000_000_000;
  return gb >= 1 ? `${gb.toFixed(2)} GB` : `${Math.round(bytes / 1_000_000)} MB`;
}
