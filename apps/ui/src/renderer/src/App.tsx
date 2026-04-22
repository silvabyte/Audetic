import { NavLink, Outlet } from "react-router-dom";
import { History, Home, Mic2, Settings } from "lucide-react";
import { DaemonDownBanner } from "./components/daemon-down";
import { cn } from "./lib/utils";

const navItems = [
  { to: "/", label: "Dashboard", icon: Home, end: true },
  { to: "/history", label: "History", icon: History, end: false },
  { to: "/meetings", label: "Meetings", icon: Mic2, end: false },
  { to: "/settings", label: "Settings", icon: Settings, end: false },
];

export function AppShell() {
  return (
    <div className="flex h-screen flex-col">
      <DaemonDownBanner />
      <div className="flex flex-1 min-h-0">
        <aside className="w-52 shrink-0 border-r bg-card">
          <div className="p-4">
            <div className="text-sm font-semibold">Audetic</div>
          </div>
          <nav className="px-2 space-y-1">
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
    </div>
  );
}
