import { Observer } from "mobx-react-lite";
import {
  useFetcher,
  type ActionFunctionArgs,
  type RouteObject,
} from "react-router-dom";
import { Monitor, Moon, Sun } from "lucide-react";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { useStore } from "@/stores/root-store";
import { getRootStore } from "@/stores/singleton";
import type { ThemeMode } from "@/stores/ui-store";
import { cn } from "@/lib/utils";

const APPEARANCE_INTENTS = {
  setTheme: "set-theme",
} as const;

/**
 * /settings/appearance — theme picker.
 *
 * Theme mode is a UiStore-owned observable; the action delegates to
 * `ui.setThemeMode(...)`, which persists via the preload bridge and
 * re-runs the `reaction` that toggles `.dark` on <html>. The route
 * action is the thin bridge.
 */
export const settingsAppearanceRoute: RouteObject = {
  path: "appearance",
  action: async ({ request }: ActionFunctionArgs) => {
    const form = await request.formData();
    const intent = form.get("intent");
    if (intent === APPEARANCE_INTENTS.setTheme) {
      const mode = String(form.get("mode") ?? "system") as ThemeMode;
      getRootStore().ui.setThemeMode(mode);
    }
    return null;
  },
  Component: SettingsAppearance,
};

function SettingsAppearance() {
  return (
    <div className="space-y-6">
      <header>
        <h2 className="text-xl font-semibold">Appearance</h2>
        <p className="text-sm text-muted-foreground">
          Theme preference. Applies immediately and persists across app
          restarts.
        </p>
      </header>

      <ThemeCard />
    </div>
  );
}

interface ThemeOption {
  mode: ThemeMode;
  label: string;
  description: string;
  icon: typeof Sun;
}

const THEME_OPTIONS: ThemeOption[] = [
  {
    mode: "system",
    label: "System",
    description: "Follow OS preference",
    icon: Monitor,
  },
  {
    mode: "light",
    label: "Light",
    description: "Always light",
    icon: Sun,
  },
  {
    mode: "dark",
    label: "Dark",
    description: "Always dark",
    icon: Moon,
  },
];

function ThemeCard() {
  const store = useStore();
  const fetcher = useFetcher();

  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-base">Theme</CardTitle>
        <CardDescription>
          "System" reacts live to OS theme changes.
        </CardDescription>
      </CardHeader>
      <CardContent>
        <Observer>
          {() => {
            const current = store.ui.themeMode;
            return (
              <fetcher.Form method="post" className="grid grid-cols-3 gap-3">
                <input
                  type="hidden"
                  name="intent"
                  value={APPEARANCE_INTENTS.setTheme}
                />
                {THEME_OPTIONS.map((opt) => {
                  const Icon = opt.icon;
                  const selected = current === opt.mode;
                  return (
                    <button
                      key={opt.mode}
                      type="submit"
                      name="mode"
                      value={opt.mode}
                      className={cn(
                        "flex flex-col items-start gap-1.5 rounded-md border p-3 text-left transition-colors",
                        selected
                          ? "border-primary bg-accent text-accent-foreground"
                          : "border-input hover:bg-accent/40",
                      )}
                      aria-pressed={selected}
                    >
                      <Icon className="h-4 w-4" />
                      <div className="text-sm font-medium">{opt.label}</div>
                      <div className="text-xs text-muted-foreground">
                        {opt.description}
                      </div>
                    </button>
                  );
                })}
              </fetcher.Form>
            );
          }}
        </Observer>
      </CardContent>
    </Card>
  );
}
