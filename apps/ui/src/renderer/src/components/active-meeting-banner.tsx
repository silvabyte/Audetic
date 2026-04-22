import { Observer } from "mobx-react-lite";
import { Form, useFetcher } from "react-router-dom";
import { Mic, Volume2, XCircle, StopCircle, Speaker } from "lucide-react";
import { Button } from "@/components/ui/button";
import { useStore } from "@/stores/root-store";
import type { CaptureState, MeetingPhase } from "@/stores/meeting-store";
import { MEETING_INTENTS } from "@/routes/meetings";
import { cn } from "@/lib/utils";

const PHASE_ORDER: MeetingPhase[] = [
  "recording",
  "compressing",
  "transcribing",
  "running_hook",
  "completed",
];

const PHASE_LABEL: Record<MeetingPhase, string> = {
  idle: "Idle",
  recording: "Recording",
  compressing: "Compressing",
  transcribing: "Transcribing",
  running_hook: "Running hook",
  completed: "Completed",
  error: "Error",
  cancelled: "Cancelled",
  unknown: "Unknown",
};

/**
 * Sticky banner shown whenever a meeting is active OR its post-stop
 * pipeline is still working (compressing / transcribing / running hook).
 * Hidden at idle.
 */
export function ActiveMeetingBanner() {
  const store = useStore();
  return (
    <Observer>
      {() => {
        const { meetings } = store;
        const live =
          meetings.active ||
          meetings.phase === "compressing" ||
          meetings.phase === "transcribing" ||
          meetings.phase === "running_hook";
        if (!live) return null;

        return (
          <div className="w-full border-b border-primary/30 bg-primary/5">
            <div className="mx-auto max-w-5xl px-4 py-3 flex items-center gap-4 flex-wrap">
              <div className="flex items-center gap-2">
                <span className="relative flex h-2 w-2">
                  <span
                    className={cn(
                      "absolute inline-flex h-full w-full rounded-full opacity-75",
                      meetings.active && "bg-red-400 animate-ping",
                    )}
                  />
                  <span
                    className={cn(
                      "relative inline-flex h-2 w-2 rounded-full",
                      meetings.active ? "bg-red-500" : "bg-blue-500",
                    )}
                  />
                </span>
                <div className="text-sm">
                  <div className="font-medium">
                    {meetings.title ?? "Untitled meeting"}
                  </div>
                  <div className="text-xs text-muted-foreground">
                    {PHASE_LABEL[meetings.phase]}
                    {typeof meetings.durationSeconds === "number"
                      ? ` · ${formatDuration(meetings.durationSeconds)}`
                      : ""}
                  </div>
                </div>
              </div>

              <PhaseRibbon phase={meetings.phase} />

              <CaptureStatePill capture={meetings.captureState} />

              <div className="ml-auto flex items-center gap-2">
                <MeetingControls />
              </div>
            </div>
          </div>
        );
      }}
    </Observer>
  );
}

function MeetingControls() {
  const store = useStore();
  const stopFetcher = useFetcher();
  const cancelFetcher = useFetcher();
  const stopping = stopFetcher.state !== "idle";
  const cancelling = cancelFetcher.state !== "idle";

  return (
    <Observer>
      {() => {
        const canControl = store.meetings.active;
        return (
          <div className="flex items-center gap-2">
            <cancelFetcher.Form method="post" action="/meetings">
              <input
                type="hidden"
                name="intent"
                value={MEETING_INTENTS.cancel}
              />
              <Button
                type="submit"
                variant="outline"
                size="sm"
                disabled={!canControl || cancelling}
              >
                <XCircle className="mr-1 h-3.5 w-3.5" />
                {cancelling ? "Cancelling…" : "Cancel"}
              </Button>
            </cancelFetcher.Form>
            <stopFetcher.Form method="post" action="/meetings">
              <input type="hidden" name="intent" value={MEETING_INTENTS.stop} />
              <Button
                type="submit"
                variant="destructive"
                size="sm"
                disabled={!canControl || stopping}
              >
                <StopCircle className="mr-1 h-3.5 w-3.5" />
                {stopping ? "Stopping…" : "Stop"}
              </Button>
            </stopFetcher.Form>
          </div>
        );
      }}
    </Observer>
  );
}

function PhaseRibbon({ phase }: { phase: MeetingPhase }) {
  const current = PHASE_ORDER.indexOf(phase);
  return (
    <div className="flex items-center gap-1">
      {PHASE_ORDER.map((p, i) => {
        const done = current !== -1 && i < current;
        const active = current !== -1 && i === current;
        return (
          <span
            key={p}
            title={PHASE_LABEL[p]}
            className={cn(
              "h-1.5 rounded-full transition-all",
              active ? "w-6 bg-primary" : done ? "w-4 bg-primary/70" : "w-4 bg-muted",
            )}
          />
        );
      })}
    </div>
  );
}

function CaptureStatePill({ capture }: { capture: CaptureState | null }) {
  if (!capture) return null;
  let Icon = Mic;
  let label = "Mic";
  switch (capture) {
    case "both":
      Icon = Volume2;
      label = "Mic + System";
      break;
    case "mic_only":
      Icon = Mic;
      label = "Mic only";
      break;
    case "system_only":
      Icon = Speaker;
      label = "System only";
      break;
    default:
      label = capture;
  }
  return (
    <span className="inline-flex items-center gap-1 rounded-full bg-muted px-2 py-1 text-xs text-muted-foreground">
      <Icon className="h-3 w-3" />
      {label}
    </span>
  );
}

function formatDuration(seconds: number): string {
  const mins = Math.floor(seconds / 60);
  const secs = seconds % 60;
  return `${mins.toString().padStart(2, "0")}:${secs.toString().padStart(2, "0")}`;
}
