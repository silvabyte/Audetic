import { Observer } from "mobx-react-lite";
import { Circle, Loader2, Mic, TriangleAlert } from "lucide-react";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { useStore } from "@/stores/root-store";
import { cn } from "@/lib/utils";
import type { RecordingPhase } from "@/stores/status-store";

const phaseMeta: Record<
  RecordingPhase,
  { label: string; description: string; className: string; icon: typeof Circle }
> = {
  idle: {
    label: "Idle",
    description: "Daemon is ready. Press Super+R to start a dictation.",
    className: "bg-muted text-muted-foreground",
    icon: Circle,
  },
  recording: {
    label: "Recording",
    description: "Microphone active. Press Super+R (or the Stop button) to finish.",
    className: "bg-red-500/15 text-red-400 ring-1 ring-red-500/40",
    icon: Mic,
  },
  processing: {
    label: "Processing",
    description: "Audio captured. Transcribing via the configured provider.",
    className: "bg-blue-500/15 text-blue-400 ring-1 ring-blue-500/40",
    icon: Loader2,
  },
  error: {
    label: "Error",
    description: "Last pipeline run failed. See History or check the daemon logs.",
    className:
      "bg-destructive/15 text-destructive ring-1 ring-destructive/40",
    icon: TriangleAlert,
  },
};

export function StatusPill() {
  const store = useStore();
  return (
    <Observer>
      {() => {
        const meta = phaseMeta[store.status.phase];
        const Icon = meta.icon;
        return (
          <Tooltip>
            <TooltipTrigger asChild>
              <span
                className={cn(
                  "inline-flex items-center gap-2 rounded-full px-3 py-1 text-xs font-medium cursor-help",
                  meta.className,
                )}
              >
                <Icon
                  className={cn(
                    "h-3.5 w-3.5",
                    store.status.phase === "processing" && "animate-spin",
                  )}
                />
                {meta.label}
              </span>
            </TooltipTrigger>
            <TooltipContent side="bottom">{meta.description}</TooltipContent>
          </Tooltip>
        );
      }}
    </Observer>
  );
}
