import { useState } from "react";
import { Observer } from "mobx-react-lite";
import { Link, type RouteObject } from "react-router-dom";
import {
  AlertTriangle,
  ArrowRight,
  Check,
  CheckCircle2,
  Clipboard,
  Copy,
  Download,
  Loader2,
  Mic2,
  Package,
  Radio,
  RefreshCcw,
  Terminal,
  Waypoints,
  XCircle,
} from "lucide-react";
import { toast } from "sonner";
import { Button, buttonVariants } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { cn } from "@/lib/utils";
import { useStore } from "@/stores/root-store";
import { getRootStore } from "@/stores/singleton";
import type {
  SetupCapabilityId,
  SetupState,
} from "@/stores/setup-store";
import { SharedLibraryCard } from "./shared-library-card";

const CAPABILITY_ORDER: SetupCapabilityId[] = [
  "omarchy",
  "hyprland_session",
  "hyprland_config",
  "transcription_provider",
  "text_delivery",
  "clipboard_fallback",
  "dictation_keybind",
  "meeting_keybind",
  "ffmpeg",
  "meeting_audio",
];

const CAPABILITY_LABELS: Record<SetupCapabilityId, string> = {
  omarchy: "Omarchy",
  hyprland_session: "Hyprland session",
  hyprland_config: "Hyprland config",
  transcription_provider: "Transcription",
  text_delivery: "Text delivery",
  clipboard_fallback: "Clipboard fallback",
  dictation_keybind: "Dictation shortcut",
  meeting_keybind: "Meeting shortcut",
  ffmpeg: "FFmpeg",
  meeting_audio: "Meeting audio",
};

export const settingsSetupRoute: RouteObject = {
  path: "setup",
  loader: async () => {
    const store = getRootStore();
    await Promise.all([store.setup.recheck(), store.sync.refresh()]);
    return null;
  },
  Component: SettingsSetup,
};

function SettingsSetup() {
  const store = useStore();

  return (
    <Observer>
      {() => {
        const setup = store.setup.assessment;
        const initialLoading = store.setup.loading && !setup;

        return (
          <div className="space-y-5">
            <header className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
              <div>
                <div className="mb-1 flex items-center gap-2 text-xs font-medium uppercase tracking-[0.16em] text-muted-foreground">
                  <Waypoints className="h-3.5 w-3.5" /> Voice path
                </div>
                <h2 className="text-xl font-semibold">Setup Center</h2>
                <p className="max-w-2xl text-sm text-muted-foreground">
                  One operational view of what Audetic can capture, transcribe,
                  and deliver on this machine.
                </p>
              </div>
              <Button
                type="button"
                size="sm"
                variant="outline"
                onClick={() => void store.setup.recheck()}
                disabled={store.setup.loading}
                aria-label="Recheck setup capabilities"
              >
                <RefreshCcw
                  className={cn("mr-1.5 h-3.5 w-3.5", store.setup.loading && "animate-spin")}
                />
                Recheck
              </Button>
            </header>

            {store.setup.error ? (
              <div role="alert" className="flex gap-2 rounded-md border border-destructive/50 bg-destructive/10 p-3 text-sm text-destructive">
                <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
                <span>Setup check failed: {store.setup.error}</span>
              </div>
            ) : null}

            {setup?.restart_required ? (
              <div role="status" className="flex items-center justify-between gap-3 rounded-md border border-primary/50 bg-primary/5 p-3 text-sm">
                <span>The saved provider differs from the provider active in this daemon.</span>
                <Link className={buttonVariants({ variant: "outline", size: "sm" })} to="../provider">
                  Review and restart
                </Link>
              </div>
            ) : null}

            {initialLoading ? (
              <SetupSkeleton />
            ) : setup ? (
              <>
                <SharedLibraryCard />

                <VoicePath
                  capture={phaseState([
                    store.setup.capability("hyprland_session")?.state,
                    store.setup.capability("dictation_keybind")?.state,
                  ])}
                  transcription={store.setup.capability("transcription_provider")?.state ?? "unavailable"}
                  delivery={store.setup.capability("text_delivery")?.state ?? "unavailable"}
                />

                <div className="grid gap-3 sm:grid-cols-2">
                  <WorkflowCard
                    icon={Mic2}
                    label="Dictation"
                    state={setup.workflows.dictation}
                    detail="Mic capture → transcription → focused-app delivery"
                  />
                  <WorkflowCard
                    icon={Radio}
                    label="Meetings"
                    state={setup.workflows.meetings}
                    detail="Mic + system audio → FFmpeg → transcription"
                  />
                </div>

                <NextAction />
                <ArchPackages />

                <section aria-labelledby="capabilities-heading" className="overflow-hidden rounded-lg border bg-card">
                  <div className="flex items-center justify-between border-b px-4 py-3">
                    <div>
                      <h3 id="capabilities-heading" className="text-sm font-semibold">Machine capabilities</h3>
                      <p className="text-xs text-muted-foreground">
                        Required rows affect readiness. Optional rows improve the path.
                      </p>
                    </div>
                    <span className="hidden font-mono text-[11px] text-muted-foreground sm:block">
                      {setup.platform.distribution ?? setup.platform.os} / {setup.platform.architecture}
                    </span>
                  </div>
                  <div className="divide-y">
                    {CAPABILITY_ORDER.map((id) => (
                      <CapabilityRow key={id} id={id} />
                    ))}
                  </div>
                </section>
              </>
            ) : (
              <Card>
                <CardContent className="p-5 text-sm text-muted-foreground">
                  No setup assessment is available. Recheck while the daemon is running.
                </CardContent>
              </Card>
            )}
          </div>
        );
      }}
    </Observer>
  );
}

