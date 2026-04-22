import { NavLink, Outlet } from "react-router-dom";
import { Observer } from "mobx-react-lite";
import { History, Home, Mic2, Settings } from "lucide-react";
import { Toaster } from "sonner";
import { ActiveMeetingBanner } from "./components/active-meeting-banner";
import { DaemonDownBanner } from "./components/daemon-down";
import { MeetingAutoNav } from "./components/meeting-auto-nav";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "./components/ui/tooltip";
import { useStore } from "./stores/root-store";
import { cn } from "./lib/utils";

const navItems = [
  {
    to: "/",
    label: "Dashboard",
    icon: Home,
    end: true,
    hint: "Live status, toggle recording, last transcription",
  },
  {
    to: "/history",
    label: "History",
    icon: History,
    end: false,
    hint: "Browse past dictations",
  },
  {
    to: "/meetings",
    label: "Meetings",
    icon: Mic2,
    end: false,
    hint: "Long-form recordings (Super+Shift+R)",
  },
  {
    to: "/settings",
    label: "Settings",
    icon: Settings,
    end: false,
    hint: "Provider, keybind, updates, appearance",
  },
];

export function AppShell() {
  const store = useStore();
  return (
    <TooltipProvider delayDuration={300}>
      <div className="flex h-screen flex-col">
        <MeetingAutoNav />
        <DaemonDownBanner />
        <ActiveMeetingBanner />
        <div className="flex flex-1 min-h-0">
          <aside className="w-52 shrink-0 border-r bg-card">
            <div className="p-4">
              <div className="text-sm font-semibold">Audetic</div>
            </div>
            <nav className="px-2 space-y-1">
              {navItems.map((item) => {
                const Icon = item.icon;
                return (
                  <Tooltip key={item.to}>
                    <TooltipTrigger asChild>
                      <NavLink
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
                    </TooltipTrigger>
                    <TooltipContent side="right">{item.hint}</TooltipContent>
                  </Tooltip>
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
