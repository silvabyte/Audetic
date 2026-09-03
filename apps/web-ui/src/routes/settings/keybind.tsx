import { useState } from "react";
import { Observer } from "mobx-react-lite";
import { type RouteObject } from "react-router-dom";
import {
  AlertTriangle,
  CheckCircle2,
  Keyboard,
  Loader2,
  Mic2,
  Radio,
  Trash2,
  XCircle,
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
import { Skeleton } from "@/components/ui/skeleton";
import { useStore } from "@/stores/root-store";
import { getRootStore } from "@/stores/singleton";
import type {
  InstallResult,
  KeybindStatus,
  KeybindTarget,
  UninstallResult,
} from "@/stores/config-store";

export const settingsKeybindRoute: RouteObject = {
  path: "keybind",
  loader: async () => {
    await getRootStore().config.loadKeybind();
    return null;
  },
  Component: SettingsKeybind,
};

type PendingAction =
  | { kind: "install"; preview: InstallResult; key?: string }
  | { kind: "remove"; preview: UninstallResult };

interface SuccessDetail {
  message: string;
  backupPath: string | null;
}

const TARGET_COPY: Record<KeybindTarget, {
  label: string;
  description: string;
  defaultKey: string;
  icon: typeof Mic2;
}> = {
  dictation: {
    label: "Dictation",
    description: "Toggle short voice capture and paste the transcript.",
    defaultKey: "SUPER+R",
    icon: Mic2,
  },
  meeting: {
    label: "Meeting",
    description: "Start or stop long-form microphone and system audio capture.",
    defaultKey: "SUPER+SHIFT+R",
    icon: Radio,
  },
};

function SettingsKeybind() {
  return (
    <div className="space-y-5">
      <header>
        <div className="mb-1 flex items-center gap-2 text-xs font-medium uppercase tracking-[0.16em] text-muted-foreground">
          <Keyboard className="h-3.5 w-3.5" /> Hyprland
        </div>
        <h2 className="text-xl font-semibold">Shortcuts</h2>
        <p className="max-w-2xl text-sm text-muted-foreground">
          Preview the daemon-generated config before Audetic changes anything.
          Each voice target is installed and removed independently.
        </p>
      </header>

      <div className="grid gap-4 xl:grid-cols-2">
        <KeybindTargetCard target="dictation" />
        <KeybindTargetCard target="meeting" />
      </div>
    </div>
  );
}

function KeybindTargetCard({ target }: { target: KeybindTarget }) {
  const store = useStore();
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [dialogOpen, setDialogOpen] = useState(false);
  const [pending, setPending] = useState<PendingAction | null>(null);
  const [success, setSuccess] = useState<SuccessDetail | null>(null);

  async function previewInstall(status: KeybindStatus): Promise<void> {
    setBusy(true);
    setSuccess(null);
    const customKey = key.trim() || undefined;
    const result = await store.config.previewKeybind(target, customKey);
    setBusy(false);
    if (!result) {
      toast.error(`Couldn't preview ${TARGET_COPY[target].label.toLowerCase()} shortcut`, {
        description: store.config.lastError ?? undefined,
      });
      return;
    }
    setPending({ kind: "install", preview: result, key: customKey });
    setDialogOpen(true);
    if (status.status === "installed" && !customKey) {
      setKey(status.display_key);
    }
  }

  async function previewRemove(): Promise<void> {
    setBusy(true);
    setSuccess(null);
    const result = await store.config.uninstallKeybind(target, true);
    setBusy(false);
    if (!result) {
      toast.error(`Couldn't preview removal`, {
        description: store.config.lastError ?? undefined,
      });
      return;
    }
    setPending({ kind: "remove", preview: result });
    setDialogOpen(true);
  }

  async function applyPending(): Promise<void> {
    if (!pending) return;
    setBusy(true);
    if (pending.kind === "install") {
      const result = await store.config.installKeybind(target, pending.key);
      setBusy(false);
      if (!result?.success) {
        toast.error(`Couldn't save ${TARGET_COPY[target].label.toLowerCase()} shortcut`, {
          description: result?.message ?? store.config.lastError ?? undefined,
        });
        return;
      }
      const detail = {
        message: result.message,
        backupPath: result.backup_path ?? null,
      };
      setSuccess(detail);
      toast.success(result.message, {
        description: detail.backupPath ? `Backup: ${detail.backupPath}` : undefined,
      });
      setKey("");
    } else {
      const result = await store.config.uninstallKeybind(target);
      setBusy(false);
      if (!result) {
        toast.error(`Couldn't remove ${TARGET_COPY[target].label.toLowerCase()} shortcut`, {
          description: store.config.lastError ?? undefined,
        });
        return;
      }
      const detail = {
        message: result.removed
          ? `${TARGET_COPY[target].label} shortcut removed`
          : `${TARGET_COPY[target].label} shortcut was already absent`,
        backupPath: result.backup_path ?? null,
      };
      setSuccess(detail);
      toast.success(detail.message, {
        description: detail.backupPath ? `Backup: ${detail.backupPath}` : undefined,
      });
    }
    setDialogOpen(false);
    setPending(null);
    void store.setup.recheck();
  }

  return (
    <Observer>
      {() => {
        const status = store.config.keybind?.[target];
        const initialLoading = store.config.keybindState === "loading" && !status;
        const noConfig = status?.status === "no_config";
        const installed = status?.status === "installed";
        const copy = TARGET_COPY[target];
        const Icon = copy.icon;

        return (
          <>
            <Card className="flex min-w-0 flex-col shadow-none">
              <CardHeader className="space-y-3 p-4 pb-3 sm:p-5 sm:pb-3">
                <div className="flex items-start justify-between gap-3">
                  <div className="flex min-w-0 items-start gap-3">
                    <div className="rounded-md border bg-muted/40 p-2"><Icon className="h-4 w-4" /></div>
                    <div className="min-w-0">
                      <CardTitle className="text-base">{copy.label}</CardTitle>
                      <CardDescription className="mt-1 text-xs">{copy.description}</CardDescription>
                    </div>
                  </div>
                  {initialLoading ? <Skeleton className="h-5 w-20" /> : <StatusPill status={status} />}
                </div>
              </CardHeader>

              <CardContent className="flex flex-1 flex-col gap-4 p-4 pt-0 sm:p-5 sm:pt-0">
                {initialLoading ? (
                  <><Skeleton className="h-16 w-full" /><Skeleton className="h-9 w-full" /></>
                ) : !status ? (
                  <div role="alert" className="rounded-md border border-destructive/50 bg-destructive/10 p-3 text-sm text-destructive">
                    Couldn't load shortcut status.
                  </div>
                ) : (
                  <>
                    <div className="rounded-md border bg-muted/20 p-3 text-xs">
                      {installed ? (
                        <dl className="grid grid-cols-[4.5rem_minmax(0,1fr)] gap-y-1.5">
                          <dt className="text-muted-foreground">Key</dt><dd><kbd className="rounded border bg-background px-1.5 py-0.5 font-mono">{status.display_key}</kbd></dd>
                          <dt className="text-muted-foreground">Config</dt><dd className="break-all font-mono text-[11px]">{status.config_path}</dd>
                          <dt className="text-muted-foreground">Line</dt><dd className="overflow-x-auto whitespace-nowrap font-mono text-[11px]">{status.generated_line}</dd>
                        </dl>
                      ) : noConfig ? (
                        <div className="flex items-start gap-2 text-muted-foreground">
                          <XCircle className="mt-0.5 h-4 w-4 shrink-0" />
                          <span>No writable Hyprland config was found. Create <code className="font-mono">~/.config/hypr/hyprland.conf</code>, then recheck Setup.</span>
                        </div>
                      ) : (
                        <div className="flex items-start gap-2 text-muted-foreground">
                          <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
                          <span>Not installed. Audetic will write only its managed {target} section to <code className="break-all font-mono">{status.config_path}</code>.</span>
                        </div>
                      )}
                    </div>

                    <div className="space-y-1.5">
                      <label htmlFor={`${target}-key`} className="text-xs font-medium">
                        {installed ? "New shortcut" : "Shortcut"}
                      </label>
                      <Input
                        id={`${target}-key`}
                        value={key}
                        onChange={(event) => setKey(event.target.value)}
                        placeholder={installed ? status.display_key : `${copy.defaultKey} (default)`}
                        disabled={busy || noConfig}
                        autoComplete="off"
                        spellCheck={false}
                        aria-describedby={`${target}-key-help`}
                      />
                      <p id={`${target}-key-help`} className="text-[11px] text-muted-foreground">
                        Hyprland syntax, for example <code className="font-mono">SUPER ALT+R</code>.
                        {installed ? " Enter a new key to change this target." : " Leave blank for the default."}
                      </p>
                    </div>

                    {success ? (
                      <div role="status" className="rounded-md border bg-muted/30 p-3 text-xs">
                        <div className="flex items-center gap-1.5 font-medium"><CheckCircle2 className="h-3.5 w-3.5" />{success.message}</div>
                        {success.backupPath ? <p className="mt-1 break-all font-mono text-[11px] text-muted-foreground">Backup: {success.backupPath}</p> : null}
                      </div>
                    ) : null}

                    <div className="mt-auto flex flex-col-reverse gap-2 sm:flex-row sm:items-center sm:justify-between">
                      <Button type="button" variant="ghost" size="sm" onClick={() => void previewRemove()} disabled={busy || noConfig || !installed}>
                        <Trash2 className="mr-1.5 h-3.5 w-3.5" /> Remove
                      </Button>
                      <Button type="button" size="sm" onClick={() => void previewInstall(status)} disabled={busy || noConfig || (installed && key.trim() === "")}>
                        {busy ? <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" /> : null}
                        {installed ? "Preview change" : "Preview install"}
                      </Button>
                    </div>
                  </>
                )}
              </CardContent>
            </Card>

            <ConfirmationDialog
              open={dialogOpen}
              onOpenChange={setDialogOpen}
              pending={pending}
              target={target}
              busy={busy}
              onConfirm={() => void applyPending()}
            />
          </>
        );
      }}
    </Observer>
  );
}

function ConfirmationDialog({
  open,
  onOpenChange,
  pending,
  target,
  busy,
  onConfirm,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  pending: PendingAction | null;
  target: KeybindTarget;
  busy: boolean;
  onConfirm: () => void;
}) {
  const isInstall = pending?.kind === "install";
  const conflicts = isInstall ? pending.preview.conflicts : [];
  const blocked = conflicts.length > 0 || (isInstall && !pending.preview.success);
  const configPath = pending?.preview.config_path;
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent aria-describedby={`${target}-confirmation-description`}>
        <DialogHeader>
          <DialogTitle>
            {blocked ? "Shortcut conflict" : isInstall ? `Confirm ${TARGET_COPY[target].label.toLowerCase()} shortcut` : `Remove ${TARGET_COPY[target].label.toLowerCase()} shortcut?`}
          </DialogTitle>
          <DialogDescription id={`${target}-confirmation-description`}>
            {blocked
              ? "The server found a binding on this exact key. Nothing has been changed."
              : isInstall
                ? "This preview is generated by the daemon. Confirm to write the exact line below."
                : "Only Audetic's managed section for this target will be removed."}
          </DialogDescription>
        </DialogHeader>

        {pending ? (
          <div className="min-w-0 space-y-3 text-sm">
            <div>
              <div className="mb-1 text-xs font-medium text-muted-foreground">Config file</div>
              <code className="block break-all rounded-md border bg-muted/30 p-2 font-mono text-xs">{configPath}</code>
            </div>
            {isInstall ? (
              <div>
                <div className="mb-1 text-xs font-medium text-muted-foreground">Generated line</div>
                <code className="block min-w-0 max-w-full overflow-x-auto whitespace-nowrap rounded-md border bg-muted/30 p-2 font-mono text-xs">{pending.preview.generated_line}</code>
              </div>
            ) : null}
            {conflicts.length > 0 ? (
              <div role="alert" className="space-y-2 rounded-md border border-destructive/50 bg-destructive/10 p-3">
                <div className="flex items-center gap-2 text-sm font-medium text-destructive"><AlertTriangle className="h-4 w-4" />{conflicts.length} conflict{conflicts.length === 1 ? "" : "s"}</div>
                {conflicts.map((conflict) => (
                  <div key={`${conflict.config_path}:${conflict.line}`} className="text-xs">
                    <code className="font-mono">{conflict.display_key}</code> runs <code className="break-all font-mono">{conflict.command}</code>
                    <div className="break-all text-muted-foreground">{conflict.config_path}:{conflict.line}</div>
                  </div>
                ))}
              </div>
            ) : null}
          </div>
        ) : null}

        <DialogFooter className="gap-2 sm:gap-0">
          <Button type="button" variant="outline" onClick={() => onOpenChange(false)} disabled={busy}>{blocked ? "Choose another key" : "Cancel"}</Button>
          {!blocked ? (
            <Button type="button" variant={isInstall ? "default" : "destructive"} onClick={onConfirm} disabled={busy} autoFocus>
              {busy ? <Loader2 className="mr-1.5 h-4 w-4 animate-spin" /> : null}
              {isInstall ? "Apply shortcut" : "Remove shortcut"}
            </Button>
          ) : null}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function StatusPill({ status }: { status: KeybindStatus | undefined }) {
  if (!status) return <span className="rounded border px-2 py-0.5 text-[10px] uppercase tracking-wide text-muted-foreground">Unknown</span>;
  if (status.status === "installed") return <span className="inline-flex items-center gap-1 rounded border px-2 py-0.5 text-[10px] font-medium uppercase tracking-wide"><CheckCircle2 className="h-3 w-3" />Installed</span>;
  return <span className="rounded border px-2 py-0.5 text-[10px] uppercase tracking-wide text-muted-foreground">{status.status === "no_config" ? "No config" : "Not installed"}</span>;
}
