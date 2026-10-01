import { useEffect, useState } from "react";
import { Observer } from "mobx-react-lite";
import type { RouteObject } from "react-router-dom";
import {
  Activity,
  Cable,
  Check,
  Clipboard,
  CloudUpload,
  KeyRound,
  Loader2,
  RefreshCcw,
  RotateCcw,
  ShieldCheck,
  Unplug,
} from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";
import { useStore } from "@/stores/root-store";
import { getRootStore } from "@/stores/singleton";
import type { AccessKeyScope } from "@/stores/integrations-store";

export const settingsIntegrationsRoute: RouteObject = {
  path: "integrations",
  loader: async () => {
    await getRootStore().integrations.load();
    return null;
  },
  Component: SettingsIntegrations,
};

function SettingsIntegrations() {
  const store = useStore().integrations;
  const [keyName, setKeyName] = useState("");
  const [scope, setScope] = useState<AccessKeyScope>("index");

  useEffect(() => () => store.clearIssuedKey(), [store]);

  return (
    <Observer>
      {() => (
        <div className="space-y-5">
          <header className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
            <div>
              <div className="mb-1 flex items-center gap-2 text-xs font-medium uppercase tracking-[0.16em] text-muted-foreground">
                <Cable className="h-3.5 w-3.5" /> Ingress control plane
              </div>
              <h2 className="text-xl font-semibold">External audio</h2>
              <p className="mt-1 max-w-2xl text-sm text-muted-foreground">
                Bring recordings into the same Audio Note pipeline without exposing the Audetic app.
              </p>
            </div>
            <Button variant="outline" size="sm" disabled={store.state === "loading" || store.working || Boolean(store.plaud?.running)} onClick={() => void store.load()}>
              <RefreshCcw className={cn("h-3.5 w-3.5", store.state === "loading" && "animate-spin")} />
              Refresh
            </Button>
          </header>

          {store.error ? (
            <div role="alert" className="rounded-md border border-destructive/50 bg-destructive/10 px-3 py-2 text-sm text-destructive">
              {store.error}
            </div>
          ) : null}

          {store.overview ? (
            <section className="grid gap-px overflow-hidden rounded-lg border bg-border sm:grid-cols-2">
              <Endpoint label="Pebble Index" value={store.overview.index_webhook_url} />
              <Endpoint label="Generic multipart" value={store.overview.generic_audio_url} />
            </section>
          ) : null}

          <Card>
            <CardHeader>
              <CardTitle className="flex items-center gap-2"><KeyRound className="h-4 w-4" /> Ingress keys</CardTitle>
              <CardDescription>Keys are scoped to one endpoint. Audetic stores only a hash and shows the secret once.</CardDescription>
            </CardHeader>
            <CardContent className="space-y-4">
              {store.issuedKey ? (
                <div role="status" aria-label="One-time ingress key ready" className="space-y-2 rounded-md border border-amber-500/40 bg-amber-500/5 p-3">
                  <div className="flex items-center justify-between gap-3">
                    <div><p className="text-sm font-medium">Configure this header now</p><p className="text-xs text-muted-foreground">The header value cannot be recovered after this panel closes.</p></div>
                    <Button size="sm" variant="outline" onClick={async () => { try { await navigator.clipboard.writeText(authorizationHeaderValue(store.issuedKey?.secret ?? "")); toast.success("Authorization value copied"); } catch { toast.error("Clipboard access failed; select the value manually"); } }}><Clipboard className="h-3.5 w-3.5" /> Copy value</Button>
                  </div>
                  <div className="grid gap-2 rounded bg-background p-2 text-xs sm:grid-cols-[11rem_1fr]">
                    <span className="font-medium text-muted-foreground">Header name</span><code>Authorization</code>
                    <span className="font-medium text-muted-foreground">Header value</span><code className="overflow-x-auto">{authorizationHeaderValue(store.issuedKey.secret)}</code>
                    {store.issuedKey.scope === "index" ? <><span className="font-medium text-muted-foreground">Index header name</span><code>X-Index-Webhook-Version</code><span className="font-medium text-muted-foreground">Index header value</span><code>1</code></> : null}
                  </div>
                  <Button size="sm" variant="ghost" onClick={() => store.clearIssuedKey()}><Check className="h-3.5 w-3.5" /> I saved it</Button>
                </div>
              ) : null}

              <div className="grid gap-3 sm:grid-cols-[1fr_10rem_auto] sm:items-end">
                <div className="space-y-1.5"><Label htmlFor="integration-key-name">Key name</Label><Input id="integration-key-name" value={keyName} maxLength={80} placeholder="Index ring" onChange={(event) => setKeyName(event.target.value)} /></div>
                <div className="space-y-1.5"><Label htmlFor="integration-key-scope">Scope</Label><select id="integration-key-scope" className="h-9 w-full rounded-md border bg-background px-3 text-sm" value={scope} onChange={(event) => setScope(event.target.value as AccessKeyScope)}><option value="index">Pebble Index</option><option value="generic">Generic</option></select></div>
                <Button disabled={store.working || store.state === "loading" || Boolean(store.issuedKey) || !keyName.trim()} onClick={async () => { if (await store.createKey(keyName.trim(), scope)) { setKeyName(""); toast.success("Ingress key created"); } }}><KeyRound className="h-4 w-4" /> Create key</Button>
              </div>

              <div className="divide-y rounded-md border">
                {store.state !== "loaded" ? <p className="p-4 text-sm text-muted-foreground">Ingress key metadata is unavailable.</p> : store.keys.length === 0 ? <p className="p-4 text-sm text-muted-foreground">No ingress keys yet.</p> : store.keys.map((key) => (
                  <div key={key.id} className="flex flex-col gap-3 p-3 sm:flex-row sm:items-center sm:justify-between">
                    <div className="min-w-0"><div className="flex items-center gap-2"><span className="truncate text-sm font-medium">{key.name}</span><Badge tone={key.revoked_at ? "muted" : "good"}>{key.revoked_at ? "revoked" : key.scope}</Badge></div><p className="mt-1 font-mono text-[11px] text-muted-foreground">{key.id} · last used {formatDate(key.last_used_at)}</p></div>
                    {!key.revoked_at ? <Button variant="outline" size="sm" disabled={store.working || store.state === "loading"} onClick={async () => { if (await store.revokeKey(key.id)) toast.success("Ingress key revoked"); }}><Unplug className="h-3.5 w-3.5" /> Revoke</Button> : null}
                  </div>
                ))}
              </div>
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle className="flex items-center gap-2"><CloudUpload className="h-4 w-4" /> Plaud sync</CardTitle>
              <CardDescription>Uses the official Plaud CLI on this machine. Install it and run <code>plaud login</code> before syncing.</CardDescription>
            </CardHeader>
            <CardContent className="space-y-4">
              {store.plaud ? <>
                <div className="grid gap-3 rounded-md border bg-muted/20 p-3 sm:grid-cols-3"><StatusDatum label="CLI" value={store.plaud.available ? store.plaud.version ?? "installed" : "not installed"} ready={store.plaud.available} /><StatusDatum label="Session" value={store.plaud.authenticated ? "authenticated" : "login required"} ready={store.plaud.authenticated} /><StatusDatum label="Last successful sync" value={formatDate(store.plaud.last_completed_at)} ready={!store.plaud.last_error} /></div>
                <div className="flex flex-col gap-4 sm:flex-row sm:items-end sm:justify-between">
                  <div className="flex items-start gap-3"><Switch id="plaud-enabled" checked={store.plaud.enabled} disabled={store.working || !store.plaud.available || !store.plaud.authenticated} onCheckedChange={async (enabled) => { if (await store.updatePlaud(enabled)) toast.success(enabled ? "Plaud sync enabled" : "Plaud sync disabled"); }} /><div><Label htmlFor="plaud-enabled">Periodic sync</Label><p className="mt-1 text-xs text-muted-foreground">Only recordings created after sync is enabled are imported automatically.</p></div></div>
                  <div className="space-y-1.5"><Label htmlFor="plaud-interval">Interval (minutes)</Label><div className="flex gap-2"><Input id="plaud-interval" className="w-28" type="number" min={5} max={1440} disabled={store.working} value={store.intervalMinutes} onChange={(event) => store.setIntervalMinutes(Number(event.target.value))} /><Button size="sm" variant="outline" disabled={store.working || store.intervalMinutes < 5 || store.intervalMinutes > 1440} onClick={async () => { if (await store.updatePlaud(store.plaud?.enabled ?? false)) toast.success("Plaud interval saved"); }}>Save</Button></div></div>
                </div>
                <div className="flex flex-wrap gap-2"><Button disabled={store.working || store.plaud.running || !store.plaud.authenticated} onClick={async () => { if (await store.syncPlaud()) toast.success("Plaud sync scheduled"); }}>{store.plaud.running ? <Loader2 className="h-4 w-4 animate-spin" /> : <RefreshCcw className="h-4 w-4" />} Sync now</Button><Button variant="outline" disabled={store.working || store.plaud.running || !store.plaud.authenticated} onClick={async () => { if (await store.syncPlaud(true)) toast.success("Plaud backfill scheduled"); }}><RotateCcw className="h-4 w-4" /> Import history</Button></div>
                {store.plaud.last_error ? <p role="alert" className="rounded-md border border-destructive/40 bg-destructive/5 p-3 text-sm text-destructive">{store.plaud.last_error}</p> : null}
              </> : <p className="text-sm text-muted-foreground">Plaud status is unavailable.</p>}
            </CardContent>
          </Card>

          <Card>
            <CardHeader><CardTitle className="flex items-center gap-2"><Activity className="h-4 w-4" /> Recent imports</CardTitle><CardDescription>The latest delivery attempts across webhook and Plaud sources.</CardDescription></CardHeader>
            <CardContent><div className="divide-y rounded-md border">{store.state !== "loaded" ? <p className="p-4 text-sm text-muted-foreground">Import history is unavailable.</p> : store.imports.length === 0 ? <p className="p-4 text-sm text-muted-foreground">No external imports yet.</p> : store.imports.map((item) => <div key={item.id} className="grid gap-2 p-3 text-sm sm:grid-cols-[8rem_1fr_auto] sm:items-center"><div className="flex items-center gap-2"><Badge tone={item.status === "accepted" ? "good" : item.status === "failed" ? "bad" : "muted"}>{item.status}</Badge><span className="text-xs text-muted-foreground">{item.provider}</span></div><div className="min-w-0"><p className="truncate font-mono text-xs">{item.external_id}</p>{item.error ? <p className="mt-1 truncate text-xs text-destructive">{item.error}</p> : null}</div><span className="text-xs text-muted-foreground">{formatDate(item.updated_at)}</span></div>)}</div></CardContent>
          </Card>
        </div>
      )}
    </Observer>
  );
}

