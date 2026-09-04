import { useState } from "react";
import { formatDistanceToNow } from "date-fns";
import { Observer } from "mobx-react-lite";
import {
  AlertTriangle,
  Check,
  CheckCircle2,
  Copy,
  Database,
  HardDrive,
  Home,
  Laptop,
  Loader2,
  Network,
  RefreshCcw,
  Search,
  Server,
  ShieldCheck,
  Unplug,
  UploadCloud,
  Wifi,
  WifiOff,
} from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader } from "@/components/ui/card";
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
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";
import { useStore } from "@/stores/root-store";
import {
  preferencesFromStatus,
  type CacheLevel,
  type SyncPreferences,
  type SyncRole,
} from "@/stores/sync-store";

const ROLE_COPY: Record<SyncRole, { label: string; description: string }> = {
  standalone: {
    label: "Standalone",
    description: "This device keeps its library local.",
  },
  home_hub: {
    label: "Home Hub",
    description: "This device owns the Shared Library.",
  },
  connected_device: {
    label: "Connected Device",
    description: "This device contributes to a Home Hub.",
  },
};

export function SharedLibraryCard() {
  const store = useStore();

  return (
    <Observer>
      {() => {
        const sync = store.sync;
        const status = sync.status;

        return (
          <Card className="relative min-w-0 overflow-hidden border-foreground/15 shadow-none">
            <div className="pointer-events-none absolute inset-x-0 top-0 h-1 bg-gradient-to-r from-transparent via-foreground/50 to-transparent" />
            <CardHeader className="border-b bg-muted/15 p-4 sm:p-5">
              <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
                <div className="flex min-w-0 items-start gap-3">
                  <div className="relative rounded-lg border bg-background p-2.5 shadow-sm">
                    <Database className="h-5 w-5" />
                    <span className={cn(
                      "absolute -right-1 -top-1 h-2.5 w-2.5 rounded-full border-2 border-background",
                      status?.hub_reachable ? "bg-foreground" : "bg-muted-foreground/40",
                    )} />
                  </div>
                  <div className="min-w-0">
                    <div className="mb-1 flex flex-wrap items-center gap-2">
                      <h3 className="text-base font-semibold">Shared Library</h3>
                      {status ? <RolePill role={status.role} /> : null}
                    </div>
                    <p className="max-w-2xl text-xs leading-relaxed text-muted-foreground sm:text-sm">
                      Bring completed records from your Audetic devices together over your private Tailscale network.
                    </p>
                  </div>
                </div>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  className="self-start"
                  disabled={sync.loading}
                  onClick={() => void sync.refresh()}
                >
                  <RefreshCcw className={cn("mr-1.5 h-3.5 w-3.5", sync.loading && "animate-spin")} />
                  Refresh
                </Button>
              </div>
            </CardHeader>

            <CardContent className="space-y-4 p-4 sm:p-5">
              {sync.error ? (
                <InlineError>Shared Library status failed: {sync.error}</InlineError>
              ) : null}

              {sync.loading && !status ? (
                <div className="space-y-3">
                  <Skeleton className="h-20 w-full" />
                  <div className="grid gap-3 sm:grid-cols-3">
                    <Skeleton className="h-16 w-full" />
                    <Skeleton className="h-16 w-full" />
                    <Skeleton className="h-16 w-full" />
                  </div>
                </div>
              ) : status ? (
                <>
                  <div className="grid gap-3 lg:grid-cols-[minmax(0,1.25fr)_minmax(16rem,0.75fr)]">
                    <div className="rounded-lg border bg-muted/15 p-3 sm:p-4">
                      <div className="mb-3 flex items-center justify-between gap-3">
                        <div className="flex items-center gap-2 text-sm font-semibold">
                          <Network className="h-4 w-4" /> Tailscale
                        </div>
                        <ReadinessPill ready={status.network.ready} />
                      </div>
                      <dl className="grid gap-2 text-xs sm:grid-cols-2">
                        <Detail label="Login" value={status.network.owner_login ?? "Not signed in"} mono />
                        <Detail label="Device" value={status.device_name ?? status.network.dns_name ?? "Unnamed device"} />
                        <Detail label="Backend" value={status.network.backend_state ?? "Unavailable"} />
                        <Detail label="Tailnet address" value={status.network.dns_name ?? "Not available"} mono />
                      </dl>
                      {!status.network.owner_login || status.network.backend_state !== "Running" ? (
                        <div className="mt-3 flex items-start gap-2 rounded-md border bg-background p-2.5 text-xs text-muted-foreground">
                          <Unplug className="mt-0.5 h-3.5 w-3.5 shrink-0" />
                          <span>Open Tailscale and sign in before activating or discovering a Shared Library.</span>
                        </div>
                      ) : null}
                      {status.network.error ? (
                        <p className="mt-3 break-words text-xs text-destructive">{status.network.error}</p>
                      ) : null}
                    </div>

                    <div className="grid grid-cols-2 gap-2 rounded-lg border bg-muted/15 p-3 sm:grid-cols-4 lg:grid-cols-2">
                      <Metric label="Reachability" value={reachabilityLabel(status.role, status.hub_reachable)} icon={status.hub_reachable ? Wifi : WifiOff} />
                      <Metric label="Last contact" value={formatLastContact(status.last_contact_at)} icon={ShieldCheck} />
                      <Metric label="Pending" value={`${status.pending_items} item${status.pending_items === 1 ? "" : "s"}`} icon={UploadCloud} />
                      <Metric label="Waiting data" value={formatBytes(status.pending_bytes)} icon={HardDrive} />
                    </div>
                  </div>

                  {status.last_error ? <InlineError>{status.last_error}</InlineError> : null}
                  {status.pending_items > 0 ? (
                    <Button
                      type="button"
                      size="sm"
                      variant="outline"
                      disabled={sync.operation !== null}
                      onClick={() => void sync.retryPending()}
                    >
                      Retry pending uploads
                    </Button>
                  ) : null}

                  <div className="grid gap-px overflow-hidden rounded-lg border bg-border sm:grid-cols-3">
                    <SettingSummary
                      label="Recording payloads"
                      value={status.upload_recording_payloads ? "Included" : "Text + metadata only"}
                    />
                    <SettingSummary label="Library cache" value={cacheLabel(status.cache_level)} />
                    <SettingSummary
                      label="Shared configuration"
                      value={status.shared_config_enabled
                        ? status.applied_shared_config_version
                          ? `Applied · v${status.applied_shared_config_version}`
                          : "Enabled"
                        : "Device settings only"}
                    />
                  </div>

                  {sync.actionError ? <InlineError>{sync.actionError}</InlineError> : null}

                  {status.role === "standalone" ? (
                    <StandaloneSetup initialPreferences={preferencesFromStatus(status)} />
                  ) : (
                    <ActiveRoleActions
                      role={status.role}
                      hubName={status.hub?.base_url ?? status.network.dns_name ?? null}
                      setupCommand={sync.setupCommand}
                    />
                  )}
                </>
              ) : (
                <div className="rounded-md border p-3 text-sm text-muted-foreground">
                  No Shared Library status is available while the daemon is unreachable.
                </div>
              )}
            </CardContent>
          </Card>
        );
      }}
    </Observer>
  );
}

