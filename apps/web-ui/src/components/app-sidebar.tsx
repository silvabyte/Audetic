import { useEffect } from "react";
import { NavLink } from "react-router-dom";
import { Observer } from "mobx-react-lite";
import { AudioLines, PanelLeftClose, PanelLeftOpen, Settings } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { useStore } from "@/stores/root-store";
import { cn } from "@/lib/utils";

const navItems = [
  { to: "/audio-notes", label: "Audio Notes", icon: AudioLines },
  { to: "/settings", label: "Settings", icon: Settings },
];

/** Icon collapse, rail and tooltip patterns from https://ui.shadcn.com/docs/components/radix/sidebar. */
export function AppSidebar() {
  const store = useStore();

  useEffect(() => {
    function handleKeyDown(event: KeyboardEvent): void {
      const target = event.target;
      if (event.repeat || event.altKey || event.shiftKey || !(event.metaKey || event.ctrlKey) || event.key.toLowerCase() !== "b") return;
      if (target instanceof HTMLElement && (target.isContentEditable || target.closest("input, textarea, select"))) return;
      if (!window.matchMedia("(min-width: 640px)").matches) return;
      event.preventDefault();
      store.ui.toggleSidebar();
    }
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [store]);

  return <Observer>{() => {
    const collapsed = store.ui.sidebarCollapsed;
    const toggleLabel = collapsed ? "Expand sidebar" : "Collapse sidebar";
    const ToggleIcon = collapsed ? PanelLeftOpen : PanelLeftClose;
    return <aside
      data-state={collapsed ? "collapsed" : "expanded"}
      data-collapsible="icon"
      className={cn("group/sidebar relative flex w-full shrink-0 flex-col border-b bg-card transition-[width] duration-200 ease-in-out motion-reduce:transition-none sm:border-b-0 sm:border-r", collapsed ? "sm:w-14" : "sm:w-52")}
    >
      <nav id="application-navigation" aria-label="Application sections" className="flex gap-1 overflow-hidden p-2 sm:flex-1 sm:flex-col sm:pt-4">
        {navItems.map(({ to, label, icon: Icon }) => <Tooltip key={to}>
          <TooltipTrigger asChild><NavLink to={to} aria-label={label} className="flex h-10 shrink-0 items-center gap-2 overflow-hidden whitespace-nowrap rounded-md px-3 text-sm text-muted-foreground transition-colors hover:bg-accent/60 hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring aria-[current=page]:bg-accent aria-[current=page]:text-accent-foreground">
            <Icon className="size-4 shrink-0" aria-hidden="true" />
            <span aria-hidden="true" className="hidden transition-opacity duration-150 motion-reduce:transition-none sm:block sm:group-data-[state=collapsed]/sidebar:opacity-0">{label}</span>
          </NavLink></TooltipTrigger>
          <TooltipContent side="right" className={collapsed ? "" : "sm:hidden"}>{label}</TooltipContent>
        </Tooltip>)}
      </nav>
      <div className="hidden overflow-hidden p-2 sm:block">
        <Tooltip><TooltipTrigger asChild><Button variant="ghost" className="h-10 w-full justify-start gap-2 overflow-hidden px-3 text-muted-foreground" aria-label={toggleLabel} aria-expanded={!collapsed} aria-controls="application-navigation" aria-keyshortcuts="Control+b Meta+b" onClick={() => store.ui.toggleSidebar()}>
          <ToggleIcon className="size-4 shrink-0" />
          <span aria-hidden="true" className="text-xs transition-opacity duration-150 group-data-[state=collapsed]/sidebar:opacity-0 motion-reduce:transition-none">Collapse sidebar</span>
        </Button></TooltipTrigger><TooltipContent side="right">{toggleLabel}<span className="ml-2 text-muted-foreground">Ctrl / ⌘ B</span></TooltipContent></Tooltip>
      </div>
      <button type="button" tabIndex={-1} aria-label={toggleLabel} title={toggleLabel} onClick={() => store.ui.toggleSidebar()} className={cn("absolute inset-y-0 -right-1 z-10 hidden w-2 transition-colors after:absolute after:inset-y-0 after:left-1/2 after:w-px hover:after:bg-muted-foreground/40 sm:block", collapsed ? "cursor-e-resize" : "cursor-w-resize")} />
    </aside>;
  }}</Observer>;
}