function VoicePath({
  capture,
  transcription,
  delivery,
}: {
  capture: SetupState;
  transcription: SetupState;
  delivery: SetupState;
}) {
  const stages = [
    { label: "Capture", icon: Mic2, state: capture },
    { label: "Transcribe", icon: Terminal, state: transcription },
    { label: "Deliver", icon: Clipboard, state: delivery },
  ];
  return (
    <section aria-label="Dictation voice path" className="grid grid-cols-[1fr_auto_1fr_auto_1fr] items-center rounded-lg border bg-muted/25 p-2">
      {stages.map((stage, index) => {
        const Icon = stage.icon;
        return (
          <div className="contents" key={stage.label}>
            <div className="flex min-w-0 items-center justify-center gap-2 rounded-md px-2 py-2.5">
              <Icon className="hidden h-4 w-4 shrink-0 text-muted-foreground sm:block" />
              <div className="min-w-0">
                <div className="text-[11px] font-medium sm:text-sm">{stage.label}</div>
                <div className="hidden text-[11px] text-muted-foreground sm:block">{stateLabel(stage.state)}</div>
              </div>
              <StateIcon state={stage.state} />
            </div>
            {index < stages.length - 1 ? <ArrowRight className="h-3.5 w-3.5 text-muted-foreground" /> : null}
          </div>
        );
      })}
    </section>
  );
}

function WorkflowCard({ icon: Icon, label, state, detail }: {
  icon: typeof Mic2;
  label: string;
  state: SetupState;
  detail: string;
}) {
  const ready = state === "ready" || state === "not_applicable";
  return (
    <Card className="shadow-none">
      <CardContent className="flex items-start gap-3 p-4">
        <div className="rounded-md border bg-muted/40 p-2"><Icon className="h-4 w-4" /></div>
        <div className="min-w-0 flex-1">
          <div className="flex items-center justify-between gap-2">
            <h3 className="text-sm font-semibold">{label}</h3>
            <span className={cn("text-xs font-medium", ready ? "text-foreground" : "text-muted-foreground")}>
              {ready ? "Ready" : "Needs action"}
            </span>
          </div>
          <p className="mt-1 text-xs text-muted-foreground">{detail}</p>
        </div>
      </CardContent>
    </Card>
  );
}