function Endpoint({ label, value }: { label: string; value: string }) {
  return <div className="bg-card p-4"><div className="mb-2 flex items-center justify-between gap-3"><span className="text-xs font-medium uppercase tracking-wider text-muted-foreground">{label}</span><ShieldCheck className="h-4 w-4 text-emerald-500" /></div><code className="block overflow-x-auto text-xs">{value}</code></div>;
}

function StatusDatum({ label, value, ready }: { label: string; value: string; ready: boolean }) {
  return <div><p className="text-[11px] font-medium uppercase tracking-wider text-muted-foreground">{label}</p><p className="mt-1 flex items-center gap-1.5 text-sm"><span className={cn("h-1.5 w-1.5 rounded-full", ready ? "bg-emerald-500" : "bg-amber-500")} />{value}</p></div>;
}

function Badge({ children, tone }: { children: React.ReactNode; tone: "good" | "bad" | "muted" }) {
  return <span className={cn("rounded-full border px-2 py-0.5 text-[10px] font-medium uppercase tracking-wider", tone === "good" && "border-emerald-500/30 bg-emerald-500/10 text-emerald-600", tone === "bad" && "border-destructive/30 bg-destructive/10 text-destructive", tone === "muted" && "text-muted-foreground")}>{children}</span>;
}

function formatDate(value: string | null | undefined): string {
  if (!value) return "never";
  const normalized = value.includes("T") ? value : `${value.replace(" ", "T")}Z`;
  const date = new Date(normalized);
  return Number.isNaN(date.valueOf()) ? value : date.toLocaleString();
}

export function authorizationHeaderValue(secret: string): string {
  return `Bearer ${secret}`;
}
