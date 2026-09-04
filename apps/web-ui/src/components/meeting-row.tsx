import { Observer } from "mobx-react-lite";
import { NavLink } from "react-router-dom";
import { Radio, CheckCircle2, TriangleAlert, Loader2, XCircle, Trash2 } from "lucide-react";
import { toast } from "sonner";
import { Card, CardContent } from "@/components/ui/card";
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
        const deletable =
          isDeletableMeetingStatus(meeting.status) &&
          !meeting.offline &&
          !meeting.read_only;
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
              cn("block", isActive && "outline outline-primary/40 rounded-lg")
            }
          >
            <Card className="hover:border-primary/40 transition-colors">
              <CardContent className="p-4 flex items-center gap-4">
                <StatusIcon status={meeting.status} />
                <div className="min-w-0 flex-1">
                  <div className="truncate font-medium text-sm">{title}</div>
                  <div className="text-xs text-muted-foreground">
                    {!usingDateFallback &&
                      new Date(meeting.started_at).toLocaleString()}
                    {typeof meeting.duration_seconds === "number"
                      ? `${usingDateFallback ? "" : " · "}${formatDuration(meeting.duration_seconds)}`
                      : ""}
                  </div>
                  <div className="mt-1 flex flex-wrap gap-1 text-[10px] text-muted-foreground">
                    <span className="font-mono">{compactMeetingLabel(meeting.id, meeting.origin_device_id)}</span>
                    <span>· {meeting.source}</span>
                    {meeting.upload_state && <span>· {meeting.upload_state}</span>}
                    {meeting.offline && <span className="text-destructive">· hub offline</span>}
                  </div>
                </div>
                <StatusPill status={meeting.status} />
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
              </CardContent>
            </Card>
          </NavLink>
        );
      }}
    </Observer>
  );
}

function compactMeetingLabel(id: string, originDeviceId: string): string {
  return `${id.slice(0, 8)}@${originDeviceId.slice(0, 4)}`;
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