function NextAction() {
  const store = useStore();
  return (
    <Observer>
      {() => {
        const capability = store.setup.nextRequiredAction;
        if (!capability) {
          return (
            <div className="flex items-center gap-3 rounded-lg border bg-primary p-4 text-primary-foreground shadow-sm">
              <CheckCircle2 className="h-5 w-5" />
              <div><p className="text-sm font-semibold">Voice path ready</p><p className="text-xs opacity-80">Dictation and meeting requirements are satisfied.</p></div>
            </div>
          );
        }
        const route = capability.id === "transcription_provider"
          ? "/settings"
          : capability.id.endsWith("keybind") || capability.id === "hyprland_config"
            ? "/settings/keybind"
            : null;
        const packageCommand = store.setup.assessment?.arch_package_command;
        return (
          <div className="flex flex-col gap-3 rounded-lg border bg-primary p-4 text-primary-foreground shadow-sm sm:flex-row sm:items-center sm:justify-between">
            <div className="min-w-0">
              <p className="text-xs font-medium uppercase tracking-wide opacity-70">Next required action</p>
              <p className="mt-0.5 text-sm font-semibold">{capability.summary}</p>
              {capability.action ? <p className="mt-1 text-xs opacity-80">{capability.action}</p> : null}
            </div>
            {capability.id === "ffmpeg" ? <InstallFfmpegButton inverted /> : route ? (
              <Link to={route} className={cn(buttonVariants({ variant: "secondary", size: "sm" }), "shrink-0")}>Open settings <ArrowRight className="ml-1.5 h-3.5 w-3.5" /></Link>
            ) : packageCommand && capability.tools.some((tool) => !tool.available) ? (
              <a href="#packages" className={cn(buttonVariants({ variant: "secondary", size: "sm" }), "shrink-0")}>View package command</a>
            ) : (
              <Button type="button" variant="secondary" size="sm" className="shrink-0" onClick={() => void store.setup.recheck()}>
                Recheck <RefreshCcw className="ml-1.5 h-3.5 w-3.5" />
              </Button>
            )}
          </div>
        );
      }}
    </Observer>
  );
}

function ArchPackages() {
  const store = useStore();
  const [copied, setCopied] = useState(false);
  return (
    <Observer>
      {() => {
        const command = store.setup.assessment?.arch_package_command;
        if (!command) return null;
        return (
          <section id="packages" className="rounded-lg border bg-muted/20 p-4" aria-labelledby="packages-heading">
            <div className="mb-2 flex items-center gap-2">
              <Package className="h-4 w-4" />
              <h3 id="packages-heading" className="text-sm font-semibold">Arch packages</h3>
              <span className="rounded border px-1.5 py-0.5 text-[10px] uppercase tracking-wide text-muted-foreground">Manual</span>
            </div>
            <p className="mb-3 text-xs text-muted-foreground">Copy this exact command into a terminal. Audetic will never execute it or request elevation.</p>
            <div className="flex min-w-0 flex-col gap-2 sm:flex-row">
              <code className="min-w-0 flex-1 overflow-x-auto rounded-md border bg-background px-3 py-2 font-mono text-xs">{command}</code>
              <Button type="button" size="sm" variant="outline" onClick={() => void navigator.clipboard.writeText(command).then(() => { setCopied(true); toast.success("Package command copied"); window.setTimeout(() => setCopied(false), 1500); })} aria-label="Copy Arch package command">
                {copied ? <Check className="mr-1.5 h-3.5 w-3.5" /> : <Copy className="mr-1.5 h-3.5 w-3.5" />}{copied ? "Copied" : "Copy"}
              </Button>
            </div>
          </section>
        );
      }}
    </Observer>
  );
}

