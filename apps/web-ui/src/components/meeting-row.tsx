import { Observer } from "mobx-react-lite";
import { NavLink } from "react-router-dom";
import {
  CheckCircle2,
  ChevronRight,
  Loader2,
  Radio,
  Trash2,
  TriangleAlert,
  XCircle,
} from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import {
  isDeletableMeetingStatus,
  type MeetingSummary,
} from "@/stores/meeting-store";
import { getRootStore } from "@/stores/singleton";
import { cn } from "@/lib/utils";
import { meetingDisplayTitle } from "@/lib/meeting-title";

export function MeetingRow({ meeting }: { meeting: MeetingSummary }) {
  const handleDelete = async (
    e: React.MouseEvent<HTMLButtonElement>,
  ): Promise<void> => {
    // The row is a NavLink — stop the click from navigating into the detail
    // page we're about to delete out from under.
    e.preventDefault();
    e.stopPropagation();
    const label = meetingDisplayTitle({
      title: meeting.title,
      sourceFilename: meeting.source_filename,
      startedAt: meeting.started_at,
    });
    if (!window.confirm(`Delete "${label}"? This hides it from all views.`)) {
      return;
    }
    const ok = await getRootStore().meetings.deleteMeeting(meeting.id);
    toast[ok ? "success" : "error"](
      ok ? "Meeting deleted" : "Could not delete meeting",
    );
  };

  return (
    <Observer>
      {() => {
        const deletable = isDeletableMeetingStatus(meeting.status);
        const title = meetingDisplayTitle({
          title: meeting.title,
          sourceFilename: meeting.source_filename,
          startedAt: meeting.started_at,
        });
        const usingDateFallback =
          !meeting.title?.trim() && !meeting.source_filename?.trim();
        return (
          <NavLink
            to={`/meetings/${meeting.id}`}
            className={({ isActive }) =>
              cn(
                "group block transition-colors hover:bg-muted/55",
                isActive && "bg-primary/5",
              )
            }
          >
            <div className="flex items-center gap-4 px-4 py-4 sm:px-5">
              <StatusIcon status={meeting.status} />
              <div className="min-w-0 flex-1">
                <div className="truncate text-sm font-semibold tracking-tight">
                  {title}
                </div>
                <div className="mt-1 text-xs text-muted-foreground">
                  {!usingDateFallback &&
                    new Date(meeting.started_at).toLocaleString()}
                  {typeof meeting.duration_seconds === "number"
                    ? `${usingDateFallback ? "" : " · "}${formatDuration(meeting.duration_seconds)}`
                    : ""}
                </div>
              </div>
              <div className="hidden sm:block">
                <StatusPill status={meeting.status} />
              </div>
              {deletable && (
                <Tooltip>
                  <TooltipTrigger asChild>
                    <Button
                      variant="ghost"
                      size="icon"
                      className="text-muted-foreground hover:text-destructive"
                      aria-label="Delete meeting"
                      onClick={handleDelete}
                    >
                      <Trash2 className="h-4 w-4" />
                    </Button>
                  </TooltipTrigger>
                  <TooltipContent>Delete meeting</TooltipContent>
                </Tooltip>
              )}
              <ChevronRight className="h-4 w-4 text-muted-foreground/50 transition-transform group-hover:translate-x-0.5 group-hover:text-foreground" />
            </div>
          </NavLink>
        );
      }}
    </Observer>
  );
}

function StatusIcon({ status }: { status: string }) {
  const s = status.toLowerCase();
  if (s === "completed")
    return <CheckCircle2 className="h-5 w-5 text-primary/70" />;
  if (s === "error") return <TriangleAlert className="h-5 w-5 text-destructive" />;
  if (s === "cancelled")
    return <XCircle className="h-5 w-5 text-muted-foreground" />;
  if (s === "recording" || s === "compressing" || s === "transcribing" || s === "running_hook") {
    return <Loader2 className="h-5 w-5 animate-spin text-blue-400" />;
  }
  return <Radio className="h-5 w-5 text-muted-foreground" />;
}

function StatusPill({ status }: { status: string }) {
  const s = status.toLowerCase();
  const label = s.replace(/_/g, " ");
  const cls = (() => {
    if (s === "completed") return "bg-primary/15 text-primary";
    if (s === "error") return "bg-destructive/15 text-destructive";
    if (s === "cancelled") return "bg-muted text-muted-foreground";
    return "bg-blue-500/15 text-blue-400";
  })();
  return (
    <span className={cn("rounded-full px-2 py-1 text-xs font-medium", cls)}>
      {label}
    </span>
  );
}

function formatDuration(seconds: number): string {
  const mins = Math.floor(seconds / 60);
  const secs = seconds % 60;
  return `${mins}m ${secs.toString().padStart(2, "0")}s`;
}
