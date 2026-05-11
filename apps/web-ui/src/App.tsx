import { NavLink, Outlet } from "react-router-dom";
import { Observer } from "mobx-react-lite";
import { History, Radio, Settings } from "lucide-react";
import { Toaster } from "sonner";
import { CommandBar } from "./components/command-bar";
import { MeetingAutoNav } from "./components/meeting-auto-nav";
import { OnboardingOverlay } from "./components/onboarding-overlay";
import { TooltipProvider } from "./components/ui/tooltip";
import { useStore } from "./stores/root-store";
import { cn } from "./lib/utils";

const navItems = [
  { to: "/dictations", label: "Dictations", icon: History, end: false },
  { to: "/meetings", label: "Meetings", icon: Radio, end: false },
  { to: "/settings", label: "Settings", icon: Settings, end: false },
];

export function AppShell() {
  const store = useStore();
  return (
    <TooltipProvider delayDuration={300}>
      <div className="flex h-screen flex-col">
        <MeetingAutoNav />
        <OnboardingOverlay />
        <CommandBar />
        <div className="flex flex-1 min-h-0">
          <aside className="w-52 shrink-0 border-r bg-card">
            <nav className="px-2 pt-4 space-y-1">
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
          <main className="flex-1 overflow-auto">
            <Outlet />
          </main>
        </div>
        {/* Sonner theme tracks effectiveTheme so toasts don't look
            like they were pasted in from a different design system. */}
        <Observer>
          {() => (
            <Toaster
              theme={store.ui.effectiveTheme}
              richColors
              closeButton
              position="bottom-right"
            />
          )}
        </Observer>
      </div>
    </TooltipProvider>
  );
}
