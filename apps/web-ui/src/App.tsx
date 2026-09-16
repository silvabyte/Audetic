import { NavLink, Outlet } from "react-router-dom";
import { Observer } from "mobx-react-lite";
import { History, Radio, Settings } from "lucide-react";
import { Toaster } from "sonner";
import { CommandBar } from "./components/command-bar";
import { MeetingAutoNav } from "./components/meeting-auto-nav";
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
      <div className="flex h-dvh min-h-0 flex-col bg-background">
        <MeetingAutoNav />
        <CommandBar />
        <div className="flex min-h-0 flex-1 flex-col sm:flex-row">
          <aside className="w-full shrink-0 border-b bg-card sm:w-56 sm:border-b-0 sm:border-r">
            <nav
              className="flex gap-1 overflow-x-auto p-2 sm:block sm:space-y-1 sm:px-3 sm:pt-5"
              aria-label="Application sections"
            >
              {navItems.map((item) => {
                const Icon = item.icon;
                return (
                  <NavLink
                    key={item.to}
                    to={item.to}
                    end={item.end}
                    className={({ isActive }) =>
                      cn(
                        "group flex shrink-0 items-center gap-3 whitespace-nowrap rounded-lg px-3 py-2.5 text-sm font-medium transition-colors",
                        isActive
                          ? "bg-primary/10 text-primary"
                          : "text-muted-foreground hover:bg-muted hover:text-foreground",
                      )
                    }
                  >
                    <Icon className="h-4 w-4 transition-transform group-hover:scale-105" />
                    <span className="sr-only sm:not-sr-only">{item.label}</span>
                  </NavLink>
                );
              })}
            </nav>
          </aside>
          <main className="min-w-0 flex-1 overflow-auto bg-muted/20">
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
