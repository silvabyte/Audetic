import { Observer } from "mobx-react-lite";
import {
  useFetcher,
  type ActionFunctionArgs,
  type RouteObject,
} from "react-router-dom";
import { CheckCircle2, Trash2, XCircle } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { useStore } from "@/stores/root-store";
import { getRootStore } from "@/stores/singleton";

const KEYBIND_INTENTS = {
  install: "install-keybind",
  uninstall: "uninstall-keybind",
} as const;

export const settingsKeybindRoute: RouteObject = {
  path: "keybind",
  action: async ({ request }: ActionFunctionArgs) => {
    const form = await request.formData();
    const intent = form.get("intent");
    const root = getRootStore();
    const isMac = root.config.keybind?.platform === "macos";
    switch (intent) {
      case KEYBIND_INTENTS.install: {
        const key = String(form.get("key") ?? "").trim() || undefined;
        const errBefore = root.config.lastError;
        await root.config.installKeybind(key);
        const errAfter = root.config.lastError;
        if (errAfter && errAfter !== errBefore) {
          toast.error(
            isMac ? "Couldn't set shortcut" : "Couldn't install keybind",
            { description: errAfter },
          );
        } else if (root.config.keybind?.status === "installed") {
          const display = root.config.keybind.display_key;
          toast.success(
            isMac
              ? `Shortcut set${display ? ` (${display})` : ""}`
              : `Keybind installed${display ? ` (${display})` : ""}`,
          );
        }
        return null;
      }
      case KEYBIND_INTENTS.uninstall: {
        const errBefore = root.config.lastError;
        await root.config.uninstallKeybind();
        const errAfter = root.config.lastError;
        if (errAfter && errAfter !== errBefore) {
          toast.error(
            isMac ? "Couldn't disable shortcut" : "Couldn't uninstall keybind",
            { description: errAfter },
          );
        } else {
          toast.success(isMac ? "Shortcut disabled" : "Keybind removed");
        }
        return null;
      }
      default:
        return null;
    }
  },
  Component: SettingsKeybind,
};

function SettingsKeybind() {
  const store = useStore();
  return (
    <Observer>
      {() => {
        const isMac = store.config.keybind?.platform === "macos";
        return (
          <div className="space-y-6">
            <header>
              <h2 className="text-xl font-semibold">Keybind</h2>
              <p className="text-sm text-muted-foreground">
                {isMac ? (
                  <>
                    System-wide shortcut Audetic registers to toggle dictation
                    from any app. Defaults to{" "}
                    <kbd className="rounded border px-1 font-mono text-xs">
                      ⌘R
                    </kbd>
                    .
                  </>
                ) : (
                  <>
                    Hyprland binding that POSTs{" "}
                    <code className="font-mono text-xs">/api/toggle</code>.
                    Defaults to{" "}
                    <kbd className="rounded border px-1 font-mono text-xs">
                      SUPER+R
                    </kbd>
                    .
                  </>
                )}
              </p>
            </header>

            <KeybindStatusCard />
            <KeybindInstallCard />
          </div>
        );
      }}
    </Observer>
  );
}