function CapabilityRow({ id }: { id: SetupCapabilityId }) {
  const store = useStore();
  return (
    <Observer>
      {() => {
        const capability = store.setup.capability(id);
        if (!capability) return null;
        const requirement = capability.required_for_dictation && capability.required_for_meetings
          ? "Required · both"
          : capability.required_for_dictation
            ? "Required · dictation"
            : capability.required_for_meetings
              ? "Required · meetings"
              : "Optional";
        return (
          <div className="grid gap-2 px-4 py-3 sm:grid-cols-[10rem_minmax(0,1fr)_auto] sm:items-start sm:gap-4">
            <div className="flex items-center gap-2">
              <StateIcon state={capability.state} />
              <span className="text-sm font-medium">{CAPABILITY_LABELS[id]}</span>
            </div>
            <div className="min-w-0">
              <p className="text-sm">{capability.summary}</p>
              {capability.detail ? <p className="break-words font-mono text-[11px] text-muted-foreground">{capability.detail}</p> : null}
              {capability.tools.length ? (
                <div className="mt-1.5 flex flex-wrap gap-1.5" aria-label={`${CAPABILITY_LABELS[id]} tools`}>
                  {capability.tools.map((tool) => (
                    <span key={tool.id} title={tool.path ?? `Package: ${tool.arch_package ?? "unknown"}`} className="inline-flex items-center gap-1 rounded border px-1.5 py-0.5 font-mono text-[10px] text-muted-foreground">
                      <span className={cn("h-1.5 w-1.5 rounded-full", tool.available ? "bg-foreground" : "bg-muted-foreground/40")} />{tool.id}
                    </span>
                  ))}
                </div>
              ) : null}
              {id === "ffmpeg" && capability.state !== "ready" ? <div className="mt-2"><InstallFfmpegButton /></div> : null}
              {id === "transcription_provider" && capability.state !== "ready" ? <InlineLink to="/settings" label="Review provider" /> : null}
              {(id === "dictation_keybind" || id === "meeting_keybind") && capability.state !== "ready" ? <InlineLink to="/settings/keybind" label="Manage shortcuts" /> : null}
            </div>
            <span className="w-fit rounded border px-1.5 py-0.5 text-[10px] uppercase tracking-wide text-muted-foreground">{requirement}</span>
          </div>
        );
      }}
    </Observer>
  );
}

function InlineLink({ to, label }: { to: string; label: string }) {
  return <Link to={to} className="mt-1.5 inline-flex items-center text-xs font-medium underline underline-offset-4">{label}<ArrowRight className="ml-1 h-3 w-3" /></Link>;
}

function InstallFfmpegButton({ inverted = false }: { inverted?: boolean }) {
  const store = useStore();
  return (
    <Observer>
      {() => {
        const phase = store.onboarding.installPhase;
        const installing = phase === "starting" || phase === "downloading" || phase === "extracting";
        return (
          <div className="space-y-1.5">
            <Button type="button" size="sm" variant={inverted ? "secondary" : "outline"} disabled={installing} onClick={() => void store.onboarding.installFfmpeg().then(() => store.setup.recheck())}>
              {installing ? <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" /> : <Download className="mr-1.5 h-3.5 w-3.5" />}
              {installing ? `${store.onboarding.installMessage ?? "Installing"}${store.onboarding.installPercent !== null ? ` · ${store.onboarding.installPercent}%` : ""}` : "Install app-local FFmpeg"}
            </Button>
            {store.onboarding.installError ? <p role="alert" className={cn("text-xs", inverted ? "text-primary-foreground" : "text-destructive")}>{store.onboarding.installError}</p> : null}
          </div>
        );
      }}
    </Observer>
  );
}

function StateIcon({ state }: { state: SetupState }) {
  if (state === "ready") return <CheckCircle2 className="h-4 w-4 shrink-0 text-foreground" aria-label="Ready" />;
  if (state === "not_applicable") return <Check className="h-4 w-4 shrink-0 text-muted-foreground" aria-label="Not applicable" />;
  if (state === "unavailable") return <XCircle className="h-4 w-4 shrink-0 text-muted-foreground" aria-label="Unavailable" />;
  return <AlertTriangle className="h-4 w-4 shrink-0 text-muted-foreground" aria-label="Needs action" />;
}

function phaseState(states: Array<SetupState | undefined>): SetupState {
  return states.every((state) => state === "ready" || state === "not_applicable") ? "ready" : "needs_action";
}

function stateLabel(state: SetupState): string {
  return state === "ready" ? "ready" : state === "not_applicable" ? "not needed" : state === "unavailable" ? "unavailable" : "needs action";
}

function SetupSkeleton() {
  return <div className="space-y-3"><Skeleton className="h-16 w-full" /><div className="grid gap-3 sm:grid-cols-2"><Skeleton className="h-24 w-full" /><Skeleton className="h-24 w-full" /></div><Skeleton className="h-52 w-full" /></div>;
}
