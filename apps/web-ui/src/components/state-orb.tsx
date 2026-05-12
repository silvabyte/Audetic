import { Observer } from "mobx-react-lite";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { useStore } from "@/stores/root-store";
import { cn } from "@/lib/utils";

type OrbState =
  | "offline"
  | "error"
  | "meeting-active"
  | "meeting-pipeline"
  | "dictation-rec"
  | "dictation-proc"
  | "idle";

const orbStyles: Record<
  OrbState,
  { container: string; glyph: string; pulse: boolean; spinRing: boolean }
> = {
  offline: {
    container: "bg-muted/30 grayscale",
    glyph: "text-muted-foreground/50",
    pulse: false,
    spinRing: false,
  },
  error: {
    container: "bg-destructive/15 ring-2 ring-destructive/60",
    glyph: "text-destructive",
    pulse: false,
    spinRing: false,
  },
  "meeting-active": {
    container: "bg-primary/15 ring-2 ring-primary/60",
    glyph: "text-primary",
    pulse: true,
    spinRing: false,
  },
  "meeting-pipeline": {
    container: "bg-blue-500/10 ring-2 ring-blue-500/40",
    glyph: "text-blue-500",
    pulse: false,
    spinRing: true,
  },
  "dictation-rec": {
    container: "bg-red-500/15 ring-2 ring-red-500/60",
    glyph: "text-red-500",
    pulse: true,
    spinRing: false,
  },
  "dictation-proc": {
    container: "bg-blue-500/10 ring-2 ring-blue-500/40",
    glyph: "text-blue-500",
    pulse: false,
    spinRing: true,
  },
  idle: {
    container: "bg-muted/40",
    glyph: "text-foreground/70",
    pulse: false,
    spinRing: false,
  },
};

export function StateOrb() {
  const store = useStore();
  return (
    <Observer>
      {() => {
        const state = computeState(store);
        const styles = orbStyles[state];
        const tip = describe(state, store);
        return (
          <Tooltip>
            <TooltipTrigger asChild>
              <span className="relative inline-flex h-9 w-9 shrink-0 items-center justify-center cursor-help">
                {styles.spinRing && (
                  <span
                    aria-hidden
                    className="absolute inset-[-3px] rounded-full border-2 border-blue-500/30 border-t-blue-500 animate-spin"
                    style={{ animationDuration: "2.4s" }}
                  />
                )}
                <span
                  className={cn(
                    "relative inline-flex h-9 w-9 items-center justify-center rounded-full transition-colors",
                    styles.container,
                    styles.pulse && "animate-pulse",
                  )}
                >
                  <AudeticGlyph
                    className={cn("h-4 w-4 transition-colors", styles.glyph)}
                  />
                </span>
              </span>
            </TooltipTrigger>
            <TooltipContent side="bottom">{tip}</TooltipContent>
          </Tooltip>
        );
      }}
    </Observer>
  );
}

function computeState(store: ReturnType<typeof useStore>): OrbState {
  if (!store.daemonReachable) return "offline";
  if (store.meetings.active) return "meeting-active";
  const mp = store.meetings.phase;
  if (mp === "compressing" || mp === "transcribing" || mp === "running_hook") {
    return "meeting-pipeline";
  }
  const sp = store.status.phase;
  if (sp === "error") return "error";
  if (sp === "recording") return "dictation-rec";
  if (sp === "processing") return "dictation-proc";
  return "idle";
}

function describe(state: OrbState, store: ReturnType<typeof useStore>): string {
  switch (state) {
    case "offline":
      return "Daemon offline. Start audetic on 127.0.0.1:3737.";
    case "error":
      return store.status.lastError ?? "Last pipeline run failed. Check daemon logs.";
    case "meeting-active": {
      const title = store.meetings.title ?? "Untitled meeting";
      const dur = store.meetings.durationSeconds;
      return typeof dur === "number"
        ? `${title} · ${formatDuration(dur)}`
        : title;
    }
    case "meeting-pipeline":
      return `Meeting: ${meetingPhaseLabel(store.meetings.phase).toLowerCase()}…`;
    case "dictation-rec":
      return "Dictation recording. Press Super+R or the stop button to finish.";
    case "dictation-proc":
      return "Transcribing dictation via the configured provider.";
    case "idle":
      return "Daemon ready. Press Super+R to dictate, Super+Shift+R for a meeting.";
  }
}

function meetingPhaseLabel(phase: string): string {
  switch (phase) {
    case "compressing":
      return "Compressing";
    case "transcribing":
      return "Transcribing";
    case "running_hook":
      return "Running hook";
    case "completed":
      return "Completed";
    case "cancelled":
      return "Cancelled";
    default:
      return "Working";
  }
}

function formatDuration(seconds: number): string {
  const mins = Math.floor(seconds / 60);
  const secs = seconds % 60;
  return `${mins.toString().padStart(2, "0")}:${secs.toString().padStart(2, "0")}`;
}

function AudeticGlyph({ className }: { className?: string }) {
  return (
    <svg
      viewBox="0 0 636 770"
      xmlns="http://www.w3.org/2000/svg"
      className={className}
      aria-hidden
    >
      <path
        fill="currentColor"
        d="M395.522 635.606V520.185C430.328 518.077 459.861 504.374 484.119 479.076C509.433 452.724 522.09 421.102 522.09 384.209C522.09 366.29 518.398 349.425 511.015 333.614C504.687 317.803 495.721 304.1 484.119 292.505C473.572 279.856 460.388 269.843 444.567 262.464C428.746 254.031 412.398 249.288 395.522 248.234V134.394C428.219 135.448 459.333 142.827 488.866 156.53C518.398 170.233 543.711 188.679 564.806 211.869C586.955 234.004 604.358 259.829 617.015 289.343C629.672 318.857 636 350.479 636 384.209C636 417.94 629.672 450.089 617.015 480.657C604.358 510.171 586.955 536.523 564.806 559.713C543.711 581.848 518.398 599.767 488.866 613.47C459.333 627.173 428.219 634.552 395.522 635.606ZM374.955 654.579V770C323.274 768.946 274.756 758.405 229.403 738.378C184.05 717.296 143.97 689.363 109.164 654.579C75.4129 619.795 48.5174 579.213 28.4776 532.834C9.49254 486.455 0 436.913 0 384.209C0 331.506 9.49254 282.491 28.4776 237.166C48.5174 190.787 75.4129 150.205 109.164 115.421C143.97 80.6365 184.05 53.2307 229.403 33.2033C274.756 12.1218 323.274 1.05406 374.955 0V113.84C339.095 114.894 305.343 122.799 273.701 137.556C242.06 152.314 214.109 171.814 189.851 196.058C166.647 220.301 148.189 248.761 134.478 281.437C120.766 313.06 113.91 347.317 113.91 384.209C113.91 421.102 120.766 455.886 134.478 488.563C148.189 520.185 166.647 548.118 189.851 572.361C214.109 596.605 242.06 616.105 273.701 630.862C305.343 645.619 339.095 653.525 374.955 654.579Z"
      />
    </svg>
  );
}