function KeybindStatusCard() {
  const store = useStore();
  const fetcher = useFetcher();
  const uninstalling = fetcher.state !== "idle";

  return (
    <Observer>
      {() => {
        const kb = store.config.keybind;
        const state = store.config.keybindState;
        const isMac = kb?.platform === "macos";

        if (state === "loading" && !kb) {
          return (
            <Card>
              <CardContent className="p-6 space-y-3">
                <Skeleton className="h-4 w-32" />
                <Skeleton className="h-3 w-64" />
              </CardContent>
            </Card>
          );
        }
        if (!kb) {
          return (
            <Card>
              <CardContent className="p-6 text-sm text-destructive">
                Couldn't load keybind status.
              </CardContent>
            </Card>
          );
        }

        switch (kb.status) {
          case "installed":
            return (
              <Card>
                <CardHeader className="flex-row items-center justify-between space-y-0">
                  <div className="space-y-1">
                    <CardTitle className="text-base flex items-center gap-2">
                      <CheckCircle2 className="h-4 w-4 text-primary" />
                      {isMac ? "Active" : "Installed"}
                    </CardTitle>
                    <CardDescription>
                      <kbd className="rounded border px-1.5 py-0.5 font-mono text-xs">
                        {kb.display_key}
                      </kbd>
                      {isMac ? (
                        " toggles dictation system-wide."
                      ) : (
                        <>
                          {" → "}
                          <code className="font-mono text-xs">
                            {kb.command}
                          </code>
                        </>
                      )}
                    </CardDescription>
                  </div>
                  <fetcher.Form method="post">
                    <input
                      type="hidden"
                      name="intent"
                      value={KEYBIND_INTENTS.uninstall}
                    />
                    <Button
                      type="submit"
                      variant="outline"
                      size="sm"
                      disabled={uninstalling}
                    >
                      <Trash2 className="mr-1 h-3.5 w-3.5" />
                      {uninstalling
                        ? isMac
                          ? "Disabling…"
                          : "Removing…"
                        : isMac
                          ? "Disable"
                          : "Uninstall"}
                    </Button>
                  </fetcher.Form>
                </CardHeader>
                {!isMac && (
                  <CardContent className="text-xs text-muted-foreground">
                    Config: <code className="font-mono">{kb.config_path}</code>
                  </CardContent>
                )}
              </Card>
            );
          // macOS: the global hotkey is turned off.
          case "disabled":
            return (
              <Card>
                <CardHeader>
                  <CardTitle className="text-base flex items-center gap-2">
                    <XCircle className="h-4 w-4 text-muted-foreground" />
                    Disabled
                  </CardTitle>
                  <CardDescription>
                    No global shortcut is registered. Set one below to toggle
                    dictation from any app.
                  </CardDescription>
                </CardHeader>
              </Card>
            );
          case "not_installed":
            return (
              <Card>
                <CardHeader>
                  <CardTitle className="text-base flex items-center gap-2">
                    <XCircle className="h-4 w-4 text-muted-foreground" />
                    Not installed
                  </CardTitle>
                  <CardDescription>
                    {kb.config_path ? (
                      <>
                        Found Hyprland config at{" "}
                        <code className="font-mono text-xs">
                          {kb.config_path}
                        </code>
                        , but no Audetic binding.
                      </>
                    ) : (
                      "No Hyprland config detected."
                    )}
                  </CardDescription>
                </CardHeader>
              </Card>
            );
          case "no_config":
            return (
              <Card>
                <CardContent className="p-6 text-sm">
                  No Hyprland config found. Install via your compositor's
                  standard location.
                </CardContent>
              </Card>
            );
          default:
            return null;
        }
      }}
    </Observer>
  );
}

function KeybindInstallCard() {
  const store = useStore();
  const fetcher = useFetcher();
  const submitting = fetcher.state !== "idle";

  return (
    <Observer>
      {() => {
        const kb = store.config.keybind;
        const isMac = kb?.platform === "macos";
        const isActive = kb?.status === "installed";

        return (
          <Card>
            <CardHeader>
              <CardTitle className="text-base">
                {isMac
                  ? isActive
                    ? "Change the shortcut"
                    : "Set a shortcut"
                  : "Install a binding"}
              </CardTitle>
              <CardDescription>
                {isMac ? (
                  <>
                    Modifiers + a key. e.g.{" "}
                    <code className="font-mono text-xs">CMD+R</code>,{" "}
                    <code className="font-mono text-xs">CMD+SHIFT+R</code>,{" "}
                    <code className="font-mono text-xs">CTRL+ALT+CMD+R</code>.
                  </>
                ) : (
                  <>
                    Optional custom key. Format matches Hyprland's binding
                    syntax — e.g.{" "}
                    <code className="font-mono text-xs">SUPER+R</code>,{" "}
                    <code className="font-mono text-xs">SUPER SHIFT, T</code>.
                  </>
                )}
              </CardDescription>
            </CardHeader>
            <CardContent>
              <fetcher.Form method="post" className="flex gap-2">
                <input
                  type="hidden"
                  name="intent"
                  value={KEYBIND_INTENTS.install}
                />
                <Input
                  name="key"
                  type="text"
                  placeholder={isMac ? "CMD+R (default)" : "SUPER+R (default)"}
                  disabled={submitting}
                  autoComplete="off"
                />
                <Button type="submit" disabled={submitting}>
                  {submitting
                    ? "Saving…"
                    : isMac
                      ? isActive
                        ? "Change"
                        : "Enable"
                      : "Install"}
                </Button>
              </fetcher.Form>
            </CardContent>
          </Card>
        );
      }}
    </Observer>
  );
}
