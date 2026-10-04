import { Outlet, useMatch } from "react-router-dom";
import { Observer } from "mobx-react-lite";
import { Toaster } from "sonner";
import { CommandBar } from "./components/command-bar";
import { AppSidebar } from "./components/app-sidebar";
import { TooltipProvider } from "./components/ui/tooltip";
import { useStore } from "./stores/root-store";

export function AppShell() {
  const store = useStore();
  const isNoteDetail = useMatch("/audio-notes/:id") !== null;
  return (
    <TooltipProvider delayDuration={300}>
      <div className="flex h-dvh min-h-0 flex-col">
        <CommandBar />
        <div className="flex min-h-0 flex-1 flex-col sm:flex-row">
          <AppSidebar />
          <main className="min-w-0 flex-1 overflow-auto">
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
              offset={isNoteDetail ? { bottom: 112, right: 24 } : undefined}
              mobileOffset={isNoteDetail ? { bottom: 112, right: 16, left: 16 } : undefined}
            />
          )}
        </Observer>
      </div>
    </TooltipProvider>
  );
}