function StandaloneSetup({ initialPreferences }: { initialPreferences: SyncPreferences }) {
  const store = useStore();
  const [deviceName, setDeviceName] = useState(initialPreferences.deviceName ?? "");
  const [uploadRecordings, setUploadRecordings] = useState(initialPreferences.uploadRecordingPayloads);
  const [cacheLevel, setCacheLevel] = useState<CacheLevel>(initialPreferences.cacheLevel);
  const [sharedConfig, setSharedConfig] = useState(initialPreferences.sharedConfigEnabled);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [serveConfirmed, setServeConfirmed] = useState(false);

  const preferences: SyncPreferences = {
    deviceName,
    uploadRecordingPayloads: uploadRecordings,
    cacheLevel,
    sharedConfigEnabled: sharedConfig,
  };

  async function previewHomeHub(): Promise<void> {
    const result = await store.sync.previewHomeHub(preferences);
    if (!result?.serve_preview) {
      toast.error("Couldn't preview Home Hub activation", {
        description: store.sync.actionError ?? undefined,
      });
      return;
    }
    setServeConfirmed(false);
    setConfirmOpen(true);
  }

  async function activateHomeHub(): Promise<void> {
    const result = await store.sync.activateHomeHub(preferences);
    if (!result || result.status.role !== "home_hub") {
      toast.error("Couldn't activate this Home Hub", {
        description: store.sync.actionError ?? undefined,
      });
      return;
    }
    setConfirmOpen(false);
    toast.success("This device is now the Home Hub");
  }

  async function connect(): Promise<void> {
    const result = await store.sync.connectSelectedHub(preferences);
    if (!result || result.status.role !== "connected_device") {
      toast.error("Couldn't connect to the Home Hub", {
        description: store.sync.actionError ?? undefined,
      });
      return;
    }
    toast.success("Connected to the Shared Library");
  }

  return (
    <Observer>
      {() => {
        const sync = store.sync;
        const busy = sync.operation !== null;
        const hubs = sync.discoveredHubs;

        return (
          <div className="space-y-4 border-t pt-4">
            <div className="grid gap-3 sm:grid-cols-[minmax(0,1fr)_minmax(0,1.2fr)]">
              <div className="space-y-1.5">
                <Label htmlFor="sync-device-name">Device name</Label>
                <Input
                  id="sync-device-name"
                  value={deviceName}
                  onChange={(event) => setDeviceName(event.target.value)}
                  placeholder="For example, Studio Mac"
                  disabled={busy}
                  autoComplete="off"
                />
                <p className="text-[11px] text-muted-foreground">Shown to your other Audetic devices.</p>
              </div>

              <details className="group rounded-md border bg-muted/10 p-3">
                <summary className="cursor-pointer text-sm font-medium">Connected Device options</summary>
                <div className="mt-3 space-y-3 border-t pt-3">
                  <SwitchRow
                    id="sync-recording-payloads"
                    label="Upload recording payloads"
                    description="Include available audio, not just text and metadata."
                    checked={uploadRecordings}
                    onCheckedChange={setUploadRecordings}
                    disabled={busy}
                  />
                  <div className="space-y-1.5">
                    <Label htmlFor="sync-cache-level" className="text-xs">Offline library cache</Label>
                    <select
                      id="sync-cache-level"
                      className="h-9 w-full rounded-md border border-input bg-background px-2.5 text-xs focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                      value={cacheLevel}
                      onChange={(event) => setCacheLevel(event.target.value as CacheLevel)}
                      disabled={busy}
                    >
                      <option value="live_only">Live only</option>
                      <option value="text_for_offline_use">Text for offline use</option>
                      <option value="text_and_available_audio">Text + available audio</option>
                    </select>
                  </div>
                  <SwitchRow
                    id="sync-shared-config"
                    label="Use Shared Configuration"
                    description="Apply behavior and appearance owned by the Home Hub."
                    checked={sharedConfig}
                    onCheckedChange={setSharedConfig}
                    disabled={busy}
                  />
                </div>
              </details>
            </div>

            <div className="grid gap-3 lg:grid-cols-2">
              <div className="flex min-w-0 flex-col rounded-lg border bg-muted/10 p-4">
                <div className="flex items-start gap-3">
                  <div className="rounded-md border bg-background p-2"><Home className="h-4 w-4" /></div>
                  <div>
                    <h4 className="text-sm font-semibold">Make this the Home Hub</h4>
                    <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
                      Own the Shared Library here. Audetic will preview the exact Tailscale Serve change before anything is applied.
                    </p>
                  </div>
                </div>
                <Button className="mt-4 w-full sm:w-fit sm:self-end" size="sm" onClick={() => void previewHomeHub()} disabled={busy}>
                  {sync.operation === "previewing_home_hub" ? <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" /> : <Server className="mr-1.5 h-3.5 w-3.5" />}
                  Preview activation
                </Button>
              </div>

              <div className="flex min-w-0 flex-col rounded-lg border bg-muted/10 p-4">
                <div className="flex items-start gap-3">
                  <div className="rounded-md border bg-background p-2"><Laptop className="h-4 w-4" /></div>
                  <div>
                    <h4 className="text-sm font-semibold">Connect to a Home Hub</h4>
                    <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
                      Search this Tailscale account for compatible Audetic Home Hubs, then choose where this device contributes.
                    </p>
                  </div>
                </div>
                <Button className="mt-4 w-full sm:w-fit sm:self-end" variant="outline" size="sm" onClick={() => void sync.discover()} disabled={busy}>
                  {sync.operation === "discovering" ? <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" /> : <Search className="mr-1.5 h-3.5 w-3.5" />}
                  {sync.discoveryAttempted ? "Search again" : "Find Home Hubs"}
                </Button>
              </div>
            </div>

            {sync.discoveryAttempted && sync.operation !== "discovering" ? (
              <DiscoveryResults
                hubs={hubs.map((hub) => ({
                  connection: { ...hub.connection },
                  device_name: hub.device_name,
                  protocol_version: hub.protocol_version,
                }))}
                failures={sync.discoveryFailures.map((failure) => ({ ...failure }))}
                selectedHubId={sync.selectedHubId}
                onSelect={(hubId) => sync.selectHub(hubId)}
                onConnect={() => void connect()}
                connecting={sync.operation === "connecting"}
              />
            ) : null}

            <Dialog open={confirmOpen} onOpenChange={setConfirmOpen}>
              <DialogContent aria-describedby="home-hub-confirmation-description">
                <DialogHeader>
                  <DialogTitle>Confirm Home Hub activation</DialogTitle>
                  <DialogDescription id="home-hub-confirmation-description">
                    This daemon-authored preview is the exact Tailscale Serve change Audetic will apply. It remains private to your tailnet.
                  </DialogDescription>
                </DialogHeader>
                <div className="min-w-0 space-y-3">
                  <div>
                    <div className="mb-1.5 text-xs font-medium text-muted-foreground">Exact Serve preview</div>
                    <pre className="max-h-52 min-w-0 overflow-auto whitespace-pre-wrap break-words rounded-md border bg-muted/30 p-3 font-mono text-xs">{sync.servePreview}</pre>
                  </div>
                  <label className="flex cursor-pointer items-start gap-2 rounded-md border p-3 text-xs">
                    <input
                      type="checkbox"
                      className="mt-0.5 h-4 w-4 accent-current"
                      checked={serveConfirmed}
                      onChange={(event) => setServeConfirmed(event.target.checked)}
                    />
                    <span>I reviewed this exact Serve mapping and want this device to own the Shared Library.</span>
                  </label>
                </div>
                <DialogFooter className="gap-2 sm:gap-0">
                  <Button type="button" variant="outline" onClick={() => setConfirmOpen(false)} disabled={busy}>Cancel</Button>
                  <Button type="button" onClick={() => void activateHomeHub()} disabled={!serveConfirmed || busy}>
                    {sync.operation === "activating_home_hub" ? <Loader2 className="mr-1.5 h-4 w-4 animate-spin" /> : null}
                    Activate Home Hub
                  </Button>
                </DialogFooter>
              </DialogContent>
            </Dialog>
          </div>
        );
      }}
    </Observer>
  );
}

function DiscoveryResults({
  hubs,
  failures,
  selectedHubId,
  onSelect,
  onConnect,
  connecting,
}: {
  hubs: Array<{ connection: { hub_id: string; base_url: string; owner_login: string }; device_name?: string | null; protocol_version: number }>;
  failures: Array<{ candidate: string; reason: string }>;
  selectedHubId: string | null;
  onSelect: (hubId: string) => void;
  onConnect: () => void;
  connecting: boolean;
}) {
  if (hubs.length === 0) {
    return (
      <div className="rounded-lg border border-dashed p-4">
        <div className="flex items-start gap-3">
          <WifiOff className="mt-0.5 h-4 w-4 shrink-0 text-muted-foreground" />
          <div>
            <p className="text-sm font-medium">No compatible Home Hub found</p>
            <p className="mt-1 text-xs text-muted-foreground">
              Activate a Home Hub on another signed-in device. If discovery is unavailable, use the command generated there.
            </p>
          </div>
        </div>
        {failures.length > 0 ? (
          <details className="mt-3 text-xs text-muted-foreground">
            <summary className="cursor-pointer">{failures.length} candidate{failures.length === 1 ? "" : "s"} could not be used</summary>
            <ul className="mt-2 space-y-1.5 border-l pl-3">
              {failures.map((failure) => (
                <li key={`${failure.candidate}:${failure.reason}`}>
                  <span className="break-all font-mono">{failure.candidate}</span>: {failure.reason}
                </li>
              ))}
            </ul>
          </details>
        ) : null}
      </div>
    );
  }

  return (
    <fieldset className="rounded-lg border p-3 sm:p-4">
      <legend className="px-1 text-xs font-medium text-muted-foreground">
        {hubs.length === 1 ? "Home Hub found" : `${hubs.length} Home Hubs found · choose one`}
      </legend>
      <div className="space-y-2">
        {hubs.map((hub) => {
          const selected = hub.connection.hub_id === selectedHubId;
          return (
            <label key={hub.connection.hub_id} className={cn(
              "flex cursor-pointer items-start gap-3 rounded-md border p-3 transition-colors",
              selected ? "border-foreground bg-muted/40" : "bg-background hover:bg-muted/20",
            )}>
              <input
                type="radio"
                name="sync-home-hub"
                className="mt-1 h-4 w-4 accent-current"
                checked={selected}
                onChange={() => onSelect(hub.connection.hub_id)}
              />
              <span className="min-w-0 flex-1">
                <span className="block text-sm font-medium">{hub.device_name ?? "Unnamed Home Hub"}</span>
                <span className="block break-all font-mono text-[11px] text-muted-foreground">{hub.connection.base_url}</span>
                <span className="block text-[11px] text-muted-foreground">{hub.connection.owner_login} · protocol {hub.protocol_version}</span>
              </span>
              {selected ? <CheckCircle2 className="mt-0.5 h-4 w-4 shrink-0" /> : null}
            </label>
          );
        })}
      </div>
      <div className="mt-3 flex justify-end">
        <Button type="button" size="sm" className="w-full sm:w-auto" onClick={onConnect} disabled={!selectedHubId || connecting}>
          {connecting ? <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" /> : <Wifi className="mr-1.5 h-3.5 w-3.5" />}
          Connect this device
        </Button>
      </div>
    </fieldset>
  );
}

function ActiveRoleActions({ role, hubName, setupCommand }: { role: Exclude<SyncRole, "standalone">; hubName: string | null; setupCommand: string | null }) {
  const store = useStore();
  const [demoteOpen, setDemoteOpen] = useState(false);

  async function demote(): Promise<void> {
    const result = await store.sync.demoteToStandalone();
    if (!result || result.status.role !== "standalone") {
      toast.error("Couldn't return to Standalone", {
        description: store.sync.actionError ?? undefined,
      });
      return;
    }
    setDemoteOpen(false);
    toast.success("This device is now Standalone");
  }

  return (
    <Observer>
      {() => {
        const busy = store.sync.operation !== null;
        return (
          <div className="space-y-3 border-t pt-4">
            <div className="flex flex-col gap-3 rounded-lg border bg-muted/10 p-3 sm:flex-row sm:items-center sm:justify-between">
              <div className="min-w-0">
                <div className="flex items-center gap-2 text-sm font-medium">
                  {role === "home_hub" ? <Home className="h-4 w-4" /> : <Laptop className="h-4 w-4" />}
                  {ROLE_COPY[role].label} active
                </div>
                <p className="mt-1 break-all text-xs text-muted-foreground">
                  {role === "home_hub"
                    ? `Shared Library available at ${hubName ?? "this device's tailnet address"}.`
                    : `Connected to ${hubName ?? "the selected Home Hub"}.`}
                </p>
              </div>
              <Button type="button" size="sm" variant="outline" className="shrink-0" onClick={() => setDemoteOpen(true)} disabled={busy}>
                Return to Standalone
              </Button>
            </div>

            {role === "home_hub" && setupCommand ? <GeneratedCommand command={setupCommand} /> : null}

            <Dialog open={demoteOpen} onOpenChange={setDemoteOpen}>
              <DialogContent aria-describedby="sync-demotion-description">
                <DialogHeader>
                  <DialogTitle>Return this device to Standalone?</DialogTitle>
                  <DialogDescription id="sync-demotion-description">
                    {role === "home_hub"
                      ? "Audetic will remove only the exact Tailscale Serve mapping it owns. Connected Devices will lose access until they choose another Home Hub."
                      : "New records will stop contributing to the Home Hub. Local records remain on this device."}
                  </DialogDescription>
                </DialogHeader>
                <DialogFooter className="gap-2 sm:gap-0">
                  <Button type="button" variant="outline" onClick={() => setDemoteOpen(false)} disabled={busy}>Cancel</Button>
                  <Button type="button" variant="destructive" onClick={() => void demote()} disabled={busy}>
                    {store.sync.operation === "demoting" ? <Loader2 className="mr-1.5 h-4 w-4 animate-spin" /> : null}
                    Return to Standalone
                  </Button>
                </DialogFooter>
              </DialogContent>
            </Dialog>
          </div>
        );
      }}
    </Observer>
  );
}

function GeneratedCommand({ command }: { command: string }) {
  const [copied, setCopied] = useState(false);

  async function copy(): Promise<void> {
    try {
      await navigator.clipboard.writeText(command);
      setCopied(true);
      toast.success("Connected Device command copied");
      window.setTimeout(() => setCopied(false), 1500);
    } catch (error) {
      toast.error("Couldn't copy command", { description: error instanceof Error ? error.message : String(error) });
    }
  }

  return (
    <div className="rounded-lg border bg-muted/15 p-3 sm:p-4">
      <div className="flex items-start gap-2">
        <Laptop className="mt-0.5 h-4 w-4 shrink-0" />
        <div>
          <p className="text-sm font-medium">Connect another device</p>
          <p className="mt-1 text-xs text-muted-foreground">If automatic discovery finds nothing, run this daemon-generated command on that device.</p>
        </div>
      </div>
      <div className="mt-3 flex min-w-0 flex-col gap-2 sm:flex-row">
        <code className="min-w-0 flex-1 overflow-x-auto whitespace-nowrap rounded-md border bg-background px-3 py-2 font-mono text-xs">{command}</code>
        <Button type="button" size="sm" variant="outline" onClick={() => void copy()}>
          {copied ? <Check className="mr-1.5 h-3.5 w-3.5" /> : <Copy className="mr-1.5 h-3.5 w-3.5" />}
          {copied ? "Copied" : "Copy"}
        </Button>
      </div>
    </div>
  );
}

function RolePill({ role }: { role: SyncRole }) {
  const Icon = role === "home_hub" ? Home : role === "connected_device" ? Laptop : HardDrive;
  return (
    <span className="inline-flex items-center gap-1 rounded-full border bg-background px-2 py-0.5 text-[10px] font-medium uppercase tracking-wide">
      <Icon className="h-3 w-3" /> {ROLE_COPY[role].label}
    </span>
  );
}

function ReadinessPill({ ready }: { ready: boolean }) {
  return (
    <span className={cn(
      "inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-[10px] font-medium uppercase tracking-wide",
      ready ? "bg-background" : "text-muted-foreground",
    )}>
      {ready ? <CheckCircle2 className="h-3 w-3" /> : <AlertTriangle className="h-3 w-3" />}
      {ready ? "Ready" : "Needs login"}
    </span>
  );
}

function Detail({ label, value, mono = false }: { label: string; value: string; mono?: boolean }) {
  return (
    <div className="min-w-0">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className={cn("mt-0.5 break-all font-medium", mono && "font-mono text-[11px]")}>{value}</dd>
    </div>
  );
}

function Metric({ label, value, icon: Icon }: { label: string; value: string; icon: typeof Wifi }) {
  return (
    <div className="min-w-0 rounded-md bg-card p-2.5">
      <div className="flex items-center gap-1.5 text-[10px] uppercase tracking-wide text-muted-foreground"><Icon className="h-3 w-3" />{label}</div>
      <div className="mt-1 truncate text-xs font-medium" title={value}>{value}</div>
    </div>
  );
}

function SettingSummary({ label, value }: { label: string; value: string }) {
  return (
    <div className="min-w-0 bg-card p-3">
      <div className="text-[10px] uppercase tracking-wide text-muted-foreground">{label}</div>
      <div className="mt-1 truncate text-xs font-medium" title={value}>{value}</div>
    </div>
  );
}

function SwitchRow({ id, label, description, checked, onCheckedChange, disabled }: { id: string; label: string; description: string; checked: boolean; onCheckedChange: (checked: boolean) => void; disabled: boolean }) {
  return (
    <div className="flex items-start justify-between gap-3">
      <div>
        <Label htmlFor={id} className="text-xs">{label}</Label>
        <p className="mt-1 text-[11px] leading-relaxed text-muted-foreground">{description}</p>
      </div>
      <Switch id={id} checked={checked} onCheckedChange={onCheckedChange} disabled={disabled} aria-label={label} />
    </div>
  );
}

function InlineError({ children }: { children: React.ReactNode }) {
  return (
    <div role="alert" className="flex gap-2 rounded-md border border-destructive/50 bg-destructive/10 p-3 text-xs text-destructive">
      <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
      <span className="break-words">{children}</span>
    </div>
  );
}

function reachabilityLabel(role: SyncRole, reachable: boolean): string {
  if (role === "standalone") return "Local only";
  return reachable ? "Reachable" : "Unavailable";
}

function cacheLabel(level: CacheLevel): string {
  if (level === "text_for_offline_use") return "Offline text";
  if (level === "text_and_available_audio") return "Offline text + audio";
  return "Live only";
}

function formatLastContact(value: string | null | undefined): string {
  if (!value) return "Never";
  try {
    return formatDistanceToNow(new Date(value), { addSuffix: true });
  } catch {
    return value;
  }
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = units[0];
  for (let index = 1; value >= 1024 && index < units.length; index += 1) {
    value /= 1024;
    unit = units[index];
  }
  return `${value >= 10 ? value.toFixed(0) : value.toFixed(1)} ${unit}`;
}
