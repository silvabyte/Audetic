import { NavLink, Outlet, type RouteObject } from "react-router-dom";
import { FileText, Keyboard, Mic, Palette, RefreshCcw } from "lucide-react";
import { getRootStore } from "@/stores/singleton";
import { cn } from "@/lib/utils";

/** Factory so the caller supplies child routes without tripping the
 * `index: false` narrowing on NonIndexRouteObject. */
export function settingsLayoutRoute(children: RouteObject[]): RouteObject {
  return {
    path: "settings",
    loader: async () => {
      // Kick off all four parallel fetches. ConfigStore tracks per-section
      // load state; individual section pages show their own skeletons.
      void getRootStore().config.loadAll();
      return null;
    },
    Component: SettingsLayout,
    children,
  };
}

interface SettingsNavItem {
  to: string;
  label: string;
  icon: typeof FileText;
  end?: boolean;
}

const navItems: SettingsNavItem[] = [
  { to: "/settings", label: "Provider", icon: Mic, end: true },
  { to: "/settings/keybind", label: "Keybind", icon: Keyboard },
  { to: "/settings/updates", label: "Updates", icon: RefreshCcw },
  { to: "/settings/appearance", label: "Appearance", icon: Palette },
  { to: "/settings/config-file", label: "Config file", icon: FileText },
];

function SettingsLayout() {
  return (
    <div className="mx-auto flex w-full max-w-5xl gap-6 p-8">
      <aside className="w-44 shrink-0">
        <h1 className="mb-3 text-xs font-semibold uppercase tracking-wide text-muted-foreground">
          Settings
        </h1>
        <nav className="space-y-1">
          {navItems.map((item) => {
            const Icon = item.icon;
            return (
              <NavLink
                key={item.to}
                to={item.to}
                end={item.end}
                className={({ isActive }) =>
                  cn(
                    "flex items-center gap-2 rounded-md px-3 py-2 text-sm transition-colors",
                    isActive
                      ? "bg-accent text-accent-foreground"
                      : "text-muted-foreground hover:bg-accent/60 hover:text-foreground",
                  )
                }
              >
                <Icon className="h-4 w-4" />
                {item.label}
              </NavLink>
            );
          })}
        </nav>
      </aside>
      <main className="flex-1 min-w-0">
        <Outlet />
      </main>
    </div>
  );
}
