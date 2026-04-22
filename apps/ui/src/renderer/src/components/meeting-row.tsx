import { NavLink } from "react-router-dom";
import { Mic, CheckCircle2, TriangleAlert, Loader2, XCircle } from "lucide-react";
import { Card, CardContent } from "@/components/ui/card";
import type { MeetingSummary } from "@/stores/meeting-store";
import { cn } from "@/lib/utils";

export function MeetingRow({ meeting }: { meeting: MeetingSummary }) {
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
            <div className="truncate font-medium text-sm">
              {meeting.title ?? <span className="text-muted-foreground">Untitled</span>}
            </div>
            <div className="text-xs text-muted-foreground">
              {new Date(meeting.started_at).toLocaleString()}
              {typeof meeting.duration_seconds === "number"
                ? ` · ${formatDuration(meeting.duration_seconds)}`
                : ""}
            </div>
          </div>
          <StatusPill status={meeting.status} />
        </CardContent>
      </Card>
    </NavLink>
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
  return <Mic className="h-5 w-5 text-muted-foreground" />;
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
